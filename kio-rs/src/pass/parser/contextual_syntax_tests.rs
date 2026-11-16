use super::parse;

#[test]
fn default_remains_an_ordinary_import_and_value_name() {
    let source =
        "mod main; import provider {fn default as d} fn default(default: .) -> . { default }";
    let parsed = parse(source).expect("ordinary default names");
    let formatted = crate::pretty::pretty_module(&parsed);
    let reparsed = parse(&formatted).expect("formatted ordinary names");
    assert_eq!(formatted, crate::pretty::pretty_module(&reparsed));
    assert!(formatted.contains("default as d"));
    assert!(formatted.contains("fn default(default: .)"));
}

#[test]
fn contextual_macro_selectors_commit_only_after_complete_shape() {
    for (selection, source, local, tagged) in [
        ("fn macro", "macro", "macro", false),
        ("fn macro as local", "macro", "local", false),
        ("macro as", "as", "as", true),
        ("macro as as local", "as", "local", true),
        ("macro macro as local", "macro", "local", true),
        ("macro op", "op", "op", true),
        ("macro default", "default", "default", true),
    ] {
        let module = super::parse(&format!("mod main; import provider {{{selection}}}")).unwrap();
        let crate::ast::ImportKind::Selective { items, .. } = &module.imports[0].kind else {
            panic!("selective import")
        };
        assert_eq!(items[0].as_name(), Some(source));
        assert_eq!(items[0].as_local_name_with_span().unwrap().0, local);
        let crate::ast::ImportItem::Name {
            kind, kind_span, ..
        } = &items[0]
        else {
            panic!("named selection")
        };
        assert_eq!(
            *kind == crate::ast::ImportNameKind::Macro,
            tagged,
            "{selection}"
        );
        assert!(kind_span.is_some(), "{selection}");
    }
    for selection in [
        "macro Foo",
        "macro op _ + _",
        "macro default T",
        "macro {field}",
        "macro weave as Type",
    ] {
        assert!(
            super::parse(&format!("mod main; import provider {{{selection}}}")).is_err(),
            "{selection}"
        );
    }
}

#[test]
fn incomplete_macro_as_does_not_commit_a_provider_selection() {
    let source = "mod main; import provider {macro as as ";
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
