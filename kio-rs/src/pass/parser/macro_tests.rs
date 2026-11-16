use crate::ast::{CallArg, Expr, Item};

fn body(source: &str) -> Expr {
    let module = super::parse(source).unwrap();
    let Item::FnDef(def) = module.items.last().unwrap() else {
        panic!("function")
    };
    def.body.clone()
}

#[test]
fn qualified_boolean_names_keep_path_ownership() {
    for call in ["a.f(())", "a . f(())", "a.t(())", "(a.f)(())"] {
        let source = format!("module main; import provider as a; fn run() {{ {call} }}");
        let Expr::Call { callee, .. } = body(&source) else {
            panic!("ordinary call: {call}")
        };
        let Expr::Path { segments, .. } = *callee else {
            panic!("qualified path")
        };
        assert_eq!(segments.len(), 2);
        let eager = super::parse(&source).unwrap();
        assert_eq!(
            super::parse_lazy(&source).unwrap().force_all().unwrap(),
            eager
        );
    }
    for prefix in [".f", ".t", ".f[Bool]"] {
        let source = format!(
            "module main; import provider(macro choose); fn run() {{ choose({prefix}) {{ () }} }}"
        );
        let Expr::BlockCall { prefix, .. } = body(&source) else {
            panic!("macro block")
        };
        assert_eq!(prefix.len(), 1);
        assert!(matches!(
            prefix[0].args.as_slice(),
            [CallArg::Value(Expr::BoolLit { .. })]
        ));
    }
}

#[test]
fn grouped_direct_heads_and_native_contexts_have_fixed_ownership() {
    for head in ["selected", "(selected)", "((selected))"] {
        let source = format!(
            "module main; import provider(macro selected); fn run() {{ {head}(()) {{ () }} }}"
        );
        let Expr::BlockCall { head: parsed, .. } = body(&source) else {
            panic!("grouped direct block")
        };
        assert_eq!(parsed.as_str(), "selected");
    }
    for call in [
        "let(()) { () }",
        "(let)(()) { () }",
        "let { () }",
        "recur(()) { () }",
    ] {
        assert!(matches!(
            body(&format!("module main; fn run() {{ {call} }}")),
            Expr::BlockCall { .. }
        ));
    }
    for call in [
        "let value { () }",
        "recur value(()) { () }",
        "selected { () }(())",
    ] {
        assert!(
            super::parse(&format!("module main; fn run() {{ {call} }}")).is_err(),
            "{call}"
        );
    }
    assert!(matches!(
        body("module main; fn run() { recur value(()) }"),
        Expr::RecCall { .. }
    ));
    let source = "module main; fn run() { (selected { .(value: .) { value } })(()) }";
    assert!(matches!(body(source), Expr::Call { .. }));
    let module = super::parse(source).unwrap();
    let formatted = crate::pretty::pretty_module(&module);
    assert!(matches!(body(&formatted), Expr::Call { .. }));
    assert!(super::parse("module main; fn run() { (selected) { () }(()) }").is_err());
}

#[test]
fn exclamation_keeps_ordinary_operator_ownership() {
    let source = "module main; fn postfix(value: .) -> . -> . { .(item: .) { item } } op _ ! { impl postfix; }; fn run() { value!(()) }";
    let Expr::Call { callee, .. } = body(source) else {
        panic!("postfix result call")
    };
    assert!(matches!(*callee, Expr::OpChain { .. }));
}

#[test]
fn macro_ufcs_boolean_prefix_stays_a_value() {
    let source = "module main; import provider(macro choose); fn run(value: .) { value.>choose (.f) { () } }";
    assert!(
        matches!(body(source), Expr::BlockCall { prefix, .. } if prefix.len() == 1 && matches!(prefix[0].args.as_slice(), [CallArg::Value(Expr::Path { .. }), CallArg::Value(Expr::BoolLit { .. })]))
    );
}

