use crate::ast::{Expr, Item};

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
    for prefix in [".f", ".t", ".f(Bool)"] {
        let source = format!(
            "module main; import provider(macro choose); fn run() {{ choose({prefix}) {{ () }} }}"
        );
        let Expr::BlockCall { prefix, .. } = body(&source) else {
            panic!("macro block")
        };
        assert!(matches!(prefix.as_slice(), [Expr::BoolLit { .. }]));
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
        matches!(body(source), Expr::BlockCall { prefix, .. } if matches!(prefix.as_slice(), [Expr::Path { .. }, Expr::BoolLit { .. }]))
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
        ".f(Bool)",
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
    for (call, count) in [
        ("selected { () }", 0),
        ("selected() { () }", 1),
        ("selected(a, b) { () }", 2),
        ("selected((a, b)) { () }", 1),
    ] {
        let Expr::BlockCall { prefix, .. } = body(&format!("module main; fn run() {{ {call} }}"))
        else {
            panic!("block call")
        };
        assert_eq!(prefix.len(), count, "{call}");
    }
}
