#![allow(unused_variables)]
#![allow(unused_imports)]
#![allow(non_camel_case_types)]

use order_book_matcher::host::OrderBookMatcherHost;



struct __ArrayStorage(::std::rc::Rc<::std::cell::RefCell<Vec<order_book_matcher::shapes::KioStoredValue>>>);

impl Clone for __ArrayStorage {
    fn clone(&self) -> Self { __ArrayStorage(::std::rc::Rc::clone(&self.0)) }
}

impl PartialEq for __ArrayStorage {
    fn eq(&self, other: &Self) -> bool { ::std::rc::Rc::ptr_eq(&self.0, &other.0) }
}

impl __ArrayStorage {
    fn new(v: Vec<order_book_matcher::shapes::KioStoredValue>) -> Self { __ArrayStorage(::std::rc::Rc::new(::std::cell::RefCell::new(v))) }
}




#[derive(Clone)]
struct StubHost {
    domain_sink: ::std::rc::Rc<::std::cell::RefCell<Vec<u8>>>,
}


impl OrderBookMatcherHost for StubHost {
    type elab_testapi__Bool = bool;
    type elab_testapi__I32 = i32;
    type elab_testapi__Int = i32;
    type elab_testapi__String = String;
    type testapi__ArrayStorage = __ArrayStorage;
    type testapi__Bool = bool;
    type testapi__I32 = i32;
    type testapi__String = String;

    fn testapi_arith__addI32(&self, arg0: i32, arg1: i32) -> i32 {
        arg0.wrapping_add(arg1)
    }

    fn testapi_arith__leqI32(&self, arg0: i32, arg1: i32) -> bool {
        arg0 <= arg1
    }

    fn testapi_arith__mulI32(&self, arg0: i32, arg1: i32) -> i32 {
        arg0.wrapping_mul(arg1)
    }

    fn testapi_arith__subI32(&self, arg0: i32, arg1: i32) -> i32 {
        arg0.wrapping_sub(arg1)
    }

    fn testapi_array__arrayGet<t: order_book_matcher::shapes::KioType>(&self, arg0: order_book_matcher::ffi::env::testapi_array__arrayGet::arg0<Self, t>, arg1: i32) -> <t as order_book_matcher::shapes::KioType>::Facade {
        { let __storage = arg0.storage(); let __a = __storage.0.borrow(); if arg1 < 0 || (arg1 as usize) >= __a.len() { panic!("array_get: index {} out of bounds (len {})", arg1, __a.len()); } order_book_matcher::shapes::KioValue::<t>::from_stored(__a[arg1 as usize].clone()).unpack() }
    }

    fn testapi_array__arrayLen<t: order_book_matcher::shapes::KioType>(&self, arg0: order_book_matcher::ffi::env::testapi_array__arrayLen::arg0<Self, t>) -> i32 {
        arg0.storage().0.borrow().len() as i32
    }

    fn testapi_array__arrayMakeEmpty<t: order_book_matcher::shapes::KioType>(&self) -> order_book_matcher::ffi::env::testapi_array__arrayMakeEmpty::ret<Self, t> {
        order_book_matcher::ffi::env::testapi_array__arrayMakeEmpty::ret::<Self, t>::from_storage(__ArrayStorage::new(Vec::new()))
    }

    fn testapi_array__arrayMakeFilled<t: order_book_matcher::shapes::KioType>(&self, arg0: i32, arg1: <t as order_book_matcher::shapes::KioType>::Facade) -> order_book_matcher::ffi::env::testapi_array__arrayMakeFilled::ret<Self, t> {
        if arg0 < 0 { panic!("array_make_filled: negative size {}", arg0); } order_book_matcher::ffi::env::testapi_array__arrayMakeFilled::ret::<Self, t>::from_storage(__ArrayStorage::new((0..arg0).map(|_| order_book_matcher::shapes::KioValue::<t>::pack(arg1.clone()).into_stored()).collect()))
    }

    fn testapi_array__arrayPopBack<t: order_book_matcher::shapes::KioType>(&self, arg0: order_book_matcher::ffi::env::testapi_array__arrayPopBack::arg0<Self, t>) -> order_book_matcher::ffi::env::testapi_array__arrayPopBack::ret<t> {
        { let popped = arg0.storage().0.borrow_mut().pop(); match popped { Some(v) => order_book_matcher::ffi::env::testapi_array__arrayPopBack::ret::<t>::Left(order_book_matcher::shapes::KioValue::<t>::from_stored(v).unpack()), None => order_book_matcher::ffi::env::testapi_array__arrayPopBack::ret::<t>::Right(()), } }
    }

