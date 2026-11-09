use super::{parse, parse_lazy};
use crate::ast::{BlockExposure, CallArg, CallStage, Expr, Item, NeutralItem};

#[test]
fn block_cursor_regions_survive_incomplete_bodies_without_descriptors() {
    use super::BlockCursorRegion;
    for (body, expected_head, expected_region) in [
        ("outer (val|ue) { () }", "outer", BlockCursorRegion::Prefix),
        (
            "((outer))(val|ue) { () }",
            "outer",
            BlockCursorRegion::Prefix,
        ),
        (
            "val|ue.>outer(()) { () }",
            "outer",
            BlockCursorRegion::Prefix,
        ),
        (
            "outer(()) { () }.<val|ue",
            "outer",
            BlockCursorRegion::Prefix,
        ),
        ("outer { val|ue }", "outer", BlockCursorRegion::Body(0)),
        (
            "outer { () } fa|llback { () }",
            "outer",
            BlockCursorRegion::Label(1),
        ),
        ("outer { () } |", "outer", BlockCursorRegion::Label(1)),
        (
            "outer { () } fallback { val|",
            "outer",
            BlockCursorRegion::Body(1),
        ),
        ("outer { inner { val|", "inner", BlockCursorRegion::Body(0)),
    ] {
        let marked = format!("module app; fn f(value: .) {{ {body}");
        let offset = marked.find('|').unwrap() as u32;
        let source = marked.replace('|', "");
        let probe = super::probe_tooling(
            &source,
            Some(crate::ast::KioFileKind::Module),
            Some(offset),
            None,
        );
        let fact = probe
            .facts
            .block_cursor
            .unwrap_or_else(|| panic!("{marked}: no block cursor"));
        assert_eq!(fact.head.as_str(), expected_head, "{marked}");
        assert_eq!(fact.region, expected_region, "{marked}");
        if matches!(expected_region, BlockCursorRegion::Label(_)) {
            assert_eq!(
                probe.facts.cursor.unwrap().slot,
                super::CursorSlot::BlockLabel,
                "{marked}"
            );
        }
    }
}

fn single_prefix_values(prefix: &[CallStage]) -> Vec<&Expr> {
    assert_eq!(prefix.len(), 1, "one written value group");
    prefix[0]
        .args
        .iter()
        .map(|arg| match arg {
            CallArg::Value(value) => value,
            CallArg::Type(_) => panic!("expected value prefix"),
        })
        .collect()
}

fn function(module: &crate::ast::Module) -> &Expr {
    let Item::FnDef(def) = module.items.last().expect("function") else {
        panic!("expected final function");
    };
    &def.body
}

#[test]
fn block_ufcs_places_the_receiver_in_the_ordinary_prefix() {
    use crate::ast::UfcsFlavor::*;
    for (body, flavor, names) in [
        (
            "r.>f(a, b) { c } next { d }",
            ReceiverFirst,
            ["r", "a", "b"],
        ),
        (
            "r.>>f(a, b) { c } next { d }",
            ReceiverLast,
            ["a", "b", "r"],
        ),
        (
            "f(a, b) { c } next { d }.<<r",
            ArgumentFirst,
            ["r", "a", "b"],
        ),
        (
            "((f(a, b) { c } next { d })).<r",
            ArgumentLast,
            ["a", "b", "r"],
        ),
    ] {
        let Expr::BlockCall {
            prefix,
            splice,
            blocks,
            ..
        } = roundtrip(body, "")
        else {
            panic!("{body}: block call")
        };
        assert_eq!(splice, Some(flavor));
        assert_eq!(blocks.len(), 2);
        let actual: Vec<_> = single_prefix_values(&prefix)
            .into_iter()
            .map(|value| match value {
                Expr::Path { segments, .. } => segments[0].name.as_str(),
                _ => panic!("{body}: path prefix"),
            })
            .collect();
        assert_eq!(actual, names, "{body}");
    }
    for body in [
        "r.>f (a) { b }",
        "r.>>f (a) { b }",
        "f (a) { b }.<<r",
        "f (a) { b }.<r",
        "r.>f { b }",
        "r.>>f { b }",
        "f { b }.<<r",
        "f { b }.<r",
        "outer (r.>convert(a)) { b }",
        "outer (r.>convert) { b }",
        "outer (g.<r) { b }",
        "outer(r.>inner (a) { c }) { b }",
        "(f (a) { b }.<r)(x)",
        "(r.>f (a) { b })(x)",
    ] {
        roundtrip(body, "");
    }
}

