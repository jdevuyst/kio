#![forbid(unsafe_code)]

use rust_general_tail_calls::ffi::exp::{api__Poly as poly, api__link as value};
use rust_general_tail_calls::host::RustGeneralTailCallsHost;
use rust_general_tail_calls::shapes::nominal::api::Poly;
use rust_general_tail_calls::shapes::{KioNative, KioType, KioUnit};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use poly::readPoly_ret_impl as _;

#[derive(Clone)]
struct Host(Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<Leases>);

#[derive(Default)]
struct Leases {
    live: AtomicUsize,
    made: AtomicUsize,
    finished: AtomicUsize,
}

impl Leases {
    fn reset(&self) {
        assert_eq!(self.live.load(Ordering::Relaxed), 0);
        self.made.store(0, Ordering::Relaxed);
        self.finished.store(0, Ordering::Relaxed);
    }
}

struct LeaseOwner(Arc<Leases>);

impl Drop for LeaseOwner {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct Lease(Arc<LeaseOwner>);

impl PartialEq for Lease {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl RustGeneralTailCallsHost for Host {
    type api__I32 = i32;
    type api__Lease = Lease;

    fn api__borrow(&self) -> Lease {
        self.2.live.fetch_add(1, Ordering::Relaxed);
        self.2.made.fetch_add(1, Ordering::Relaxed);
        Lease(Arc::new(LeaseOwner(self.2.clone())))
    }

    fn api__finish(&self, _lease: Lease, value: i32) -> i32 {
        let finished = self.2.finished.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            self.2.live.load(Ordering::Relaxed),
            self.2.made.load(Ordering::Relaxed) - finished,
            "each completed frame releases its lease before the next resume"
        );
        value + 1
    }

    fn api__tick(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
        if self
            .1
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(1)
            })
            == Ok(1)
        {
            panic!("expected host panic");
        }
    }
}

struct TypeRoot;

impl poly::makePoly_arg0_impl<Host> for TypeRoot {
    fn specialize<A: KioType>(&self) -> i32 {
        37
    }
}

#[cfg(thread_safe)]
fn require_send_sync<T: Send + Sync>(_: &T) {}

fn exercise() {
    const DEPTH: usize = 20_001;
    let counts = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let panic_after = Arc::new(AtomicUsize::new(0));
    let leases = Arc::new(Leases::default());
    let packages = counts.each_ref().map(|count| {
        rust_general_tail_calls::create_rustGeneralTailCalls(Host(
            count.clone(),
            panic_after.clone(),
            leases.clone(),
        ))
    });
    assert_eq!(
        packages[0]
            .api
            .ordered(11, value::arg0::<Host>::new(|argument| argument), 29),
        29
    );
    let mut retained = vec![value::arg0::<Host>::new(|argument| argument)];
    for index in 0..DEPTH {
        let previous = retained.last().unwrap().clone();
        retained.push(if index % 3 == 0 {
            packages[index % 2].api.wideLink(previous)
        } else {
            packages[index % 2].api.link(previous)
        });
    }
    assert!(
        counts
            .iter()
            .all(|count| count.load(Ordering::Relaxed) == 0)
    );
    let top = retained.last().unwrap();
    #[cfg(thread_safe)]
    require_send_sync(top);
    assert_eq!(top.call(17), 17);
    assert_eq!(top.call(29), 29);
    assert_eq!(counts[0].load(Ordering::Relaxed), DEPTH + 1);
    assert_eq!(counts[1].load(Ordering::Relaxed), DEPTH - 1);
    panic_after.store(257, Ordering::Relaxed);
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| top.call(41)))
        .expect_err("the host panic propagates through pending tail transfers");
    assert_eq!(failure.downcast_ref::<&str>(), Some(&"expected host panic"));
    assert_eq!(counts[0].load(Ordering::Relaxed), DEPTH + 1 + 129);
    assert_eq!(counts[1].load(Ordering::Relaxed), DEPTH - 1 + 128);
    assert_eq!(top.call(53), 53);
    assert_eq!(
        counts[0].load(Ordering::Relaxed),
        DEPTH + 1 + 129 + DEPTH.div_ceil(2)
    );
    assert_eq!(
        counts[1].load(Ordering::Relaxed),
        DEPTH - 1 + 128 + DEPTH / 2
    );
    let value_counts = counts.each_ref().map(|count| count.load(Ordering::Relaxed));
    // Keep each predecessor alive until its successor is released: this tests
    // invocation depth independently of recursive final-owner destruction.
    while retained.pop().is_some() {}

    let mut retained = vec![Poly::<Host>::makePoly(poly::makePoly_arg0::new(TypeRoot))];
    for index in 0..DEPTH {
        retained.push(
            packages[index % 2]
                .api
                .typeLink(retained.last().unwrap().clone()),
        );
    }
    assert_eq!(counts[0].load(Ordering::Relaxed), value_counts[0]);
    assert_eq!(counts[1].load(Ordering::Relaxed), value_counts[1]);
    let top = Poly::<Host>::readPoly(retained.last().unwrap().clone());
    #[cfg(thread_safe)]
    require_send_sync(&top);
    assert_eq!(top.specialize::<KioUnit>(), 37);
    assert_eq!(top.specialize::<KioNative<String>>(), 37);
    assert_eq!(
        counts[0].load(Ordering::Relaxed),
        value_counts[0] + DEPTH + 1
    );
    assert_eq!(
        counts[1].load(Ordering::Relaxed),
        value_counts[1] + DEPTH - 1
    );
    drop(top);
    while retained.pop().is_some() {}

    let mut retained = vec![value::arg0::<Host>::new(|argument| argument)];
    for index in 0..DEPTH {
        retained.push(
            packages[index % 2]
                .api
                .pendingLink(retained.last().unwrap().clone()),
        );
    }
    let top = retained.last().unwrap();
    #[cfg(thread_safe)]
    require_send_sync(top);
    assert_eq!(top.call(41), 41 + DEPTH as i32);
    assert_eq!(leases.made.load(Ordering::Relaxed), DEPTH);
    assert_eq!(leases.finished.load(Ordering::Relaxed), DEPTH);
    assert_eq!(leases.live.load(Ordering::Relaxed), 0);

    leases.reset();
    panic_after.store(257, Ordering::Relaxed);
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| top.call(43)))
        .expect_err("the host panic unwinds queued guest resume frames");
    assert_eq!(failure.downcast_ref::<&str>(), Some(&"expected host panic"));
    assert_eq!(leases.made.load(Ordering::Relaxed), 257);
    assert_eq!(leases.finished.load(Ordering::Relaxed), 0);
    assert_eq!(leases.live.load(Ordering::Relaxed), 0);

    leases.reset();
    assert_eq!(top.call(47), 47 + DEPTH as i32);
    assert_eq!(leases.finished.load(Ordering::Relaxed), DEPTH);
    assert_eq!(leases.live.load(Ordering::Relaxed), 0);
    // Callable environments stay independently anchored; only invocation-local
    // leases measure frame cleanup, including the exceptional path above.
    while retained.pop().is_some() {}
}

fn main() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(exercise)
        .expect("start bounded-stack host thread")
        .join()
        .expect("complete both retained tail chains");
    println!("value and forall chains");
}
