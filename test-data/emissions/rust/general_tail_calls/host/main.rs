#![forbid(unsafe_code)]

use rust_general_tail_calls::ffi::exp::{api__Poly as poly, api__link as value};
use rust_general_tail_calls::host::RustGeneralTailCallsHost;
use rust_general_tail_calls::shapes::nominal::api::Poly;
use rust_general_tail_calls::shapes::{KioNative, KioType, KioUnit};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use poly::readPoly_ret_impl as _;

#[derive(Clone)]
struct Host(Arc<AtomicUsize>, Arc<AtomicUsize>);

impl RustGeneralTailCallsHost for Host {
    type api__I32 = i32;

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
    let packages = counts.each_ref().map(|count| {
        rust_general_tail_calls::create_rustGeneralTailCalls(Host(
            count.clone(),
            panic_after.clone(),
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