#[test]
fn block_reverse_splice_follows_labels_and_uses_a_tight_receiver() {
    let Expr::BlockCall {
        splice,
        prefix,
        blocks,
        ..
    } = roundtrip("outer (a) { b } label { inner (c) { d } }.<r", "")
    else {
        panic!("outer block")
    };
    assert_eq!(splice, Some(crate::ast::UfcsFlavor::ArgumentLast));
    assert_eq!(prefix.len(), 1);
    let values = single_prefix_values(&prefix);
    assert_eq!(values.len(), 2);
    for (value, name) in values.into_iter().zip(["a", "r"]) {
        assert!(matches!(value, Expr::Path { segments, .. } if segments[0].name == name));
    }
    assert!(matches!(
        &blocks[1].items[0],
        NeutralItem::Expression {
            value: Expr::BlockCall { splice: None, .. },
            ..
        }
    ));
    let Expr::BlockCall { splice, blocks, .. } =
        roundtrip("outer (a) { b } label { inner (c) { d }.<r }", "")
    else {
        panic!("outer block")
    };
    assert_eq!(splice, None);
    assert!(matches!(
        &blocks[1].items[0],
        NeutralItem::Expression {
            value: Expr::BlockCall {
                splice: Some(_),
                ..
            },
            ..
        }
    ));
    for body in [
        "outer (a) { b } label { r.>inner (c) { d } }",
        "f (a) { b }.<r(x).>g",
        "f (a) { b }.<(r.>g)",
    ] {
        roundtrip(body, "");
    }
    assert!(
        matches!(roundtrip("f (a) { b }.<r(x).>g", ""), Expr::Ufcs { receiver, .. }
        if matches!(*receiver, Expr::BlockCall { .. }))
    );
}

#[test]
fn block_reverse_splice_requires_its_own_final_label_braces() {
    use crate::error::Applicability;

    for (arrow, flavor) in [
        (".<", crate::ast::UfcsFlavor::ArgumentLast),
        (".<<", crate::ast::UfcsFlavor::ArgumentFirst),
    ] {
        for (open, close) in [("", ""), ("((", "))")] {
            for (before_child, before_arrow) in [(" ", ""), (" // child\n ", " // suffix\n ")] {
                let source = format!(
                    "module consumer; fn subject() {{ {open}outer (a) {{ b }} label{before_child}inner(c) {{ d }}{close}{before_arrow}{arrow}r }}"
                );
                let error = parse(&source).expect_err("reverse splice requires label braces");
                assert!(
                    error.diag().1.contains("reverse UFCS requires braces"),
                    "{source}: {error:?}"
                );
                let span = error.diag().0;
                assert_eq!(&source[span.start as usize..span.end as usize], arrow);
                let fixes = error.diagnostic().fixes();
                assert_eq!(fixes.len(), 1);
                let fix = &fixes[0];
                assert_eq!(fix.applicability, Applicability::MachineApplicable);
                let insertion_points = [
                    (source.find("label").unwrap() + "label".len(), " {"),
                    (source.find("{ d }").unwrap() + "{ d }".len(), " }"),
                ];
                assert_eq!(fix.edits.len(), insertion_points.len());
                for (edit, (offset, text)) in fix.edits.iter().zip(insertion_points) {
                    assert_eq!(
                        edit.span,
                        crate::span::Span::new(offset as u32, offset as u32)
                    );
                    assert_eq!(edit.replacement, text);
                }
                let mut repaired = source.clone();
                for edit in fix.edits.iter().rev() {
                    repaired.insert_str(edit.span.start as usize, &edit.replacement);
                }
                assert_eq!(comments(&source), comments(&repaired));
                let parsed = parse(&repaired).expect("brace fix parses");
                let Expr::BlockCall {
                    splice,
                    prefix,
                    blocks,
                    ..
                } = function(&parsed)
                else {
                    panic!("fix keeps the outer call")
                };
                assert_eq!(*splice, Some(flavor));
                let values = single_prefix_values(prefix);
                let receiver = if flavor.inserts_first() {
                    values[0]
                } else {
                    values.last().unwrap()
                };
                assert!(matches!(receiver, Expr::Path { segments, .. } if segments[0].name == "r"));
                assert!(!blocks[1].elided);
                assert!(matches!(
                    &blocks[1].items[0],
                    NeutralItem::Expression {
                        value: Expr::BlockCall { splice: None, .. },
                        ..
                    }
                ));
                let rendered = crate::pretty::pretty_module(&parsed);
                let reparsed = parse(&rendered).expect("formatter retains required braces");
                assert_eq!(topology(function(&parsed)), topology(function(&reparsed)));
                assert_eq!(comments(&source), comments(&rendered));
                assert_eq!(rendered, crate::pretty::pretty_module(&reparsed));
            }
        }
        roundtrip(
            &format!(
                "outer (a) {{ b }} label {{ middle(c) {{ d }} tail inner (e) {{ f }} }}{arrow}r"
            ),
            "",
        );
        roundtrip(
            &format!("outer (a) {{ b }} label {{ inner(c) {{ d }}{arrow}r }}"),
            "",
        );
        roundtrip(&format!("outer(g{arrow}r) {{ b }}"), "");
    }
    for body in [
        "outer (a) { b } label inner (c) { d }",
        "r.>outer (a) { b } label inner (c) { d }",
        "r.>>outer (a) { b } label inner (c) { d }",
        "outer (a) { b } label inner (c) { d }.>g",
    ] {
        roundtrip(body, "");
    }
}

