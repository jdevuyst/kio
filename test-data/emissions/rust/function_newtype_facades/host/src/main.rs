#![forbid(unsafe_code)]

use ffi_rust_function_newtype_facades::ffi;
use ffi_rust_function_newtype_facades::ffi::exp::main__Polymorphic::{
    makePolymorphic_arg0_impl as MakePolymorphic, readPolymorphic_ret_impl as ProjectPolymorphic,
};
use ffi_rust_function_newtype_facades::ffi::exp::main__RecursiveExistentialFunction::readRecursiveExistentialFunction_continuation_impl as ProjectExistential;
use ffi_rust_function_newtype_facades::ffi::exp::main__RecursivePoly::{
    makeRecursivePoly_arg0_impl as MakeRecursive,
    readRecursivePoly_ret_impl as ProjectRecursive,
};
use ffi_rust_function_newtype_facades::host::FfiRustFunctionNewtypeFacadesHost;
use ffi_rust_function_newtype_facades::shapes::nominal::main::{
    HigherOrderParameter, HigherOrderReturn, Identity as IdentityValue, List, PackedTriple,
    Polymorphic, RecursiveExistentialFunction, RecursivePoly,
};
use ffi_rust_function_newtype_facades::shapes::{
    KioApplied1, KioApply1, KioFn1, KioNative, KioNewtypeConstructor_main__Identity_P0,
    KioNewtypeConstructor_main__List_P0, KioNewtypeMarker_main__RecursiveExistentialFunction,
    KioStoredValue, KioType, KioTypeConstructor1, KioUnit, KioValue, Product,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone)]
struct Host;

impl FfiRustFunctionNewtypeFacadesHost for Host {
    type main__I32 = i32;
}

type IdentityConstructor = KioNewtypeConstructor_main__Identity_P0<Host>;
type ListConstructor = KioNewtypeConstructor_main__List_P0<Host>;

#[derive(Clone)]
struct NativeOption;

impl KioTypeConstructor1 for NativeOption {
    type Apply<A: KioType> = KioNative<Option<A::Facade>>;

    // The abstract carrier stores `Option<KioStoredValue>` so its representation
    // does not depend on `A`. `lift` is the outbound typed-facade-to-token pass;
    // `project` is the inbound inverse. These element-wise passes are boundary
    // conversion, not validation or serialization; each uses one explicit
    // `Option::map`.
    fn lift<A: KioType>(value: Option<A::Facade>) -> KioApply1<Self, A> {
        let payloads = value.map(|value| KioValue::<A>::pack(value).into_stored());
        let stored = KioValue::<KioNative<Option<KioStoredValue>>>::pack(payloads).into_stored();
        KioValue::<KioApplied1<Self, A>>::from_stored(stored).unpack()
    }

    fn project<A: KioType>(value: KioApply1<Self, A>) -> Option<A::Facade> {
        let stored = KioValue::<KioApplied1<Self, A>>::pack(value).into_stored();
        let payloads = KioValue::<KioNative<Option<KioStoredValue>>>::from_stored(stored).unpack();
        payloads.map(|value| KioValue::<A>::from_stored(value).unpack())
    }
}

struct PolymorphicIdentity;

impl MakePolymorphic<Host, IdentityConstructor> for PolymorphicIdentity {
    fn apply<A: KioType, B: KioType>(
        &self,
        args: Product<KioFn1<A, B>, KioApply1<IdentityConstructor, A>>,
    ) -> KioApply1<IdentityConstructor, B> {
        let Product {
            _0: step,
            _1: value,
        } = args;
        let value = <IdentityConstructor as KioTypeConstructor1>::project::<A>(value);
        let value = IdentityValue::<Host, A>::readIdentity(value);
        let value = IdentityValue::<Host, B>::makeIdentity(step.call(value));
        <IdentityConstructor as KioTypeConstructor1>::lift::<B>(value)
    }
}

fn map_list<A: KioType, B: KioType>(step: &KioFn1<A, B>, value: List<Host, A>) -> List<Host, B> {
    match List::<Host, A>::readList(value) {
        ffi::exp::main__List::readList_ret::<Host, A>::Left(()) => {
            List::<Host, B>::makeList(ffi::exp::main__List::makeList_arg0::<Host, B>::Left(()))
        }
        ffi::exp::main__List::readList_ret::<Host, A>::Right(cell) => List::<Host, B>::makeList(
            ffi::exp::main__List::makeList_arg0::<Host, B>::Right(Product {
                _0: step.call(cell._0),
                _1: map_list(step, cell._1),
            }),
        ),
    }
}

struct PolymorphicList;

