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

#[test]
fn contextual_macro_selectors_commit_only_after_complete_shape() {
    for (selection, source, local, tagged) in [
        ("macro", "macro", "macro", false),
        ("macro as local", "macro", "local", false),
        ("macro as", "as", "as", true),
        ("macro as as local", "as", "local", true),
        ("macro macro as local", "macro", "local", true),
        ("macro op", "op", "op", true),
        ("macro default", "default", "default", true),
    ] {
        let module = super::parse(&format!("module main; import provider({selection});")).unwrap();
        let crate::ast::ImportKind::Selective { items, .. } = &module.imports[0].kind else {
            panic!("selective import")
        };
        assert_eq!(items[0].as_name(), Some(source));
        assert_eq!(items[0].as_local_name_with_span().unwrap().0, local);
        let crate::ast::ImportItem::Name {
            macro_span,
            default_span,
            ..
        } = &items[0]
        else {
            panic!("named selection")
        };
        assert_eq!(macro_span.is_some(), tagged, "{selection}");
        assert!(default_span.is_none());
    }
    for selection in [
        "macro Foo",
        "macro op _ + _",
        "macro default T",
        "macro {field}",
        "macro weave as Type",
    ] {
        assert!(
            super::parse(&format!("module main; import provider({selection});")).is_err(),
            "{selection}"
        );
    }
}

#[test]
fn incomplete_macro_as_does_not_commit_a_provider_selection() {
    let source = "module main; import provider(macro as ";
    let probe = super::probe_tooling(
        source,
        Some(crate::ast::KioFileKind::Module),
        Some(source.len() as u32),
        None,
    );
    assert!(
        probe
            .facts
            .cursor
            .as_ref()
            .is_none_or(|cursor| cursor.import.is_none())
    );
}
