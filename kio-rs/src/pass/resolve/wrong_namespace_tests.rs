use super::{LocatedError, Package, Resolver};
use crate::ast::{Module, Prime};
use crate::error::Error;
use crate::pass::parser::parse;
use crate::span::Span;
use std::path::{Path, PathBuf};

fn prime_module(source: &str) -> Module<Prime> {
    crate::prime::lower::lower_module(parse(source).expect("source parses"))
        .expect("source uses Prime grammar")
}

fn prime_check(sources: &[(&str, &str)]) -> Result<Package<Prime>, LocatedError> {
    let modules = sources
        .iter()
        .map(|(file, source)| (PathBuf::from(file), prime_module(source)))
        .collect();
    let package = Package::build(Path::new(""), modules, None)?;
    package.resolve_imports()?;
    package.check_in_body_resolution()?;
    crate::prime::typer::check_package(&package)
}

#[cfg(feature = "surface")]
fn full_check(sources: &[(&str, &str)]) -> Result<Package<Prime>, LocatedError> {
    use crate::pipeline::Pipeline;
    let parsed = sources
        .iter()
        .map(|(file, source)| (PathBuf::from(file), parse(source).expect("source parses")))
        .collect();
    let (modules, _) = crate::pass::full::FullPipeline::lower_package(parsed, None)?;
    let package = Package::build(Path::new(""), modules, None)?;
    package.resolve_imports()?;
    package.check_in_body_resolution()?;
    crate::pass::typecheck_full::check_package(&package)
}

fn assert_known_type(error: &Error, name: &str, primary: Span, kind: &str) {
    let diagnostic = error.diagnostic();
    assert_eq!(
        diagnostic.message,
        format!("`{name}` is a type, not a value")
    );
    assert_eq!(diagnostic.span, primary);
    assert_eq!(diagnostic.secondary().len(), 1);
    assert_eq!(
        diagnostic.secondary()[0].text,
        format!("{kind} declared here")
    );
    assert_eq!(diagnostic.unresolved_name(), None);
    assert!(diagnostic.fixes().is_empty());
}

#[test]
fn type_parameter_call_argument_fallback_keeps_admitted_calls() {
    let source = "module main;
        newtype List[A] : . { constructor mk; projector get; };
        fn nil[A]() -> List(A) { List.mk(()) }
        fn cons[A](head: A, tail: List(A)) -> List(A) { tail }
        fn singleton[A](x: A) -> List(A) { cons(x, nil(A, ())) }
        fn identity[A](value: A) -> A { value }
        fn applied[*F, A](value: F(A)) -> F(A) { identity(F(A), value) }
        fn residual[A]() -> . -> List(A) { nil(A) }";
    #[cfg(feature = "surface")]
    full_check(&[("main.kio", source)]).expect("ordinary full type arguments remain admitted");
    let prime_source = source
        .replace("List.mk(())", "List.mk(A, ())")
        .replace("cons(x, nil", "cons(A, x, nil");
    prime_check(&[("main.kio", &prime_source)])
        .expect("explicit ordinary Prime type arguments remain admitted");
}

#[test]
fn type_parameter_argument_resolution_keeps_nested_lexical_scope() {
    let source = "module main;
        fn identity[A](value: A) -> A { value }
        fn shadow[A](value: A) -> A {
            let inner = .[A](other: A) -> A { identity(A, other) };
            inner(A, value)
        }";
    Resolver::check_module(&prime_module(source))
        .expect("the body resolver must not reject a lexical type argument");
}

#[test]
fn type_argument_resolution_requires_the_exact_reserved_binding() {
    use crate::ast::{CallArg, Expr, Item, SignatureParam};
    let source = "module main; fn accept[A]() -> . { () } fn caller[A]() -> . { accept(A, ()) }";
    let mut module = prime_module(source);
    let Item::FnDef(caller) = &mut module.items[1] else {
        panic!("caller")
    };
    let SignatureParam::Type(param) = &mut caller.sig.params[0] else {
        panic!("type binder")
    };
    param.name = "__Bound_type__".to_owned();
    let Expr::Call { args, .. } = &mut caller.body else {
        panic!("call")
    };
    let CallArg::Value(argument @ Expr::Path { .. }) = &mut args[0] else {
        panic!("type candidate")
    };
    let Expr::Path { segments, .. } = argument else {
        unreachable!()
    };
    segments[0].name = "__Bound_type__".to_owned();
    assert!(crate::pass::typecheck_core::value_arg_looks_like_type_arg(
        argument
    ));
    Resolver::check_module(&module).expect("an exact hygienic type binder remains a candidate");

    let Item::FnDef(caller) = &mut module.items[1] else {
        panic!("caller")
    };
    let SignatureParam::Type(param) = &mut caller.sig.params[0] else {
        panic!("type binder")
    };
    param.name = "__Other_type__".to_owned();
    let error = Resolver::check_module(&module).expect_err("reservation alone grants no binding");
    assert!(error.diagnostic().message.contains("__Bound_type__"));
    assert!(error.diagnostic().secondary().is_empty());

    let mut module = prime_module(
        "module main; fn consume(value: .) -> . { value } fn caller(value: .) -> . { consume(value) }",
    );
    let Item::FnDef(caller) = &mut module.items[1] else {
        panic!("caller")
    };
    let SignatureParam::Value(param) = &mut caller.sig.params[0] else {
        panic!("value binder")
    };
    param.name = "__bound_value__".to_owned();
    let Expr::Call { args, .. } = &mut caller.body else {
        panic!("call")
    };
    let CallArg::Value(Expr::Path { segments, .. }) = &mut args[0] else {
        panic!("value argument")
    };
    segments[0].name = "__bound_value__".to_owned();
    Resolver::check_module(&module).expect("a reserved value binding still uses value resolution");
}