impl MakePolymorphic<Host, ListConstructor> for PolymorphicList {
    fn apply<A: KioType, B: KioType>(
        &self,
        args: Product<KioFn1<A, B>, KioApply1<ListConstructor, A>>,
    ) -> KioApply1<ListConstructor, B> {
        let Product {
            _0: step,
            _1: value,
        } = args;
        let value = <ListConstructor as KioTypeConstructor1>::project::<A>(value);
        let value = map_list(&step, value);
        <ListConstructor as KioTypeConstructor1>::lift::<B>(value)
    }
}

struct PolymorphicNativeOption;

impl MakePolymorphic<Host, NativeOption> for PolymorphicNativeOption {
    fn apply<A: KioType, B: KioType>(
        &self,
        args: Product<KioFn1<A, B>, KioApply1<NativeOption, A>>,
    ) -> KioApply1<NativeOption, B> {
        let Product {
            _0: step,
            _1: value,
        } = args;
        let value = <NativeOption as KioTypeConstructor1>::project::<A>(value);
        let value = value.map(|value| step.call(value));
        <NativeOption as KioTypeConstructor1>::lift::<B>(value)
    }
}

fn empty_list<A: KioType>() -> List<Host, A> {
    List::<Host, A>::makeList(ffi::exp::main__List::makeList_arg0::<Host, A>::Left(()))
}

fn prepend<A: KioType>(head: A::Facade, tail: List<Host, A>) -> List<Host, A> {
    List::<Host, A>::makeList(ffi::exp::main__List::makeList_arg0::<Host, A>::Right(
        Product { _0: head, _1: tail },
    ))
}

fn list_values(value: List<Host, KioNative<i32>>) -> Vec<i32> {
    match List::<Host, KioNative<i32>>::readList(value) {
        ffi::exp::main__List::readList_ret::<Host, KioNative<i32>>::Left(()) => Vec::new(),
        ffi::exp::main__List::readList_ret::<Host, KioNative<i32>>::Right(cell) => {
            let mut values = vec![cell._0];
            values.extend(list_values(cell._1));
            values
        }
    }
}

type Recursive = RecursivePoly<Host>;

struct RecursiveLoop {
    value: Rc<RefCell<Option<Recursive>>>,
    calls: Rc<Cell<usize>>,
}

impl MakeRecursive<Host> for RecursiveLoop {
    fn apply<A: KioType>(&self, _value: A::Facade) -> Recursive {
        self.calls.set(self.calls.get() + 1);
        self.value
            .borrow()
            .as_ref()
            .cloned()
            .expect("recursive value initialized")
    }
}

type RecursiveExistential = RecursiveExistentialFunction<Host>;

struct Return42;

impl ProjectExistential<Host, KioNative<i32>> for Return42 {
    fn apply<Hidden: KioType>(
        &self,
        _step: KioFn1<Hidden, KioNewtypeMarker_main__RecursiveExistentialFunction<Host>>,
    ) -> i32 {
        42
    }
}

