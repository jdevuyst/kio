use super::{Package, Resolver};
use crate::ast::{Module, Prime};
use crate::error::Error;
use crate::pass::parser::parse;
use crate::span::Span;
use std::path::{Path, PathBuf};

fn prime_module(source: &str) -> Module<Prime> {
    crate::prime::lower::lower_module(parse(source).expect("source parses"))
        .expect("source uses Prime grammar")
}

fn assert_later_host_error(source: &str, name: &str) {
    let module = prime_module(source);
    let error = Resolver::check_module(&module).expect_err("later host must be out of scope");
    let Error::NameRes(diagnostic) = error else {
        panic!("expected name resolution, got {error:?}");
    };
    assert_eq!(diagnostic.message, format!("unbound name `{name}`"));
    let start = u32::try_from(source.find(name).expect("reference is present")).unwrap();
    assert_eq!(
        diagnostic.span,
        Span::new(start, start + u32::try_from(name.len()).unwrap()),
        "the diagnostic belongs to the use, not the later declaration"
    );
}

#[test]
fn host_source_order_prime_rejects_later_value() {
    assert_later_host_error(
        "module x; fn before() -> . { later() } host fn later() -> .;",
        "later",
    );
}

#[test]
fn host_source_order_prime_rejects_later_signature_type() {
    assert_later_host_error(
        "module x; fn before(value: Later) -> Later { value } host type Later;",
        "Later",
    );
}

#[test]
fn host_source_order_prime_rejects_later_host_signature_type() {
    assert_later_host_error(
        "module x; host fn before(value: Later) -> Later; host type Later;",
        "Later",
    );
}

#[test]
fn host_source_order_prime_rejects_later_type_in_payloads_and_annotations() {
    for source in [
        "module x; type Earlier = Later; host type Later;",
        "module x; newtype Earlier : Later { constructor mk; projector get; }; host type Later;",
        "module x; rec newtype Node : . | (Node & Later) { constructor mk; projector get; }; host type Later;",
        "module x; fn before() -> . { let value = .(x: Later) -> Later { x }; () } host type Later;",
    ] {
        assert_later_host_error(source, "Later");
    }
}

#[test]
fn host_source_order_prime_preserves_prior_hosts_and_written_recursion() {
    for source in [
        "module x; host fn earlier() -> .; fn after() -> . { earlier() }",
        "module x; host type Earlier; host fn identity(value: Earlier) -> Earlier; fn after(value: Earlier) -> Earlier { identity(value) }",
        "module x; host type Earlier; rec newtype Node : . | (Node & Earlier) { constructor mk; projector get; };",
        "module x; host type Earlier; rec { newtype Node : Payload { constructor mk; projector get; }; type Payload = . | (Node & Earlier); }",
    ] {
        Resolver::check_module(&prime_module(source)).expect("preceding hosts remain in scope");
    }
}

#[test]
fn host_source_order_prime_preserves_explicit_external_imports() {
    let modules = [
        (
            PathBuf::from("provider.kio"),
            prime_module(
                "module provider; host type Value; host fn identity(value: Value) -> Value;",
            ),
        ),
        (
            PathBuf::from("selective.kio"),
            prime_module(
                "module selective; import provider(Value, identity); fn after(value: Value) -> Value { identity(value) }",
            ),
        ),
        (
            PathBuf::from("qualified.kio"),
            prime_module(
                "module qualified; import provider as p; fn after(value: p.Value) -> p.Value { p.identity(value) }",
            ),
        ),
    ];
    let package = Package::build(Path::new(""), modules.into(), None).expect("package builds");
    package
        .resolve_imports()
        .expect("the written imports resolve");
    package
        .check_in_body_resolution()
        .expect("external imports retain their exact host bindings");
}

#[cfg(feature = "surface")]
#[test]
fn host_source_order_lowered_rejects_later_value_and_type() {
    for (source, name) in [
        (
            "module x; fn before() -> . { later() } host fn later() -> .;",
            "later",
        ),
        (
            "module x; fn before(value: Later) -> Later { value } host type Later;",
            "Later",
        ),
        (
            "module x; equiv before() { later(); () } host fn later() -> .;",
            "later",
        ),
        (
            "module x; fn before() -> . { let .(value: Later -> Later) = .(x: Later) -> Later { x }; () } host type Later;",
            "Later",
        ),
    ] {
        let surface = parse(source).expect("source parses");
        let desugared = crate::pass::desugar::desugar_module(surface).expect("desugars");
        let (mut modules, _) = crate::pass::label_elab::elaborate_package(
            vec![(PathBuf::from("x.kio"), desugared)],
            None,
        )
        .expect("labels lower");
        let module = modules.pop().expect("one module").1;
        let error = Resolver::check_module(&module).expect_err("later host is unavailable");
        assert!(
            matches!(error, Error::NameRes(ref diagnostic)
            if diagnostic.message == format!("unbound name `{name}`")),
            "{error:?}"
        );
    }
}
