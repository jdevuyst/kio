#![forbid(unsafe_code)]

use bench::host::BenchHost;
use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;
use std::time::Instant;

#[derive(Clone)]
struct Host {
    trace: Option<Rc<RefCell<Vec<i32>>>>,
}

impl BenchHost for Host {
    type bench__I32 = i32;
    fn bench__step(&self, value: i32, label: i32) -> i32 {
        if let Some(trace) = &self.trace {
            trace.borrow_mut().push(label);
        }
        value + label + 1
    }
}

struct Config {
    arm: String,
    mode: String,
    depth: i32,
    repeat: u64,
    seed: u64,
    warmup: u64,
    purpose: String,
}

fn batch(c: &Config, mut invoke: impl FnMut(i32) -> i32) -> (u64, u128, u128) {
    let delta = (c.depth as u64 + 1) * (c.depth as u64 + 2) / 2;
    let expected: u64 = (0..c.repeat)
        .map(|i| (c.seed + i * 17) % 65536 + delta)
        .sum();
    let start = Instant::now();
    for i in 0..c.warmup {
        let value = ((c.seed + i * 17) % 65536) as i32;
        assert_eq!(invoke(black_box(value)) as u64, value as u64 + delta);
    }
    let warmup_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    let mut checksum = 0_u64;
    for i in 0..c.repeat {
        checksum += black_box(invoke(black_box(((c.seed + i * 17) % 65536) as i32))) as u64;
    }
    let invoke_ns = start.elapsed().as_nanos();
    assert_eq!(checksum, expected, "measured checksum mismatch");
    (checksum, warmup_ns, invoke_ns)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(
        args.len(),
        8,
        "usage: driver ARM MODE DEPTH REPEAT SEED WARMUP PURPOSE"
    );
    let c = Config {
        arm: args[1].clone(),
        mode: args[2].clone(),
        depth: args[3].parse().unwrap(),
        repeat: args[4].parse().unwrap(),
        seed: args[5].parse().unwrap(),
        warmup: args[6].parse().unwrap(),
        purpose: args[7].clone(),
    };
    assert!(["B", "T", "C", "S", "P"].contains(&c.arm.as_str()));
    assert!(
        [
            "direct",
            "native",
            "host-loop",
            "tail",
            "pending",
            "callback-tail",
            "callback-pending"
        ]
        .contains(&c.mode.as_str())
    );
    assert!(["time", "witness"].contains(&c.purpose.as_str()));
    assert!((0..=10000).contains(&c.depth) && (1..=1000000).contains(&c.repeat));
    assert!(c.seed < 65536 && c.warmup <= 1000000);
    assert!(!["direct", "native"].contains(&c.mode.as_str()) || c.depth == 0);
    assert!(c.purpose != "witness" || (c.depth <= 8 && c.repeat == 1 && c.warmup == 0));
    let host = Host {
        trace: (c.purpose == "witness").then(|| Rc::new(RefCell::new(Vec::new()))),
    };
    let start = Instant::now();
    let pkg = bench::create_bench(host.clone());
    let package_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    let construct_ns;
    let release_start;
    let result;
    if c.mode == "tail" || c.mode == "pending" {
        let mut retained = vec![pkg.bench.seed()];
        for label in 1..=c.depth {
            let previous = retained.last().unwrap().clone();
            retained.push(if c.mode == "tail" {
                pkg.bench.tail(previous, label)
            } else {
                pkg.bench.pending(previous, label)
            });
        }
        construct_ns = start.elapsed().as_nanos();
        if let Some(trace) = &host.trace {
            assert!(trace.borrow().is_empty());
        }
        result = batch(&c, |value| {
            pkg.bench.invoke(retained.last().unwrap().clone(), value)
        });
        release_start = Instant::now();
        while retained.pop().is_some() {}
    } else if c.mode.starts_with("callback-") {
        let mut retained = vec![pkg.bench.callbackSeed()];
        for label in 1..=c.depth {
            let previous = retained.last().unwrap().clone();
            retained.push(if c.mode == "callback-tail" {
                pkg.bench.callbackTail(previous, label)
            } else {
                pkg.bench.callbackPending(previous, label)
            });
        }
        construct_ns = start.elapsed().as_nanos();
        if let Some(trace) = &host.trace {
            assert!(trace.borrow().is_empty());
        }
        result = batch(&c, |value| retained.last().unwrap().call(value));
        release_start = Instant::now();
        while retained.pop().is_some() {}
    } else if c.mode == "native" {
        let native_host = host.clone();
        let callback = bench::ffi::exp::bench__callNative::arg0::<Host>::new(move |value| {
            native_host.bench__step(value, 0)
        });
        construct_ns = start.elapsed().as_nanos();
        result = batch(&c, |value| pkg.bench.callNative(callback.clone(), value));
        release_start = Instant::now();
        drop(callback);
    } else {
        construct_ns = start.elapsed().as_nanos();
        result = if c.mode == "direct" {
            batch(&c, |value| pkg.bench.direct(value))
        } else {
            batch(&c, |mut value| {
                for label in (0..=c.depth).rev() {
                    value = host.bench__step(value, label);
                }
                value
            })
        };
        release_start = Instant::now();
    }
    drop(pkg);
    let release_roots_ns = release_start.elapsed().as_nanos();
    let trace = host
        .trace
        .as_ref()
        .map(|x| x.borrow().clone())
        .unwrap_or_default();
    if c.purpose == "witness" {
        let wanted: Vec<i32> = if c.mode == "pending" || c.mode == "callback-pending" {
            (0..=c.depth).collect()
        } else {
            (0..=c.depth).rev().collect()
        };
        assert_eq!(trace, wanted, "host effect order mismatch");
    }
    let (checksum, warmup_ns, invoke_ns) = result;
    let total = package_ns + construct_ns + warmup_ns + invoke_ns + release_roots_ns;
    println!(
        "{{\"schema\":\"kio-primary-driver-v1\",\"backend\":\"rust\",\"arm\":\"{}\",\"mode\":\"{}\",\"depth\":{},\"repeat\":{},\"seed\":{},\"warmup\":{},\"purpose\":\"{}\",\"checksum\":{},\"expected\":{},\"trace\":{:?},\"package_ns\":{},\"construct_ns\":{},\"warmup_ns\":{},\"invoke_ns\":{},\"release_roots_ns\":{},\"total_phases_ns\":{},\"release_limit\":\"roots released in reverse construction order; no live-heap claim\"}}",
        c.arm,
        c.mode,
        c.depth,
        c.repeat,
        c.seed,
        c.warmup,
        c.purpose,
        checksum,
        checksum,
        trace,
        package_ns,
        construct_ns,
        warmup_ns,
        invoke_ns,
        release_roots_ns,
        total
    );
}