#[test]
fn block_splice_rejects_empty_lists_without_confusing_explicit_unit() {
    for body in [
        "r.>f() { b }",
        "r.>>f() { b }",
        "f() { b }.<r",
        "((f() { b })).<<r",
    ] {
        let source = format!("module consumer; fn subject() {{ {body} }}");
        let error = parse(&source).expect_err(body);
        assert!(
            error.diag().1.contains("explicitly empty UFCS"),
            "{body}: {error:?}"
        );
        let span = error.diag().0;
        assert_eq!(&source[span.start as usize..span.end as usize], "()");
        let replacements: Vec<_> = error
            .diagnostic()
            .fixes()
            .iter()
            .flat_map(|fix| fix.edits.iter().map(|edit| edit.replacement.as_str()))
            .collect();
        assert!(replacements.contains(&"(())"));
        assert!(replacements.contains(&""));
    }
    for body in [
        "r.>f(()) { b }",
        "r.>>f(()) { b }",
        "f(()) { b }.<r",
        "f(()) { b }.<<r",
        "f() { b }",
    ] {
        roundtrip(body, "");
    }
}

#[test]
fn block_splice_does_not_reopen_completed_or_already_spliced_calls() {
    for body in [
        "f (a) { b }(x).<r",
        "(f (a) { b }.<r).<<s",
        "(r.>f (a) { b }).<s",
        "r.>m.f a { b }",
        "m.f a { b }.<r",
    ] {
        assert!(
            parse(&format!("module consumer; fn subject() {{ {body} }}")).is_err(),
            "{body}"
        );
    }
}

#[test]
fn reverse_splice_receivers_keep_their_own_block_calls() {
    for body in ["f(a).<r { b }", "f(a).<<r { b }"] {
        assert!(matches!(roundtrip(body, ""), Expr::Ufcs { receiver, .. }
            if matches!(receiver.as_ref(), Expr::BlockCall { head, .. } if head.name == "r")));
    }
    let Expr::BlockCall {
        head,
        prefix,
        blocks,
        ..
    } = roundtrip("f (a) { b }.<r (label) { c }", "")
    else {
        panic!("outer block")
    };
    assert_eq!(head.name, "f");
    assert_eq!(blocks.len(), 1);
    assert!(
        matches!(single_prefix_values(&prefix).last(), Some(Expr::BlockCall { head, blocks, .. }) if head.name == "r" && blocks.len() == 1)
    );
}

