use super::{parse, parse_signature_file};

#[test]
fn default_modifiers_roundtrip_without_reserving_the_value_name() {
    let source = "module main; import provider(default Int as I, default as d); \
        default pub host type Float role(f64); default pub type Integer = I; \
        fn default() -> . { () }";
    let parsed = parse(source).expect("contextual default modifiers");
    let formatted = crate::pretty::pretty_module(&parsed);
    let reparsed = parse(&formatted).expect("formatted default syntax");
    assert_eq!(formatted, crate::pretty::pretty_module(&reparsed));
    assert!(formatted.contains("default host type Float"));
    assert!(formatted.contains("default pub type Integer"));
    assert!(formatted.contains("default as d"));
}

#[test]
fn default_modifiers_reject_unsupported_declarations_and_import_categories() {
    for item in [
        "pub default host type Int role(i64);",
        "default fn value() -> . { () }",
        "default host fn value() -> .;",
        "default newtype Wrapper : . { constructor wrap; projector unwrap; };",
        "default literal one = 1;",
        "import provider(default value);",
        "import provider(default op _ + _);",
    ] {
        let source = format!("module main; {item}");
        assert!(parse(&source).is_err(), "{source}");
    }
}

#[test]
fn signatures_reject_module_local_default_selections() {
    for item in [
        "default pub host type Int role(i64);",
        "default pub type Int = .;",
        "import provider(default Int);",
    ] {
        let source = format!(
            "signature app v(1); v(1) {{ nonbreaking {{ add {{ module api {{ {item} }} }} }} }}"
        );
        let error = parse_signature_file(&source, None)
            .expect_err("selection is not a contract annotation");
        assert!(error.diag().1.contains("default"), "{source}: {error:?}");
    }
    let source = "signature app v(1); v(1) { with { module api { rec { default pub type Int = Other; pub newtype Other : Int { pub constructor make; pub projector read; }; } } }; nonbreaking { add { api.Int; api.Other; } } }";
    let error = parse_signature_file(source, None)
        .expect_err("recursive members retain the contract restriction");
    assert!(error.diag().1.contains("default"), "{error:?}");
}