#[test]
fn every_written_block_prefix_uses_an_argument_list() {
    for prefix in [
        "value",
        "1",
        "-1",
        "1.5",
        "-1.5",
        "\"text\"",
        ".t",
        ".f[Bool]",
        "convert(value)",
        "A.member(value)",
    ] {
        let source = format!("module main; fn run() {{ selected({prefix}) {{ () }} }}");
        let Expr::BlockCall { prefix: values, .. } = body(&source) else {
            panic!("block call")
        };
        assert_eq!(values.len(), 1, "{prefix}");
        let bare = format!("module main; fn run() {{ selected {prefix} {{ () }} }}");
        assert!(super::parse(&bare).is_err(), "{bare}");
    }
    for (call, group_sizes) in [
        ("selected { () }", &[][..]),
        ("selected() { () }", &[0][..]),
        ("selected(a, b) { () }", &[2][..]),
        ("selected((a, b)) { () }", &[1][..]),
        ("selected(a)(b) { () }", &[1, 1][..]),
    ] {
        let Expr::BlockCall { prefix, .. } = body(&format!("module main; fn run() {{ {call} }}"))
        else {
            panic!("block call")
        };
        assert_eq!(
            prefix
                .iter()
                .map(|stage| stage.args.len())
                .collect::<Vec<_>>(),
            group_sizes,
            "{call}"
        );
        assert!(prefix.iter().all(|stage| {
            stage
                .args
                .iter()
                .all(|arg| matches!(arg, CallArg::Value(_)))
        }));
        if call == "selected((a, b)) { () }" {
            assert!(
                matches!(prefix[0].args.as_slice(), [CallArg::Value(Expr::Tuple { items, .. })] if items.len() == 2)
            );
        }
    }
}

#[test]
fn comptime_literal_surface_and_formatting() {
    use crate::ast::ComptimeLiteralValue;
    for (source, expected) in [
        ("__.t__(ct)", ComptimeLiteralValue::Bool(true)),
        ("__.f__(ct)", ComptimeLiteralValue::Bool(false)),
        (
            r#"__"hé\n"__(ct)"#,
            ComptimeLiteralValue::Str("hé\n".into()),
        ),
    ] {
        let module_source = format!("module main; fn run(ct: T) {{ {source} }}");
        let module = super::parse(&module_source).unwrap();
        let Item::FnDef(def) = &module.items[0] else {
            panic!("function")
        };
        assert!(matches!(&def.body, Expr::ComptimeLiteral { value, .. } if *value == expected));
        let formatted = crate::pretty::pretty_module(&module);
        assert_eq!(
            crate::pretty::pretty_module(&super::parse(&formatted).unwrap()),
            formatted
        );
    }
    for source in ["__.t__", "__.t__()", "__.t__(ct, other)", "__42__(ct)"] {
        assert!(
            super::parse(&format!("module main; fn run(ct: T) {{ {source} }}")).is_err(),
            "{source}"
        );
    }
    let literal = r"long compile-time text with an escaped newline\n and Unicode λ, repeated to exceed the ordinary string reflow threshold";
    let text = literal.replace(r"\n", "\n");
    let source = format!(
        r#"module main; fn run(ct: T) {{ __"{literal}"__( // proof
 ct // closing
 ) }}"#
    );
    let parsed = super::parse(&source).unwrap();
    let formatted = crate::pretty::pretty_module(&parsed);
    assert!(formatted.contains("// proof") && formatted.contains("// closing"));
    let reparsed = super::parse(&formatted).unwrap();
    let Item::FnDef(def) = &reparsed.items[0] else {
        panic!("function")
    };
    assert!(
        matches!(&def.body, Expr::ComptimeLiteral { value: ComptimeLiteralValue::Str(value), .. } if value == &text)
    );
    assert_eq!(crate::pretty::pretty_module(&reparsed), formatted);
    let source = "module main; fn run(ct: T) { __.t__( // proof\n ct // closing\n ) }";
    let formatted = crate::pretty::pretty_module(&super::parse(source).unwrap());
    assert!(formatted.contains("// proof") && formatted.contains("// closing"));
    assert_eq!(
        crate::pretty::pretty_module(&super::parse(&formatted).unwrap()),
        formatted
    );
}