#[test]
fn block_splice_receivers_cannot_supply_type_arguments() {
    for body in [
        "A.>f { b }",
        "A.>>f { b }",
        "f { b }.<A",
        "f { b }.<<A",
        "(A[B]).>f { b }",
        "f { b }.<(A[B])",
    ] {
        let source = format!("module consumer; fn subject() {{ {body} }}");
        let error = parse(&source).expect_err("a block receiver is an ordinary value prefix");
        assert!(
            error
                .diag()
                .1
                .contains("block prefix requires a value argument"),
            "{body}: {error:?}"
        );
    }
}

#[test]
fn block_splice_nested_receivers_fit_the_default_test_stack() {
    for flavor in [".>", ".>>", ".<", ".<<"] {
        let mut body = "value".to_owned();
        for _ in 0..24 {
            body = if matches!(flavor, ".>" | ".>>") {
                format!("({body}){flavor}f(a) {{ b }}")
            } else {
                format!("f (a) {{ b }}{flavor}({body})")
            };
        }
        roundtrip(&body, "");
    }
}

#[test]
fn block_splice_formatting_preserves_comments_and_operator_boundaries() {
    for body in [
        "r // receiver\n .>f // prefix\n (a) { b }",
        "f (a) { b } // splice\n .<r // receiver\n .>g",
        "f(// unit\n ()) { b }.<r",
        "outer (a) { b } label { inner (c) { d } // splice\n .<r }",
    ] {
        roundtrip(body, "");
    }
    for body in [
        "a + ((b < k >).>f { c })",
        "[! (< x >.>f { c }) !]",
        "[! f { c }.<(< x >) !]",
    ] {
        roundtrip(
            body,
            "import absent(op _ + _, op ___ < _ >, op < _ >, varop [! !]);",
        );
    }
}