#[test]
fn type_parameter_argument_in_a_value_slot_still_rejects() {
    let source =
        "module main; fn consume(value: .) -> . { value } fn caller[A]() -> . { consume(A) }";
    Resolver::check_module(&prime_module(source)).expect("call slot selection belongs to typing");
    prime_check(&[("main.kio", source)]).expect_err("a type cannot supply a value slot");
    #[cfg(feature = "surface")]
    full_check(&[("main.kio", source)]).expect_err("a type cannot supply a value slot");
}

#[cfg(feature = "surface")]
#[test]
fn type_parameter_call_argument_fallback_covers_ufcs_and_bang_calls() {
    let source = "module main;
        import __comptime__;
        pure fn implementation(ct: __Comptime__, _ty: __Type__, value: __Checked_term__) -> __Checked_term__ { value }
        elab identity : [A] A -> A { impl implementation; };
        fn ordinary[A](value: A) -> A { value }
        fn ufcs[A](value: A) -> A { value.>ordinary(A) }
        fn bang[A](value: A) -> A { identity!(A, value) }
        fn bang_ufcs[A](value: A) -> A { value.>identity!(A) }";
    full_check(&[("main.kio", source)]).expect("all call paths defer type arguments to typing");
}

#[test]
fn type_parameter_value_error_selects_the_inner_binding_before_normalization() {
    for name in ["A", "_A"] {
        let source = format!(
            "module main; fn outer[{name}](value: {name}) -> . {{ let inner = .[{name}](other: {name}) -> . {{ {name} }}; () }}"
        );
        let primary = source.rfind(&format!("{name} }}")).unwrap();
        let declaration = source.rfind(&format!("[{name}]")).unwrap();
        let expected_primary = Span::new(primary as u32, (primary + name.len()) as u32);
        let expected_declaration =
            Span::new(declaration as u32, (declaration + name.len() + 2) as u32);
        let module = prime_module(&source);
        let error = Resolver::check_module(&module).unwrap_err();
        assert!(matches!(error, Error::NameRes(_)));
        assert_known_type(&error, name, expected_primary, "type parameter");
        assert_eq!(error.diagnostic().secondary()[0].span, expected_declaration);
        assert_eq!(error.diagnostic().secondary()[0].file, None);
        let prime = prime_check(&[("main.kio", &source)]).unwrap_err();
        assert_eq!(prime.error.diagnostic(), error.diagnostic());
        #[cfg(feature = "surface")]
        {
            let full = full_check(&[("main.kio", &source)]).unwrap_err();
            assert_eq!(full.error.diagnostic(), error.diagnostic());
        }
    }
}

#[test]
fn type_parameter_value_error_does_not_select_a_shadowed_nominal() {
    let source = "module main; type A = .; fn value[A]() -> . { A }";
    let error = prime_check(&[("main.kio", source)]).unwrap_err().error;
    let primary = source.rfind("A }").unwrap();
    assert_known_type(
        &error,
        "A",
        Span::new(primary as u32, primary as u32 + 1),
        "type parameter",
    );
    let declaration = source.find("[A]").unwrap();
    assert_eq!(
        error.diagnostic().secondary()[0].span,
        Span::new(declaration as u32, declaration as u32 + 3)
    );
}

#[test]
fn local_nominal_value_errors_keep_exact_declaration_kind_and_span() {
    for (declaration, kind, binding_start, binding_len) in [
        ("host type Value;", "host type", 5, "type Value;".len()),
        ("type Value = .;", "type alias", 5, 5),
        (
            "newtype Value : . { constructor mk; projector get; };",
            "newtype",
            8,
            5,
        ),
    ] {
        let source = format!("module main; {declaration} fn value() -> . {{ Value }}");
        let primary = source.rfind("Value }").unwrap();
        let expected_primary = Span::new(primary as u32, primary as u32 + 5);
        let binding = "module main; ".len() + binding_start;
        let expected_binding = Span::new(binding as u32, (binding + binding_len) as u32);
        let error = prime_check(&[("main.kio", &source)]).unwrap_err().error;
        assert!(matches!(error, Error::NameRes(_)));
        assert_known_type(&error, "Value", expected_primary, kind);
        assert_eq!(error.diagnostic().secondary()[0].span, expected_binding);
        assert_eq!(error.diagnostic().secondary()[0].file, None);
        #[cfg(feature = "surface")]
        {
            let full = full_check(&[("main.kio", &source)]).unwrap_err();
            assert_eq!(full.error.diagnostic(), error.diagnostic());
        }
    }
}

