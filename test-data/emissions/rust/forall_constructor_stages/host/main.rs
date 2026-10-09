#![forbid(unsafe_code)]

use rust_forall_constructor_stages::ffi::exp::{
    api__functionControl as function, api__ignore as ignore, api__relay as relay,
    api__twice as twice,
};
use rust_forall_constructor_stages::host::RustForallConstructorStagesHost;
use rust_forall_constructor_stages::shapes::{KioFn1, KioNative, KioType, KioUnit};
use std::cell::Cell;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use function::ret_impl as _;
use ignore::arg0_impl as _;
use relay::ret_field0_impl as _;
use relay::ret_field0_type_ret_impl as _;

#[derive(Clone)]
struct Host;

impl RustForallConstructorStagesHost for Host {
    type api__I32 = i32;
}

struct Counter(Rc<Cell<i32>>);

impl Counter {
    fn tick(&self) -> i32 {
        let next = self.0.get() + 1;
        self.0.set(next);
        next
    }
}

impl ignore::arg0_impl<Host> for Counter {
    fn specialize<A: KioType>(&self) {
        self.tick();
    }
}

impl twice::arg0_impl<Host> for Counter {
    fn specialize<A: KioType>(&self) {
        self.tick();
    }
}

impl relay::arg0_field0_impl<Host> for Counter {
    fn specialize<A: KioType>(&self) -> relay::arg0_field0_type_ret<Host, A> {
        relay::arg0_field0_type_ret::new(SecondStage::<A>(Counter(self.0.clone()), PhantomData))
    }
}

struct SecondStage<A: KioType>(Counter, PhantomData<A>);

impl<A: KioType> relay::arg0_field0_type_ret_impl<Host, A> for SecondStage<A> {
    fn specialize<B: KioType>(&self) -> i32 {
        self.0.tick()
    }
}

impl function::arg0_impl<Host> for Counter {
    fn specialize<A: KioType>(&self) -> KioFn1<A, A> {
        let count = Counter(self.0.clone());
        KioFn1::new(move |value| {
            count.tick();
            value
        })
    }
}

struct PanickingStage(Counter);

impl ignore::arg0_impl<Host> for PanickingStage {
    fn specialize<A: KioType>(&self) {
        self.0.tick();
        std::panic::panic_any("stage failure");
    }
}

fn main() {
    let package = rust_forall_constructor_stages::create_rustForallConstructorStages(Host);

    let count = Rc::new(Cell::new(0));
    let value = ignore::arg0::<Host>::new(Counter(count.clone()));
    assert_eq!(count.get(), 0, "constructing a forall ran its body");
    package.api.ignore(value.clone());
    assert_eq!(
        count.get(),
        0,
        "transporting an ignored forall ran its body"
    );
    value.specialize::<KioUnit>();
    assert_eq!(count.get(), 1);
    value.specialize::<KioNative<String>>();
    assert_eq!(
        count.get(),
        2,
        "a second application reused a cached result"
    );
    println!("unit stages: {}", count.get());

    let count = Rc::new(Cell::new(0));
    let value = twice::arg0::<Host>::new(Counter(count.clone()));
    assert_eq!(count.get(), 0);
    package.api.twice(value);
    assert_eq!(
        count.get(),
        2,
        "Kio applications did not each run the stage"
    );
    println!("kio applications: {}", count.get());

    let count = Rc::new(Cell::new(0));
    let value = relay::arg0::<Host> {
        _0: relay::arg0_field0::<Host>::new(Counter(count.clone())),
        _1: (),
    };
    assert_eq!(count.get(), 0, "constructing a nested forall ran its body");
    let returned = package.api.relay(value);
    assert_eq!(count.get(), 0, "relaying a nested forall ran its body");
    let first = returned
        ._0
        .specialize::<KioUnit>()
        .specialize::<KioNative<String>>();
    let second = returned
        ._0
        .specialize::<KioNative<String>>()
        .specialize::<KioUnit>();
    assert_eq!((first, second, count.get()), (1, 2, 2));
    println!("nested result stages: {first} {second}");

    let count = Rc::new(Cell::new(0));
    let value = function::arg0::<Host>::new(Counter(count.clone()));
    assert_eq!(count.get(), 0);
    let returned = package.api.functionControl(value);
    assert_eq!(count.get(), 0);
    assert_eq!(returned.specialize::<KioNative<i32>>().call(17), 17);
    assert_eq!(
        returned
            .specialize::<KioNative<String>>()
            .call("kept".to_owned()),
        "kept"
    );
    assert_eq!(count.get(), 2);
    println!("function stages: {}", count.get());

    let count = Rc::new(Cell::new(0));
    let value = ignore::arg0::<Host>::new(PanickingStage(Counter(count.clone())));
    assert_eq!(count.get(), 0, "constructing a throwing stage ran it");
    package.api.ignore(value.clone());
    assert_eq!(count.get(), 0);
    for expected in 1..=2 {
        let error = catch_unwind(AssertUnwindSafe(|| value.specialize::<KioUnit>()))
            .expect_err("stage did not throw");
        assert_eq!(error.downcast_ref::<&str>(), Some(&"stage failure"));
        assert_eq!(count.get(), expected);
    }
    println!("panicking stages: {}", count.get());
}
