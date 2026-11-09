use super::{parse, parse_signature_file};

#[test]
fn default_remains_an_ordinary_import_and_value_name() {
    let source =
        "module main; import provider(default as d); fn default(default: .) -> . { default }";
    let parsed = parse(source).expect("ordinary default names");
    let formatted = crate::pretty::pretty_module(&parsed);
    let reparsed = parse(&formatted).expect("formatted ordinary names");
    assert_eq!(formatted, crate::pretty::pretty_module(&reparsed));
    assert!(formatted.contains("default as d"));
    assert!(formatted.contains("fn default(default: .)"));
}

#[test]
fn removed_default_modifiers_and_selectors_are_rejected() {
    parse("module main; type { Wrapper: . { mk; get; }; };").expect("ordinary nominal declaration");
    for item in [
        "default host type Int role(i64);",
        "default pub host type Int role(i64);",
        "default type Int = .;",
        "default pub type Int = .;",
        "rec { default type Int = .; }",
        "pub default host type Int role(i64);",
        "default fn value() -> . { () }",
        "default host fn value() -> .;",
        "default type { Wrapper: . { mk; get; }; };",
        "default literal one = 1;",
        "import provider(default Int);",
        "import provider(default Int as Number);",
        "import provider(default value);",
        "import provider(default op _ + _);",
    ] {
        let source = format!("module main; {item}");
        assert!(parse(&source).is_err(), "{source}");
    }
}

#[test]
fn signatures_reject_removed_default_syntax() {
    for item in [
        "default pub host type Int role(i64);",
        "default pub type Int = .;",
        "import provider(default Int);",
    ] {
        let source = format!(
            "signature app v(1); v(1) {{ nonbreaking {{ add {{ module api {{ {item} }} }} }} }}"
        );
        assert!(parse_signature_file(&source, None).is_err(), "{source}");
    }
    let source = "signature app v(1); v(1) { with { module api { rec { default pub type Int = Other; pub type { Other: Int { pub mk; pub get; }; }; } } }; nonbreaking { add { api.Int; api.Other; } } }";
    parse_signature_file(&source.replace("default ", ""), None)
        .expect("the same recursive signature without the removed modifier");
    assert!(parse_signature_file(source, None).is_err());
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
        let crate::ast::ImportItem::Name { macro_span, .. } = &items[0] else {
            panic!("named selection")
        };
        assert_eq!(macro_span.is_some(), tagged, "{selection}");
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