#[test]
fn imported_nominal_value_errors_keep_the_selected_provider() {
    let provider = "module provider; pub type Value = .;";
    let decoy = "module decoy; pub newtype Value : . { pub constructor mk; pub projector get; };";
    for (usage, value, code) in [
        (
            "import provider(Value);",
            "Value",
            crate::exit_code::ExitCode::NameRes,
        ),
        (
            "import provider as p;",
            "p.Value",
            crate::exit_code::ExitCode::Type,
        ),
    ] {
        let consumer = format!("module consumer; {usage} fn value() -> . {{ {value} }}");
        let sources = [
            ("provider.kio", provider),
            ("decoy.kio", decoy),
            ("consumer.kio", consumer.as_str()),
        ];
        let located = prime_check(&sources).unwrap_err();
        assert_eq!(located.file_path, PathBuf::from("consumer.kio"));
        assert_eq!(located.error.exit_code(), code);
        let primary = consumer.rfind(&format!("{value} }}")).unwrap();
        assert_known_type(
            &located.error,
            value,
            Span::new(primary as u32, (primary + value.len()) as u32),
            "type alias",
        );
        let label = &located.error.diagnostic().secondary()[0];
        assert_eq!(label.file, Some(PathBuf::from("provider.kio")));
        let declaration = provider.find("Value").unwrap();
        assert_eq!(
            label.span,
            Span::new(declaration as u32, declaration as u32 + 5)
        );
        #[cfg(feature = "surface")]
        {
            let full = full_check(&sources).unwrap_err();
            assert_eq!(full.error.diagnostic(), located.error.diagnostic());
        }
    }
}

#[test]
fn missing_inaccessible_and_later_nominals_do_not_claim_an_available_type() {
    for source in [
        "module main; fn value() -> . { Missing }",
        "module main; fn value() -> . { Value } host type Value;",
    ] {
        let error = prime_check(&[("main.kio", source)]).unwrap_err().error;
        assert!(error.diagnostic().message.starts_with("unbound name"));
        assert!(!error.diagnostic().message.contains("is a type"));
        assert!(error.diagnostic().secondary().is_empty());
    }
    for visibility in ["", "pub(owner)"] {
        let provider = format!("module owner/provider; {visibility} type Hidden = .;");
        let consumer = "module consumer; import owner/provider as p; fn value() -> . { p.Hidden }";
        let error = prime_check(&[
            ("owner/provider.kio", &provider),
            ("consumer.kio", consumer),
        ])
        .unwrap_err()
        .error;
        assert_eq!(
            error.diagnostic().message,
            "module aliased as `p` has no `pub fn` named `Hidden`"
        );
        assert!(error.diagnostic().secondary().is_empty());
    }
}

#[test]
fn valid_type_bindings_and_constructor_paths_keep_their_meaning() {
    for source in [
        "module main; host type Value; fn value(input: Value) -> Value { input }",
        "module main; fn value[A](input: A) -> A { input }",
        "module main; newtype Value : . { constructor mk; projector get; }; fn value() -> . { Value.get(Value.mk(())) }",
        "module main; fn outer[A](value: A) -> . { let inner = .[A](other: A) -> A { other }; () }",
    ] {
        prime_check(&[("main.kio", source)]).expect("valid type/value positions remain accepted");
        #[cfg(feature = "surface")]
        full_check(&[("main.kio", source)]).expect("full pipeline preserves the same valid source");
    }
}

#[cfg(feature = "surface")]
#[test]
fn generated_nominal_value_errors_use_the_lowered_declaration_kind() {
    for declaration in ["labels { field: . };", "labels Fields = { field: . };"] {
        let source = format!("module main; {declaration} fn value() -> . {{ Field }}");
        let error = full_check(&[("main.kio", &source)]).unwrap_err().error;
        let primary = source.rfind("Field }").unwrap();
        assert_known_type(
            &error,
            "Field",
            Span::new(primary as u32, primary as u32 + 5),
            "newtype",
        );
    }
}

#[test]
fn scoped_import_value_error_uses_the_accessible_declaration() {
    let provider = "module owner/provider; pub(owner) type Value = .;";
    let consumer = "module owner/consumer; import owner/provider as p; fn value() -> . { p.Value }";
    let sources = [
        ("owner/provider.kio", provider),
        ("owner/consumer.kio", consumer),
    ];
    let error = prime_check(&sources).unwrap_err().error;
    assert_eq!(
        error.diagnostic().message,
        "`p.Value` is a type, not a value"
    );
    assert_eq!(
        error.diagnostic().secondary()[0].file,
        Some(PathBuf::from("owner/provider.kio"))
    );
    #[cfg(feature = "surface")]
    assert_eq!(
        full_check(&sources).unwrap_err().error.diagnostic(),
        error.diagnostic()
    );
}
