use ffi_rust_exact_host_bindings::host::FfiRustExactHostBindingsHost;
use std::cell::RefCell;
use std::rc::Rc;

fn require_exact_host_contract<H: FfiRustExactHostBindingsHost>() {
    fn require_i32_role<T: Clone + PartialEq + 'static + From<i32>>() {}
    fn require_string_role<T: Clone + PartialEq + 'static + From<String>>() {}
    fn require_bool_role<T: Clone + PartialEq + 'static + From<bool>>() {}
    fn require_plain<T: Clone + PartialEq + 'static>() {}

    require_i32_role::<H::left__Shared>();
    require_i32_role::<H::right__Shared>();
    require_string_role::<H::main__Text>();
    require_string_role::<H::main__OwnedText>();
    require_plain::<H::main__Token>();
    require_bool_role::<H::main__Unused>();
}

#[derive(Clone, Debug, PartialEq)]
struct RightShared(i64);

impl From<i32> for RightShared {
    fn from(value: i32) -> Self {
        Self(i64::from(value))
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Text(String);

impl From<String> for Text {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct OwnedText(String);

impl From<String> for OwnedText {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Token(u8);

#[derive(Clone, Debug, PartialEq)]
struct Unused(bool);

impl From<bool> for Unused {
    fn from(value: bool) -> Self {
        Self(value)
    }
}

#[derive(Clone, Default)]
struct ExactHost {
    calls: Rc<RefCell<Vec<&'static str>>>,
}

fn assert_text_callback_alias(
    _: &ffi_rust_exact_host_bindings::ffi::exp::main__applyText::arg0<ExactHost>,
) {
}

impl FfiRustExactHostBindingsHost for ExactHost {
    type left__Shared = i64;
    type main__OwnedText = OwnedText;
    type main__Text = Text;
    type main__Token = Token;
    type main__Unused = Unused;
    type right__Shared = RightShared;

    fn a___c(&self) {
        self.calls.borrow_mut().push("a._c");
    }

    fn __kio_host_a_u__c(&self) {
        self.calls.borrow_mut().push("a_.c");
    }

    fn main__borrowText(&self, value: Self::main__Text) -> Self::main__Text {
        value
    }

    fn main__ownText(&self, value: Self::main__OwnedText) -> Self::main__OwnedText {
        value
    }

    fn main__hostIdentityFlag(&self, flag: Self::main__Unused) -> Self::main__Unused {
        flag
    }
}

fn main() {
    require_exact_host_contract::<ExactHost>();
    let host = ExactHost::default();
    let calls = Rc::clone(&host.calls);
    let package = ffi_rust_exact_host_bindings::create_ffiRustExactHostBindings(host);

    assert_eq!(package.left.keep(41_i64), 41_i64);
    assert_eq!(package.left.literal(), 41_i64);
    let left_step =
        ffi_rust_exact_host_bindings::ffi::exp::left__apply::arg0::<ExactHost>::new(|value| {
            value + 1
        });
    assert_eq!(package.left.apply(left_step, 41_i64), 42_i64);
    assert_eq!(package.right.keep(RightShared(42)), RightShared(42));
    assert_eq!(package.right.literal(), RightShared(42));
    let duplicated = package.right.duplicate(RightShared(43));
    assert_eq!(duplicated._0, RightShared(43));
    assert_eq!(duplicated._1, RightShared(43));
    assert_eq!(
        package.main.roundtripText(Text("owned".to_owned())),
        Text("owned".to_owned()),
    );
    assert_eq!(
        package.main.roundtripTextLiteral(),
        Text("direct literal".to_owned()),
    );
    assert_eq!(
        package
            .main
            .roundtripOwnedText(OwnedText("owned".to_owned())),
        OwnedText("owned".to_owned()),
    );
    assert_eq!(
        package.main.roundtripOwnedTextLiteral(),
        OwnedText("direct owned literal".to_owned()),
    );
    assert_eq!(package.main.keepToken(Token(7)), Token(7));
    let direct_step =
        ffi_rust_exact_host_bindings::ffi::exp::main__applyText::arg0::<ExactHost>::new(
            |value: Text| Text(format!("{} callback", value.0)),
        );
    assert_text_callback_alias(&direct_step);
    assert_eq!(
        package
            .main
            .applyText(direct_step, Text("direct".to_owned())),
        Text("direct callback".to_owned()),
    );
    let stored = ffi_rust_exact_host_bindings::ffi::exp::main__applyStoredText::arg0::<ExactHost> {
        _0: ffi_rust_exact_host_bindings::ffi::exp::main__applyText::arg0::<ExactHost>::new(
            |value: Text| Text(format!("{} stored", value.0)),
        ),
        _1: Text("structural".to_owned()),
    };
    assert_eq!(
        package.main.applyStoredText(stored),
        Text("structural stored".to_owned()),
    );
    let identity = package.main.textIdentity();
    assert_eq!(
        identity.call(Text("returned callback".to_owned())),
        Text("returned callback".to_owned()),
    );
    assert_eq!(package.main.textLiteral(), Text("literal".to_owned()));
    assert_eq!(
        package.main.ownedTextLiteral(),
        OwnedText("owned literal".to_owned()),
    );
    assert_eq!(
        package.main.choose(Unused(true), Token(1), Token(2)),
        Token(1),
    );
    assert_eq!(
        package.main.choose(Unused(false), Token(1), Token(2)),
        Token(2),
    );
    assert_eq!(
        package.main.chooseCall(Unused(true), Token(3), Token(4)),
        Token(3),
    );
    assert_eq!(
        package
            .main
            .chooseHostCall(Unused(true), Token(11), Token(12)),
        Token(11),
    );
    assert_eq!(
        package
            .main
            .chooseHostCall(Unused(false), Token(13), Token(14)),
        Token(14),
    );
    assert_eq!(
        package.main.chooseLet(Unused(false), Token(3), Token(4)),
        Token(4),
    );
    assert_eq!(package.main.chooseLiteral(Token(5), Token(6)), Token(5));
    assert_eq!(
        package
            .main
            .chooseClosure(Unused(false), Token(7), Token(8)),
        Token(8),
    );
    let choice =
        ffi_rust_exact_host_bindings::ffi::exp::main__chooseDestructured::arg0::<ExactHost> {
            _0: Unused(true),
            _1: Token(9),
        };
    assert_eq!(
        package.main.chooseDestructured(choice, Token(10)),
        Token(9),
    );

    package.a.call();
    package.a_.call();

    let observed = calls.borrow();
    assert_eq!(observed.as_slice(), &["a._c", "a_.c"]);
}