fn topology(expr: &Expr) -> serde_json::Value {
    fn normalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(macro_call) = map.get_mut("macro_call") {
                    *macro_call = serde_json::Value::Bool(!macro_call.is_null());
                }
                map.retain(|key, _| {
                    !key.ends_with("_span")
                        && !matches!(
                            key.as_str(),
                            "meta"
                                | "source_trivia"
                                | "span"
                                | "open"
                                | "close"
                                | "separators"
                                | "leading"
                                | "trailing"
                                | "id"
                                | "ext"
                                | "access_ext"
                                | "elided"
                                | "semicolon"
                        )
                });
                for child in map.values_mut() {
                    normalize(child);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    normalize(child);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::to_value(expr).unwrap();
    normalize(&mut value);
    value
}

fn comments(source: &str) -> Vec<String> {
    let (tokens, trailing) = crate::pass::lexer::lex_with_trailing(source).unwrap();
    tokens
        .into_iter()
        .flat_map(|token| token.leading_trivia)
        .chain(trailing)
        .filter_map(|trivia| match trivia {
            crate::pass::lexer::Trivia::LineComment { text, .. }
            | crate::pass::lexer::Trivia::DocCommentLine { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

fn roundtrip(body: &str, operators: &str) -> Expr {
    // The provider is deliberately absent: both parses and rendering use
    // only this source's tokens and explicitly imported operator patterns.
    let source = format!("module consumer; {operators} fn subject() {{ {body} }}");
    let parsed = parse(&source).unwrap_or_else(|error| panic!("{body}: {error:?}"));
    let rendered = crate::pretty::pretty_module(&parsed);
    let reparsed = parse(&rendered).unwrap_or_else(|error| panic!("{rendered}: {error:?}"));
    assert_eq!(
        topology(function(&parsed)),
        topology(function(&reparsed)),
        "{body}"
    );
    assert_eq!(comments(&source), comments(&rendered), "{body}");
    assert_eq!(rendered, crate::pretty::pretty_module(&reparsed), "{body}");
    function(&parsed).clone()
}

#[test]
fn block_heads_and_labels_have_no_name_specific_grammar() {
    for head in ["if", "do", "scope", "else", "let", "choose"] {
        let body = format!("{head}(value) {{ first() }} next {{ last() }}");
        let Expr::BlockCall {
            head: parsed_head,
            blocks,
            ..
        } = roundtrip(&body, "")
        else {
            panic!("expected a generic block call");
        };
        assert_eq!(parsed_head.name, head);
        assert_eq!(blocks.len(), 2);
    }
    parse("module consumer; fn subject(if: ., do: ., else: .) { (if, do, else) }")
        .expect("ordinary identifier roles");
    for body in [
        "if (condition) { yes() } else { no() }",
        "do { result() }",
        "do (receiver) { let value <- source(); value }",
    ] {
        assert!(
            parse(&format!("module consumer; fn subject() {{ {body} }}")).is_ok(),
            "{body}"
        );
    }
}

#[test]
fn boolean_block_prefixes_use_the_existing_leading_dot_literal() {
    for (body, expected) in [
        ("if (.t) { yes() } else { no() }", true),
        ("if (.f) { yes() } else { no() }", false),
        ("choose (.t[Bool]) { yes() } fallback { no() }", true),
        (
            "choose // head\n (.f) // prefix\n { yes() } fallback { no() }",
            false,
        ),
    ] {
        let expr = roundtrip(body, "");
        let Expr::BlockCall { prefix, blocks, .. } = expr else {
            panic!("expected a block call");
        };
        assert!(
            matches!(single_prefix_values(&prefix).as_slice(), [Expr::BoolLit { value, .. }] if *value == expected)
        );
        assert_eq!(blocks.len(), 2);
    }
    roundtrip("outer(inner (.t) { yes() } else { no() }) { body() }", "");
    roundtrip("outer(.t. { t1 }) { body() }", "");
    roundtrip("outer(.() { () }) { body() }", "");
    for body in [
        "outer .t. { t1 } { body() }",
        "outer .() { () } { body() }",
        "outer inner .t { yes() } { body() }",
    ] {
        assert!(
            parse(&format!("module consumer; fn subject() {{ {body} }}")).is_err(),
            "{body}"
        );
    }
}

#[test]
fn block_prefix_layout_preserves_expression_boundaries() {
    for (body, head) in [
        ("outer(value) { b() }", "outer(value) {"),
        ("outer(f[A](value)) { b() }", "outer(f[A](value)) {"),
        ("outer(A.member(value)) { b() }", "outer(A.member(value)) {"),
        ("outer(convert(value)) { b() }", "outer(convert(value)) {"),
        ("outer(-1) { b() }", "outer(-1) {"),
        ("outer(-1.5) { b() }", "outer(-1.5) {"),
        ("outer(()) { b() }", "outer(()) {"),
        ("outer() { b() }", "outer() {"),
        ("outer { b() }", "outer {"),
        ("outer(left, right) { b() }", "outer(left, right) {"),
        ("outer((left, right)) { b() }", "outer((left, right)) {"),
        ("outer({}) { b() }", "outer(()) {"),
    ] {
        roundtrip(body, "");
        let parsed = parse(&format!("module consumer; fn subject() {{ {body} }}")).unwrap();
        let rendered = crate::pretty::pretty_module(&parsed);
        assert!(rendered.contains(head), "{head}: {rendered}");
    }
    for slot in ["_", "__", "___"] {
        let operators = format!("import absent(op _ + {slot}, op - _, op _ <| _ |>);");
        for body in [
            "outer(left + right) { b() }",
            "outer (left + (inner (value) { a() })) { b() }",
            "outer(inner (value) { a() } + x) { b() }",
            "outer(left + f(inner(value) { a() })) { b() }",
            "outer (left <| inner (value) { a() } |>) { b() }",
            "outer(- value) { b() }",
        ] {
            roundtrip(body, &operators);
        }
        let parsed = parse(&format!(
            "module consumer; {operators} fn subject() {{ outer(left + right) {{ b() }} }}"
        ))
        .unwrap();
        assert!(crate::pretty::pretty_module(&parsed).contains("outer(left + right) {"));
    }
    for body in [
        "outer // prefix\n (value) { b() }",
        "outer // prefix\n (value) { b() }",
        "outer // prefix\n (f[A](value)) { b() }",
        "outer // prefix\n (convert(value)) { b() }",
        "outer // prefix\n (-1.5) { b() }",
        "outer(value // tail\n) { b() }",
        "outer // unit\n () { b() }",
        "outer(// unit contents\n) { b() }",
    ] {
        roundtrip(body, "");
    }
}

#[test]
fn final_label_elision_is_structural_and_comment_preserving() {
    let body = "outer (value) { a() } fallback { inner (next) { b() } }";
    roundtrip(body, "");
    let parsed = parse(&format!("module consumer; fn subject() {{ {body} }}")).unwrap();
    let rendered = crate::pretty::pretty_module(&parsed);
    assert!(rendered.contains("} fallback inner(next) {"), "{rendered}");
    let reparsed = parse(&rendered).unwrap();
    let Expr::BlockCall { blocks, .. } = function(&reparsed) else {
        panic!("outer block");
    };
    assert_eq!(blocks.len(), 2);
    assert!(blocks[1].elided);
    for body in [
        "outer (value) { a() } fallback { inner (next) { b() } } cleanup { c() }",
        "outer (value) { a() } fallback { // wrapper\n inner (next) { b() } }",
        "outer (value) { a() } fallback { inner (next) { b() } // wrapper\n }",
        "outer (value) { a() } fallback { before(); inner (next) { b() } }",
        "outer (value) { a() } fallback { let x = next; inner (x) { b() } }",
        "outer (value) { a() } fallback // child\n inner (next) { b() }",
    ] {
        roundtrip(body, "");
        let parsed = parse(&format!("module consumer; fn subject() {{ {body} }}")).unwrap();
        let rendered = crate::pretty::pretty_module(&parsed);
        assert!(rendered.contains("fallback {"), "{rendered}");
    }
    for body in [
        "outer { // child\n inner (value) { a() } }",
        "outer { let value = // child\n inner (x) { a() }; value }",
        "outer(// child\n inner (x) { a() }) { b() }",
        "consume(// child\n inner (x) { a() })",
    ] {
        roundtrip(body, "");
    }
}

#[test]
fn descriptors_keep_relative_order_in_eager_and_lazy_headers() {
    let source = "module consumer; macro choose: . -> . { impl impl_choose; trailing thunk; captures captured; trailing thunk else; trailing product finally }";
    let parsed = parse(source).unwrap();
    let lazy = parse_lazy(source).unwrap();
    let Item::Macro(eager, _) = &parsed.items[0] else {
        panic!("macro");
    };
    let Item::Macro(deferred, _) = &lazy.module().items[0] else {
        panic!("macro");
    };
    let descriptors = |def: &crate::ast::UserMacroDef| {
        def.trailing_blocks
            .iter()
            .map(|block| {
                (
                    block.exposure,
                    block.label.as_ref().map(|label| label.name.clone()),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(descriptors(eager), descriptors(deferred));
    assert_eq!(
        descriptors(eager),
        vec![
            (BlockExposure::Thunk, None),
            (BlockExposure::Thunk, Some("else".to_owned())),
            (BlockExposure::Product, Some("finally".to_owned())),
        ]
    );
    let rendered = crate::pretty::pretty_module(&parsed);
    assert_eq!(
        rendered,
        crate::pretty::pretty_module(&parse(&rendered).unwrap())
    );
}

#[test]
fn blockless_prefix_and_grouped_child_have_distinct_ownership() {
    let ordinary = roundtrip("outer (convert(value)) { b() }", "");
    let Expr::BlockCall { prefix, blocks, .. } = ordinary else {
        panic!("outer block");
    };
    assert!(matches!(
        single_prefix_values(&prefix).as_slice(),
        [Expr::Call { .. }]
    ));
    assert_eq!(blocks.len(), 1);
    let nested = roundtrip("outer(convert(value) { a() }) { b() }", "");
    let Expr::BlockCall { prefix, .. } = nested else {
        panic!("outer block");
    };
    assert!(matches!(
        single_prefix_values(&prefix).as_slice(),
        [Expr::BlockCall { .. }]
    ));
    let elided = roundtrip(
        "choose (condition) { first() } else choose (next) { second() } else { third() }",
        "",
    );
    let Expr::BlockCall { blocks, .. } = elided else {
        panic!("outer block");
    };
    assert!(blocks[1].elided);
    assert!(matches!(
        blocks[1].items.as_slice(),
        [NeutralItem::Expression {
            value: Expr::BlockCall { .. },
            ..
        }]
    ));
}

#[test]
fn slot_chain_extent_does_not_steal_enclosing_body() {
    for slot in ["_", "__", "___"] {
        let operators = format!("import absent(op _ + {slot}, op <| _ |>);");
        for body in [
            "outer (left + value) { b() }",
            "outer (left + (inner (value) { a() })) { b() }",
            "outer (left + <| inner (value) { a() } |>) { b() }",
            "outer(left + value) { b() } + {}",
        ] {
            let expr = roundtrip(body, &operators);
            if body.ends_with("{ b() }") {
                assert!(matches!(expr, Expr::BlockCall { .. }), "{body}");
            }
        }
        let source = format!(
            "module consumer; {operators} fn subject() {{ outer left + inner (value) {{ a() }} {{ b() }} }}"
        );
        assert!(parse(&source).is_err(), "{slot}");
    }
}

#[test]
fn block_prefix_role_follows_only_the_callee_spine() {
    for value in ["A", "A[B]", "A[B][C]"] {
        let source = format!("module consumer; fn subject() {{ outer({value}) {{ b() }} }}");
        assert!(parse(&source).is_err(), "{value}");
    }
    for value in ["f[A]", "f[A](value)", "A.member[B](value)", "-1", "-1.5"] {
        roundtrip(&format!("outer({value}) {{ b() }}"), "");
    }
    roundtrip("outer(- value) { b() }", "import absent(op - _);");
    assert!(
        parse("module consumer; import absent(op - _); fn subject() { outer - value { b() } }")
            .is_err()
    );
}

#[test]
fn neutral_items_and_comments_roundtrip_without_descriptors() {
    for body in [
        "outer // before-prefix\n(value) { b() }",
        "outer // before-unit\n(// unit\n) { b() }",
        "outer(value // prefix-tail\n) // before-body\n{ b() }",
        "outer { ;; let .(x: T) <- // rhs\n source; ; x; // tail\n ; }",
        "outer { let .(x: T, _: U) = pair; x }",
        "outer { let local = source; local }",
        "outer { let .(<T> value) = source; value }",
        "outer { ; // empty\n; }",
    ] {
        roundtrip(body, "");
    }
}

#[test]
fn neutral_final_separators_preserve_items_and_canonicalize() {
    fn final_separator(expr: &Expr) -> Option<crate::span::Span> {
        let Expr::BlockCall { blocks, .. } = expr else {
            panic!("block call");
        };
        let Some(NeutralItem::Expression { semicolon, .. }) = blocks.last().unwrap().items.last()
        else {
            panic!("final expression");
        };
        *semicolon
    }

    for (body, has_separator, equivalent) in [
        ("outer { action(); }", true, "outer { action() }"),
        ("outer { action() }", false, "outer { action() }"),
        ("outer { ;; action();; }", true, "outer { action() }"),
        (
            "outer { action() // before-semi\n; }",
            true,
            "outer { action() }",
        ),
        (
            "outer { action(); // after-semi\n }",
            true,
            "outer { action() }",
        ),
        (
            "outer { first(); action(); }",
            true,
            "outer { first(); action() }",
        ),
        (
            "outer { first(); action() }",
            false,
            "outer { first(); action() }",
        ),
        (
            "outer { first() } next { action(); }",
            true,
            "outer { first() } next { action() }",
        ),
    ] {
        let expr = roundtrip(body, "");
        assert_eq!(final_separator(&expr).is_some(), has_separator, "{body}");
        let source = format!("module consumer;  fn subject() {{ {body} }}");
        if let Some(span) = final_separator(&expr) {
            assert_eq!(&source[span.start as usize..span.end as usize], ";");
        }
        let rendered = crate::pretty::pretty_module(&parse(&source).unwrap());
        let reparsed = parse(&rendered).unwrap();
        assert!(final_separator(function(&reparsed)).is_none(), "{rendered}");
        assert_eq!(
            topology(&expr),
            topology(&roundtrip(equivalent, "")),
            "{body}"
        );
    }
}

#[test]
fn malformed_block_buffers_reject_without_inventing_items() {
    for body in [
        "outer(value) {",
        "outer { let x <- }",
        "outer { a() b() }",
        "outer { a() } else",
    ] {
        assert!(
            parse(&format!("module consumer; fn subject() {{ {body} }}")).is_err(),
            "{body}"
        );
    }
    roundtrip("outer { let x <- source; x }", "");
}

#[test]
fn placeholder_ownership_includes_direct_heads_but_excludes_labels() {
    let source =
        "module consumer; fn subject() { .x. { x9 (x3) { let .(x1: T) <- x2; x1 } x8 { x1 } } }";
    let mut parsed = parse(source).unwrap();
    let Item::FnDef(def) = parsed.items.last_mut().unwrap() else {
        panic!("function");
    };
    let Expr::FnPlaceholder {
        stem,
        state,
        body,
        meta,
        ..
    } = &mut def.body
    else {
        panic!("placeholder");
    };
    assert!(matches!(
        state,
        crate::ast::PlaceholderState::Source { slot_count: 9 }
    ));
    let references = crate::pass::placeholder::classify(&stem.name, body, meta.span).unwrap();
    let actual = references
        .occurrences
        .iter()
        .map(|(segment, slot)| {
            (
                *slot,
                &source[segment.span.start as usize..segment.span.end as usize],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, vec![(9, "x9"), (3, "x3"), (2, "x2"), (1, "x1")]);
}

#[test]
fn neutral_binder_completion_is_source_ordered_and_block_local() {
    let source =
        "module consumer; fn subject() { outer { let local = source; local } next { other } }";
    let parsed = parse(source).unwrap();
    let after = source.find("; local").unwrap() as u32 + 3;
    let rhs = source.find("= source").unwrap() as u32 + 3;
    let sibling = source.find("{ other").unwrap() as u32 + 3;
    let names = |offset| {
        crate::scope_walk::in_scope_candidates(&parsed, offset)
            .into_iter()
            .map(|candidate| candidate.label)
            .collect::<Vec<_>>()
    };
    assert!(names(after).iter().any(|name| name == "local"));
    assert!(!names(rhs).iter().any(|name| name == "local"));
    assert!(!names(sibling).iter().any(|name| name == "local"));
}

#[test]
fn neutral_binding_and_prefix_errors_keep_source_spans() {
    for value in ["A", "A[B]", "A[B][C]"] {
        let source = format!("module consumer; fn subject() {{ outer({value}) {{ b() }} }}");
        let error = parse(&source).unwrap_err();
        assert!(matches!(error, crate::error::Error::Parse(_)));
        let diagnostic = error.diagnostic();
        assert_eq!(
            &source[diagnostic.span.start as usize..diagnostic.span.end as usize],
            "A"
        );
        assert_eq!(diagnostic.span.start as usize, source.find(value).unwrap());
        assert!(
            diagnostic
                .message
                .contains("expected a value operand, found a type path")
        );
    }
    let source = "module consumer; fn subject() { outer { let local = source; local } }";
    let parsed = parse(source).unwrap();
    let Expr::BlockCall { head, blocks, .. } = function(&parsed) else {
        panic!("block call");
    };
    let NeutralItem::Binding {
        name,
        name_span,
        value,
        ..
    } = &blocks[0].items[0]
    else {
        panic!("ordinary binding");
    };
    let slice = |span: crate::span::Span| &source[span.start as usize..span.end as usize];
    assert_eq!(slice(head.span), "outer");
    assert_eq!(slice(blocks[0].open), "{");
    assert_eq!(slice(blocks[0].close), "}");
    assert_eq!(name, "local");
    assert_eq!(slice(*name_span), "local");
    assert_eq!(slice(value.span()), "source");
}