    fn testapi_array__arrayPush<t: order_book_matcher::shapes::KioType>(&self, arg0: order_book_matcher::ffi::env::testapi_array__arrayPush::arg0<Self, t>, arg1: <t as order_book_matcher::shapes::KioType>::Facade) {
        arg0.storage().0.borrow_mut().push(order_book_matcher::shapes::KioValue::<t>::pack(arg1).into_stored());
    }

    fn testapi_array__arraySet<t: order_book_matcher::shapes::KioType>(&self, arg0: order_book_matcher::ffi::env::testapi_array__arraySet::arg0<Self, t>, arg1: i32, arg2: <t as order_book_matcher::shapes::KioType>::Facade) {
        { let __storage = arg0.storage(); let mut __a = __storage.0.borrow_mut(); if arg1 < 0 || (arg1 as usize) >= __a.len() { panic!("array_set: index {} out of bounds (len {})", arg1, __a.len()); } __a[arg1 as usize] = order_book_matcher::shapes::KioValue::<t>::pack(arg2).into_stored(); }
    }

    fn testapi_fmt__boolToString(&self, arg0: bool) -> String {
        arg0.to_string()
    }

    fn testapi_fmt__intToString(&self, arg0: i32) -> String {
        arg0.to_string().into()
    }

    fn testapi_io__print(&self, arg0: String) {
        self.domain_sink.borrow_mut().extend_from_slice(arg0.as_bytes());
    }

    fn testapi_iter__loop<s: order_book_matcher::shapes::KioType, r: order_book_matcher::shapes::KioType>(&self, arg0: order_book_matcher::ffi::env::testapi_iter__loop::arg0<s, r>, arg1: <s as order_book_matcher::shapes::KioType>::Facade) -> <r as order_book_matcher::shapes::KioType>::Facade {
        let mut __s = arg1; loop { match arg0.call(__s) { order_book_matcher::ffi::env::testapi_iter__loop::arg0_cbret::<s, r>::Left(v) => { __s = v; } order_book_matcher::ffi::env::testapi_iter__loop::arg0_cbret::<s, r>::Right(v) => return v, } }
    }

    fn testapi_text__stringConcat(&self, arg0: String, arg1: String) -> String {
        format!("{}{}", arg0, arg1)
    }
}

fn main() {
    let argv: Vec<String> = ::std::env::args().collect();
    assert_eq!(argv.len(), 3, "usage: application-bin REPEAT WARMUP");
    let repeat: usize = argv[1].parse().expect("repeat integer");
    let warmup: usize = argv[2].parse().expect("warmup integer");
    assert!((1..=1_000_000).contains(&repeat) && warmup <= 1_000_000);
    const EXPECTED: &[u8] = include_bytes!("order-book.expected.stdout");
    assert_eq!(EXPECTED.len(), 2509);
    let domain_sink = ::std::rc::Rc::new(::std::cell::RefCell::new(
        Vec::<u8>::with_capacity(EXPECTED.len())));
    let pkg = order_book_matcher::create_orderBookMatcher(StubHost {
        domain_sink: ::std::rc::Rc::clone(&domain_sink),
    });
    let mut invoke_ns = 0_u128;
    let mut checksum = 0xcbf29ce484222325_u64;
    let mut checked_bytes = 0_usize;
    let total_start = ::std::time::Instant::now();
    for index in 0..warmup + repeat {
        domain_sink.borrow_mut().clear();
        let start = ::std::time::Instant::now();
        pkg.testapi.main.main();
        let elapsed = start.elapsed().as_nanos();
        let actual = domain_sink.borrow();
        assert_eq!(actual.as_slice(), EXPECTED, "application output changed");
        if index >= warmup {
            invoke_ns += elapsed;
            checked_bytes += actual.len();
            for &byte in actual.iter() {
                checksum = (checksum ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        }
    }
    let loop_ns = total_start.elapsed().as_nanos();
    assert_eq!(checked_bytes, repeat * EXPECTED.len());
    println!("{{\"schema\":\"order-book-repeat-v1\",\"repeat\":{repeat},\"warmup\":{warmup},\"invoke_ns\":{invoke_ns},\"loop_ns\":{loop_ns},\"checked_bytes\":{checked_bytes},\"fnv1a64\":\"{checksum:016x}\"}}");
}
