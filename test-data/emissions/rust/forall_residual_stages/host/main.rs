#![forbid(unsafe_code)]

use rust_forall_residual_stages::ffi::exp::{
    api__chain as chain, api__produce as produce, api__relay as relay,
};
use rust_forall_residual_stages::host::RustForallResidualStagesHost;
use rust_forall_residual_stages::shapes::{KioFn1, KioNative, KioType};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use chain::{
    ret_impl as _, ret_type_ret_impl as _, ret_type_ret_type_ret_impl as _,
    ret_type_ret_type_ret_type_ret_impl as _,
};
use produce::{ret_impl as _, ret_type_ret_impl as _};
use relay::{ret_impl as _, ret_type_ret_impl as _};

type Trace = Rc<RefCell<Vec<i32>>>;

#[derive(Clone)]
struct Host(Trace);

impl RustForallResidualStagesHost for Host {
    type api__I32 = i32;

    fn api__mark(&self, value: i32) {
        self.0.borrow_mut().push(value);
    }
}

struct First(Trace);
struct Second<A: KioType>(Trace, PhantomData<A>);

impl relay::arg0_impl<Host> for First {
    fn specialize<A: KioType>(&self) -> relay::arg0_type_ret<Host, A> {
        self.0.borrow_mut().push(1);
        relay::arg0_type_ret::new(Second::<A>(self.0.clone(), PhantomData))
    }
}

impl<A: KioType> relay::arg0_type_ret_impl<Host, A> for Second<A> {
    fn specialize<B: KioType>(&self) -> relay::arg0_type_ret_type_ret<A, B> {
        self.0.borrow_mut().push(2);
        let trace = self.0.clone();
        relay::arg0_type_ret_type_ret::<A, B>::new(move |step| {
            trace.borrow_mut().push(3);
            let trace = trace.clone();
            KioFn1::new(move |value| {
                trace.borrow_mut().push(4);
                step.call(value)
            })
        })
    }
}

fn main() {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let package = rust_forall_residual_stages::create_rustForallResidualStages(Host(trace.clone()));

    let value = package.api.produce();
    assert!(trace.borrow().is_empty());
    let after_first: produce::ret_type_ret<Host, KioNative<i32>> =
        value.specialize::<KioNative<i32>>();
    assert_eq!(*trace.borrow(), [1]);
    let string: produce::ret_type_ret_type_ret<KioNative<i32>, KioNative<String>> =
        after_first.specialize::<KioNative<String>>();
    assert_eq!(*trace.borrow(), [1, 2]);
    let boolean = after_first.clone().specialize::<KioNative<bool>>();
    assert_eq!(*trace.borrow(), [1, 2, 2]);
    let string = string.call(KioFn1::new(|value| format!("n:{value}")));
    assert_eq!(*trace.borrow(), [1, 2, 2, 3]);
    let boolean = boolean.call(KioFn1::new(|value| value > 0));
    assert_eq!(*trace.borrow(), [1, 2, 2, 3, 3]);
    assert_eq!(string.call(41), "n:41");
    assert!(boolean.call(1));
    assert_eq!(*trace.borrow(), [1, 2, 2, 3, 3, 4, 4]);
    println!("kio stages: {:?}", trace.borrow());

    trace.borrow_mut().clear();
    let supplied = relay::arg0::<Host>::new(First(trace.clone()));
    assert!(trace.borrow().is_empty());
    let returned = package.api.relay(supplied);
    assert!(trace.borrow().is_empty());
    let after_first: relay::ret_type_ret<Host, KioNative<i32>> =
        returned.specialize::<KioNative<i32>>();
    assert_eq!(*trace.borrow(), [1]);
    let string = after_first.specialize::<KioNative<String>>();
    assert_eq!(*trace.borrow(), [1, 2]);
    let boolean = after_first.clone().specialize::<KioNative<bool>>();
    assert_eq!(*trace.borrow(), [1, 2, 2]);
    let string = string.call(KioFn1::new(|value| format!("n:{value}")));
    assert_eq!(*trace.borrow(), [1, 2, 2, 3]);
    let boolean = boolean.call(KioFn1::new(|value| value > 0));
    assert_eq!(*trace.borrow(), [1, 2, 2, 3, 3]);
    assert_eq!(string.call(41), "n:41");
    assert!(boolean.call(1));
    assert_eq!(*trace.borrow(), [1, 2, 2, 3, 3, 4, 4]);
    println!("host stages: {:?}", trace.borrow());

    let first = package.api.chain().specialize::<KioNative<i32>>();
    let second = first.specialize::<KioNative<String>>();
    let third = second.specialize::<KioNative<bool>>();
    let final_function = third.specialize::<KioNative<()>>();
    let captured = final_function.call(41);
    assert_eq!(captured.call("text".to_owned()).call(true).call(()), 41);
    println!("four captured types: 41");
}
