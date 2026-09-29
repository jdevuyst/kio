use super::{parse, parse_lazy};
use crate::ast::{BlockExposure, Expr, Item, NeutralItem};

#[test]
fn block_cursor_regions_survive_incomplete_bodies_without_descriptors() {
    use super::BlockCursorRegion;
    for (body, expected_head, expected_region) in [
        ("outer! val|ue { () }", "outer", BlockCursorRegion::Prefix),
        ("outer! { val|ue }", "outer", BlockCursorRegion::Body(0)),
        (
            "outer! { () } fa|llback { () }",
            "outer",
            BlockCursorRegion::Label(1),
        ),
        ("outer! { () } |", "outer", BlockCursorRegion::Label(1)),
        (
            "outer! { () } fallback { val|",
            "outer",
            BlockCursorRegion::Body(1),
        ),
        (
            "outer! { inner! { val|",
            "inner",
            BlockCursorRegion::Body(0),
        ),
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

fn function(module: &crate::ast::Module) -> &Expr {
    let Item::FnDef(def) = module.items.last().expect("function") else {
        panic!("expected final function");
    };
    &def.body
}

fn topology(expr: &Expr) -> serde_json::Value {
    fn normalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                map.retain(|key, _| {
                    !key.ends_with("_span")
                        && !matches!(
                            key.as_str(),
                            "meta"
                                | "span"
                                | "open"
                                | "close"
                                | "separators"
                                | "leading"
                                | "trailing"
                                | "id"
                                | "ext"
                                | "access_ext"
                                | "prefix_explicit"
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
        let body = format!("{head}! value {{ first() }} next {{ last() }}");
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
        "if condition { yes() } else { no() }",
        "do { result() }",
        "do receiver { let value <- source(); value }",
    ] {
        assert!(
            parse(&format!("module consumer; fn subject() {{ {body} }}")).is_err(),
            "{body}"
        );
    }
}

#[test]
fn boolean_block_prefixes_use_the_existing_leading_dot_literal() {
    for (body, expected) in [
        ("if! .t { yes() } else { no() }", true),
        ("if! .f { yes() } else { no() }", false),
        ("choose! .t(Bool) { yes() } fallback { no() }", true),
        (
            "choose! // head\n .f // prefix\n { yes() } fallback { no() }",
            false,
        ),
    ] {
        let expr = roundtrip(body, "");
        let Expr::BlockCall { prefix, blocks, .. } = expr else {
            panic!("expected a block call");
        };
        assert!(matches!(prefix.as_slice(), [Expr::BoolLit { value, .. }] if *value == expected));
        assert_eq!(blocks.len(), 2);
    }
    roundtrip("outer!(inner! .t { yes() } else { no() }) { body() }", "");
    roundtrip("outer!(.t. { t1 }) { body() }", "");
    roundtrip("outer!(.() { () }) { body() }", "");
    for body in [
        "outer! .t. { t1 } { body() }",
        "outer! .() { () } { body() }",
        "outer! inner! .t { yes() } { body() }",
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
        ("outer!(value) { b() }", "outer! value {"),
        ("outer!(f(A)(value)) { b() }", "outer! f(A)(value) {"),
        (
            "outer!(A.member(value)) { b() }",
            "outer! A.member(value) {",
        ),
        (
            "outer!(convert!(value)) { b() }",
            "outer! convert!(value) {",
        ),
        ("outer!(-1) { b() }", "outer! -1 {"),
        ("outer!(-1.5) { b() }", "outer! -1.5 {"),
        ("outer!(()) { b() }", "outer!() {"),
        ("outer! { b() }", "outer! {"),
        ("outer!(left, right) { b() }", "outer!(left, right) {"),
        ("outer!((left, right)) { b() }", "outer!((left, right)) {"),
        (
            "outer!({ field = value }) { b() }",
            "outer!({field = value}) {",
        ),
    ] {
        roundtrip(body, "");
        let parsed = parse(&format!("module consumer; fn subject() {{ {body} }}")).unwrap();
        let rendered = crate::pretty::pretty_module(&parsed);
        assert!(rendered.contains(head), "{head}: {rendered}");
    }
    for slot in ["_", "__", "___"] {
        let operators = format!("import absent(op _ + {slot}, op - _, op _ <| _ |>);");
        for body in [
            "outer!(left + right) { b() }",
            "outer! left + (inner! value { a() }) { b() }",
            "outer!(inner! value { a() } + x) { b() }",
            "outer! left + f(inner! value { a() }) { b() }",
            "outer! left <| inner! value { a() } |> { b() }",
            "outer!(- value) { b() }",
        ] {
            roundtrip(body, &operators);
        }
        let parsed = parse(&format!(
            "module consumer; {operators} fn subject() {{ outer!(left + right) {{ b() }} }}"
        ))
        .unwrap();
        assert!(crate::pretty::pretty_module(&parsed).contains("outer! left + right {"));
    }
    for body in [
        "outer! // prefix\n (value) { b() }",
        "outer! // prefix\n value { b() }",
        "outer! // prefix\n f(A)(value) { b() }",
        "outer! // prefix\n convert!(value) { b() }",
        "outer! // prefix\n -1.5 { b() }",
        "outer!(value // tail\n) { b() }",
        "outer! // unit\n () { b() }",
        "outer!(// unit contents\n) { b() }",
    ] {
        roundtrip(body, "");
    }
}

#[test]
fn final_label_elision_is_structural_and_comment_preserving() {
    let body = "outer! value { a() } fallback { inner! next { b() } }";
    roundtrip(body, "");
    let parsed = parse(&format!("module consumer; fn subject() {{ {body} }}")).unwrap();
    let rendered = crate::pretty::pretty_module(&parsed);
    assert!(rendered.contains("} fallback inner! next {"), "{rendered}");
    let reparsed = parse(&rendered).unwrap();
    let Expr::BlockCall { blocks, .. } = function(&reparsed) else {
        panic!("outer block");
    };
    assert_eq!(blocks.len(), 2);
    assert!(blocks[1].elided);
    for body in [
        "outer! value { a() } fallback { inner! next { b() } } cleanup { c() }",
        "outer! value { a() } fallback { // wrapper\n inner! next { b() } }",
        "outer! value { a() } fallback { inner! next { b() } // wrapper\n }",
        "outer! value { a() } fallback { before(); inner! next { b() } }",
        "outer! value { a() } fallback { let x = next; inner! x { b() } }",
        "outer! value { a() } fallback // child\n inner! next { b() }",
    ] {
        roundtrip(body, "");
        let parsed = parse(&format!("module consumer; fn subject() {{ {body} }}")).unwrap();
        let rendered = crate::pretty::pretty_module(&parsed);
        assert!(rendered.contains("fallback {"), "{rendered}");
    }
    for body in [
        "outer! { // child\n inner! value { a() } }",
        "outer! { let value = // child\n inner! x { a() }; value }",
        "outer!(// child\n inner! x { a() }) { b() }",
        "consume(// child\n inner! x { a() })",
    ] {
        roundtrip(body, "");
    }
}

#[test]
fn descriptors_keep_relative_order_in_eager_and_lazy_headers() {
    let source = "module consumer; elab choose: . -> . { impl impl_choose; trailing thunk; captures captured; trailing thunk else; trailing product finally }";
    let parsed = parse(source).unwrap();
    let lazy = parse_lazy(source).unwrap();
    let Item::Elaborator(eager, _) = &parsed.items[0] else {
        panic!("elaborator");
    };
    let Item::Elaborator(deferred, _) = &lazy.module().items[0] else {
        panic!("elaborator");
    };
    let descriptors = |def: &crate::ast::UserElaboratorDef| {
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
    let ordinary = roundtrip("outer! convert!(value) { b() }", "");
    let Expr::BlockCall { prefix, blocks, .. } = ordinary else {
        panic!("outer block");
    };
    assert!(matches!(prefix.as_slice(), [Expr::UserElaborator { .. }]));
    assert_eq!(blocks.len(), 1);
    let nested = roundtrip("outer!(convert!(value) { a() }) { b() }", "");
    let Expr::BlockCall { prefix, .. } = nested else {
        panic!("outer block");
    };
    assert!(matches!(prefix.as_slice(), [Expr::BlockCall { .. }]));
    let elided = roundtrip(
        "choose! condition { first() } else choose! next { second() } else { third() }",
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
            "outer! left + value { b() }",
            "outer! left + (inner! value { a() }) { b() }",
            "outer! left + <| inner! value { a() } |> { b() }",
            "outer! left + value { b() } + { field = later }",
        ] {
            let expr = roundtrip(body, &operators);
            if body.ends_with("{ b() }") {
                assert!(matches!(expr, Expr::BlockCall { .. }), "{body}");
            }
        }
        let source = format!(
            "module consumer; {operators} fn subject() {{ outer! left + inner! value {{ a() }} {{ b() }} }}"
        );
        assert!(parse(&source).is_err(), "{slot}");
    }
}

#[test]
fn block_prefix_role_follows_only_the_callee_spine() {
    for value in ["A", "A(B)", "A(B)(C)"] {
        let source = format!("module consumer; fn subject() {{ outer!({value}) {{ b() }} }}");
        assert!(parse(&source).is_err(), "{value}");
    }
    for value in ["f(A)", "f(A)(value)", "A.member(B)(value)", "-1", "-1.5"] {
        roundtrip(&format!("outer! {value} {{ b() }}"), "");
    }
    roundtrip("outer!(- value) { b() }", "import absent(op - _);");
    assert!(
        parse("module consumer; import absent(op - _); fn subject() { outer! - value { b() } }")
            .is_err()
    );
}

#[test]
fn neutral_items_and_comments_roundtrip_without_descriptors() {
    for body in [
        "outer! // before-prefix\n(value) { b() }",
        "outer! // before-unit\n(// unit\n) { b() }",
        "outer!(value // prefix-tail\n) // before-body\n{ b() }",
        "outer! { ;; let .(x: T) <- // rhs\n source; ; x; // tail\n ; }",
        "outer! { let .(x: T, _: U) = pair; x }",
        "outer! { let .({field as local}) = source; local }",
        "outer! { let .(<T> value) = source; value }",
        "outer! { ; // empty\n; }",
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
        ("outer! { action(); }", true, "outer! { action() }"),
        ("outer! { action() }", false, "outer! { action() }"),
        ("outer! { ;; action();; }", true, "outer! { action() }"),
        (
            "outer! { action() // before-semi\n; }",
            true,
            "outer! { action() }",
        ),
        (
            "outer! { action(); // after-semi\n }",
            true,
            "outer! { action() }",
        ),
        (
            "outer! { first(); action(); }",
            true,
            "outer! { first(); action() }",
        ),
        (
            "outer! { first(); action() }",
            false,
            "outer! { first(); action() }",
        ),
        (
            "outer! { first() } next { action(); }",
            true,
            "outer! { first() } next { action() }",
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
        "outer! value {",
        "outer! { let x <- }",
        "outer! { a() b() }",
        "outer! { a() } else",
    ] {
        assert!(
            parse(&format!("module consumer; fn subject() {{ {body} }}")).is_err(),
            "{body}"
        );
    }
    roundtrip("outer! { let x <- source; x }", "");
}

#[test]
fn placeholder_ownership_uses_neutral_binders_not_heads_or_labels() {
    let source =
        "module consumer; fn subject() { .x. { x9! x3 { let .(x1: T) <- x2; x1 } x8 { x1 } } }";
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
        crate::ast::PlaceholderState::Source { slot_count: 3 }
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
    assert_eq!(actual, vec![(3, "x3"), (2, "x2"), (1, "x1")]);
}

#[test]
fn neutral_binder_completion_is_source_ordered_and_block_local() {
    let source = "module consumer; fn subject() { outer! { let .({field as local}) = source; local } next { other } }";
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
    for value in ["A", "A(B)", "A(B)(C)"] {
        let source = format!("module consumer; fn subject() {{ outer!({value}) {{ b() }} }}");
        let error = parse(&source).unwrap_err();
        assert!(matches!(error, crate::error::Error::Parse(_)));
        let diagnostic = error.diagnostic();
        assert_eq!(
            &source[diagnostic.span.start as usize..diagnostic.span.end as usize],
            value
        );
        assert!(
            diagnostic
                .message
                .contains("block prefix requires a value argument")
        );
    }
    let source =
        "module consumer; fn subject() { outer! { let .({field as local}) = source; local } }";
    let parsed = parse(source).unwrap();
    let Expr::BlockCall { head, blocks, .. } = function(&parsed) else {
        panic!("block call");
    };
    let NeutralItem::RowBinding { entries, value, .. } = &blocks[0].items[0] else {
        panic!("row binding");
    };
    let slice = |span: crate::span::Span| &source[span.start as usize..span.end as usize];
    assert_eq!(slice(head.span), "outer");
    assert_eq!(slice(blocks[0].open), "{");
    assert_eq!(slice(blocks[0].close), "}");
    assert_eq!(slice(entries[0].label_span), "field");
    assert_eq!(slice(entries[0].local_span), "local");
    assert_eq!(slice(value.span()), "source");
}