fn main() {
    let packed = PackedTriple::<Host>::makePackedTriple(
        ffi::exp::main__PackedTriple::makePackedTriple_arg0::<Host>::new(|args| {
            args._0 + args._1._0 + args._1._1
        }),
    );
    let packed = PackedTriple::<Host>::readPackedTriple(packed);
    assert_eq!(
        packed.call(Product {
            _0: 10,
            _1: Product { _0: 20, _1: 12 },
        }),
        42
    );

    let higher_parameter = HigherOrderParameter::<Host>::makeHigherOrderParameter(
        ffi::exp::main__HigherOrderParameter::makeHigherOrderParameter_arg0::<Host>::new(
            |step| step.call(41),
        ),
    );
    let higher_parameter =
        HigherOrderParameter::<Host>::readHigherOrderParameter(higher_parameter);
    assert_eq!(higher_parameter.call(KioFn1::new(|value| value + 1)), 42);

    let higher_return = HigherOrderReturn::<Host>::makeHigherOrderReturn(
        ffi::exp::main__HigherOrderReturn::makeHigherOrderReturn_arg0::<Host>::new(|delta| {
            KioFn1::new(move |value| value + delta)
        }),
    );
    let higher_return = HigherOrderReturn::<Host>::readHigherOrderReturn(higher_return);
    assert_eq!(higher_return.call(2).call(40), 42);

    let identity = IdentityValue::<Host, KioNative<i32>>::makeIdentity(41);
    let identity = <IdentityConstructor as KioTypeConstructor1>::lift::<KioNative<i32>>(identity);
    let polymorphic = Polymorphic::<Host, IdentityConstructor>::makePolymorphic(
        ffi::exp::main__Polymorphic::makePolymorphic_arg0::<Host, IdentityConstructor>::new(
            PolymorphicIdentity,
        ),
    );
    let polymorphic = Polymorphic::<Host, IdentityConstructor>::readPolymorphic(polymorphic);
    let output = <_ as ProjectPolymorphic<Host, IdentityConstructor>>::apply::<
        KioNative<i32>,
        KioNative<i32>,
    >(
        &polymorphic,
        Product {
            _0: KioFn1::new(|value| value + 1),
            _1: identity,
        },
    );
    let output = <IdentityConstructor as KioTypeConstructor1>::project::<KioNative<i32>>(output);
    let output = IdentityValue::<Host, KioNative<i32>>::readIdentity(output);
    assert_eq!(output, 42);

    let list = prepend::<KioNative<i32>>(
        40,
        prepend::<KioNative<i32>>(41, empty_list::<KioNative<i32>>()),
    );
    let list = <ListConstructor as KioTypeConstructor1>::lift::<KioNative<i32>>(list);
    let polymorphic = Polymorphic::<Host, ListConstructor>::makePolymorphic(
        ffi::exp::main__Polymorphic::makePolymorphic_arg0::<Host, ListConstructor>::new(
            PolymorphicList,
        ),
    );
    let polymorphic = Polymorphic::<Host, ListConstructor>::readPolymorphic(polymorphic);
    let output =
        <_ as ProjectPolymorphic<Host, ListConstructor>>::apply::<KioNative<i32>, KioNative<i32>>(
            &polymorphic,
            Product {
                _0: KioFn1::new(|value| value + 1),
                _1: list,
            },
        );
    let output = <ListConstructor as KioTypeConstructor1>::project::<KioNative<i32>>(output);
    assert_eq!(list_values(output), [41, 42]);

    let input: KioApply1<NativeOption, KioNative<i32>> =
        <NativeOption as KioTypeConstructor1>::lift::<KioNative<i32>>(Some(41_i32));
    let polymorphic = Polymorphic::<Host, NativeOption>::makePolymorphic(
        ffi::exp::main__Polymorphic::makePolymorphic_arg0::<Host, NativeOption>::new(
            PolymorphicNativeOption,
        ),
    );
    let polymorphic = Polymorphic::<Host, NativeOption>::readPolymorphic(polymorphic);
    let output: KioApply1<NativeOption, KioNative<String>> =
        <_ as ProjectPolymorphic<Host, NativeOption>>::apply::<KioNative<i32>, KioNative<String>>(
            &polymorphic,
            Product {
                _0: KioFn1::new(|value: i32| (value + 1).to_string()),
                _1: input,
            },
        );
    let output = <NativeOption as KioTypeConstructor1>::project::<KioNative<String>>(output);
    assert_eq!(output, Some("42".to_owned()));

    let value = KioValue::<KioNative<i32>>::pack(42_i32).into_stored();
    assert_eq!(
        KioValue::<KioNative<i32>>::from_stored(value.clone()).unpack(),
        42_i32,
    );
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        KioValue::<KioNative<String>>::from_stored(value).unpack()
    }))
    .is_err());

    let recursive_slot = Rc::new(RefCell::new(None));
    let recursive_calls = Rc::new(Cell::new(0));
    let recursive =
        Recursive::makeRecursivePoly(ffi::exp::main__RecursivePoly::makeRecursivePoly_arg0::<
            Host,
        >::new(RecursiveLoop {
            value: recursive_slot.clone(),
            calls: recursive_calls.clone(),
        }));
    recursive_slot.borrow_mut().replace(recursive.clone());
    let recursive = Recursive::readRecursivePoly(recursive);
    let recursive = <_ as ProjectRecursive<Host>>::apply::<KioNative<i32>>(&recursive, 42_i32);
    assert_eq!(recursive_calls.get(), 1);
    let recursive = Recursive::readRecursivePoly(recursive);
    let _recursive = <_ as ProjectRecursive<Host>>::apply::<KioUnit>(&recursive, ());
    assert_eq!(recursive_calls.get(), 2);

    let existential_slot = Rc::new(RefCell::new(None));
    let existential_calls = Rc::new(Cell::new(0));
    let slot = existential_slot.clone();
    let calls = existential_calls.clone();
    let recursive_existential =
        RecursiveExistential::makeRecursiveExistentialFunction(KioFn1::<
            KioNative<i32>,
            KioNewtypeMarker_main__RecursiveExistentialFunction<Host>,
        >::new(
            move |_value: i32| {
                calls.set(calls.get() + 1);
                slot.borrow()
                    .as_ref()
                    .cloned()
                    .expect("recursive existential value initialized")
            },
        ));
    existential_slot
        .borrow_mut()
        .replace(recursive_existential.clone());
    let output = RecursiveExistential::readRecursiveExistentialFunction::<KioNative<i32>>(
        recursive_existential,
        ffi::exp::main__RecursiveExistentialFunction::readRecursiveExistentialFunction_continuation::<
            Host,
            KioNative<i32>,
        >::new(Return42),
    );
    assert_eq!(output, 42);
    assert_eq!(existential_calls.get(), 0);
}
