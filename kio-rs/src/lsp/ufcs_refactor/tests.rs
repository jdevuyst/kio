use super::*;
use crate::cmd::check::{
    LspUserMacroMemos, analyze_workspace_at_with_overlay_lsp_cancellable_with_memos,
};

fn fixture(
    source: &str,
    provider: Option<&str>,
) -> (tempfile::TempDir, Module<Surface>, LspAnalysis) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("pkg")).unwrap();
    std::fs::write(dir.path().join("pkg.kio"), "mod pkg;\n").unwrap();
    std::fs::write(
        dir.path().join("pkg.pkg.kio"),
        "pkg pkg; bridge { pkg; pkg/**; }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("pkg/main.kio"), source).unwrap();
    if let Some(provider) = provider {
        std::fs::write(dir.path().join("pkg/provider.kio"), provider).unwrap();
    }
    let parsed = crate::pass::parser::parse(source).unwrap();
    let analysis = analyze_workspace_at_with_overlay_lsp_cancellable_with_memos(
        dir.path(),
        &crate::package_collection::SourceOverlay::empty(),
        &crate::lsp::cancel::CancellationToken::new(),
        &LspUserMacroMemos::for_editor(),
    )
    .unwrap()
    .unwrap();
    (dir, parsed, analysis)
}

fn selected(source: &str, text: &str) -> Range {
    let start = source.rfind(text).unwrap() as u32;
    let range = LineIndex::new(source).to_range(Span::new(start, start + text.len() as u32));
    Range::new(
        lsp_types::Position::new(range.start.line, range.start.character),
        lsp_types::Position::new(range.end.line, range.end.character),
    )
}

#[test]
fn ufcs_refactor_first_last_mixed_and_written_type_arguments() {
    for (generic, body, expected) in [
        (false, "f(f(f((), ()), ()), ())", "().>f(()).>f(()).>f(())"),
        (
            false,
            "f((), f((), f((), ())))",
            "().>>f(()).>>f(()).>>f(())",
        ),
        (
            false,
            "f(f((), f((), ())), ())",
            "().>>f(()).>>f(()).>f(())",
        ),
        (false, "f((), f(f((), ()), ()))", "().>f(()).>f(()).>>f(())"),
        (
            true,
            "f[.](f[.](f[.]((), ()), ()), ())",
            "().>f[.](()).>f[.](()).>f[.](())",
        ),
    ] {
        let definition = if generic {
            "fn f[A](a: A, b: A) -> A { a }"
        } else {
            "fn f(a: ., b: .) -> . { a }"
        };
        let source = format!("mod pkg/main; {definition} pub fn run() -> . {{ {body} }}");
        let (_, parsed, analysis) = fixture(&source, None);
        let replacement = edit(
            &source,
            &parsed,
            &analysis,
            "pkg/main",
            selected(&source, body),
        )
        .expect(body);
        assert_eq!(replacement.new_text, format!("({expected})"));
        let changed = source.replacen(body, &replacement.new_text, 1);
        fixture(&changed, None);
    }
}

#[test]
fn ufcs_refactor_imported_host_identity_matches_qualified_and_selective_sites() {
    let provider = "mod pkg/provider; host type Text role(str); host fn string_concat(a: Text, b: Text) -> Text;";
    let body = "p.string_concat(\"a\", string_concat(\"b\", p.string_concat(\"c\", \"d\")))";
    let source = format!(
        "mod pkg/main; import pkg/provider {{type Text; fn string_concat}} import pkg/provider as p; pub fn run() -> Text {{ {body} }}"
    );
    let (_, parsed, analysis) = fixture(&source, Some(provider));
    let replacement = edit(
        &source,
        &parsed,
        &analysis,
        "pkg/main",
        selected(&source, body),
    )
    .unwrap();
    assert_eq!(
        replacement.new_text,
        "(\"d\".>>p.string_concat(\"c\").>>string_concat(\"b\").>>p.string_concat(\"a\"))"
    );
    fixture(
        &source.replacen(body, &replacement.new_text, 1),
        Some(provider),
    );
}

#[test]
fn ufcs_refactor_unary_and_grouped_comment_ownership() {
    for (definition, body, expected) in [
        ("fn f(a: .) -> . { a }", "f(f(f(())))", "().>f.>f.>f"),
        (
            "fn f(a: ., b: .) -> . { a }",
            "f((f((// owned\n()), ())), ())",
            "((// owned\n()).>f(())).>f(())",
        ),
    ] {
        let source = format!("mod pkg/main; {definition} pub fn run() -> . {{ {body} }}");
        let (_, parsed, analysis) = fixture(&source, None);
        let replacement = edit(
            &source,
            &parsed,
            &analysis,
            "pkg/main",
            selected(&source, body),
        )
        .unwrap();
        assert_eq!(replacement.new_text, format!("({expected})"));
        fixture(&source.replacen(body, &replacement.new_text, 1), None);
    }
}

#[test]
fn ufcs_refactor_withholds_branch_single_call_and_delimiter_comment() {
    for body in [
        "f(f((), ()), f((), ()))",
        "f((), ())",
        "f( // unclear\n f((), ()), ())",
    ] {
        let source =
            format!("mod pkg/main; fn f(a: ., b: .) -> . {{ a }} pub fn run() -> . {{ {body} }}");
        let (_, parsed, analysis) = fixture(&source, None);
        assert!(
            edit(
                &source,
                &parsed,
                &analysis,
                "pkg/main",
                selected(&source, body)
            )
            .is_none(),
            "{body}"
        );
    }
}

#[test]
fn ufcs_refactor_arbitrary_chain_carets_and_explicit_inner_branch() {
    let mut body = "()".to_owned();
    for i in 0..24 {
        body = if i % 2 == 0 {
            format!("f({body}, ())")
        } else {
            format!("f((), {body})")
        };
    }
    let source =
        format!("mod pkg/main; fn f(a: ., b: .) -> . {{ a }} pub fn run() -> . {{ {body} }}");
    let (_, parsed, analysis) = fixture(&source, None);
    let whole = edit(
        &source,
        &parsed,
        &analysis,
        "pkg/main",
        selected(&source, &body),
    )
    .unwrap();
    assert_eq!(whole.new_text.matches(".>").count(), 24);
    for (at, _) in source
        .match_indices("f(")
        .filter(|(at, _)| *at >= source.find(&body).unwrap())
    {
        let position = LineIndex::new(&source).to_position(at as u32);
        let point = lsp_types::Position::new(position.line, position.character);
        let action = edit(
            &source,
            &parsed,
            &analysis,
            "pkg/main",
            Range::new(point, point),
        )
        .unwrap();
        assert_eq!(action, whole);
    }
    fixture(&source.replacen(&body, &whole.new_text, 1), None);

    let branch = "f(f((), ()), ())";
    let body = format!("f({branch}, f((), ()))");
    let source =
        format!("mod pkg/main; fn f(a: ., b: .) -> . {{ a }} pub fn run() -> . {{ {body} }}");
    let (_, parsed, analysis) = fixture(&source, None);
    assert!(
        edit(
            &source,
            &parsed,
            &analysis,
            "pkg/main",
            selected(&source, &body)
        )
        .is_none()
    );
    let action = edit(
        &source,
        &parsed,
        &analysis,
        "pkg/main",
        selected(&source, branch),
    )
    .unwrap();
    assert_eq!(action.new_text, "(().>f(()).>f(()))");
}

#[test]
fn ufcs_refactor_local_generic_and_written_type_stages() {
    for (source, body, expected) in [
        (
            "mod pkg/main; pub fn run[A](f: (A & A) -> A, a: A, b: A, c: A, d: A) -> A { f(f(a, f(b, c)), d) }",
            "f(f(a, f(b, c)), d)",
            "c.>>f(b).>>f(a).>f(d)",
        ),
        (
            "mod pkg/main; fn f[A][B](a: A, b: B) -> A { a } pub fn run() -> . { f[.][.](f[.][.]((), ()), ()) }",
            "f[.][.](f[.][.]((), ()), ())",
            "().>f[.][.](()).>f[.][.](())",
        ),
    ] {
        let (_, parsed, analysis) = fixture(source, None);
        let action = edit(
            source,
            &parsed,
            &analysis,
            "pkg/main",
            selected(source, body),
        )
        .expect(body);
        assert_eq!(action.new_text, format!("({expected})"));
        fixture(&source.replacen(body, &action.new_text, 1), None);
    }
}

#[test]
fn ufcs_refactor_does_not_treat_computed_results_as_the_named_callable() {
    let body = "make(())(make(())((), ()), ())";
    let source =
        format!("mod pkg/main; host fn make(x: .) -> ((. & .) -> .); pub fn run() {{ {body} }}");
    let (_, parsed, analysis) = fixture(&source, None);
    assert!(
        edit(
            &source,
            &parsed,
            &analysis,
            "pkg/main",
            selected(&source, body)
        )
        .is_none()
    );
}

#[test]
fn ufcs_refactor_finds_calls_inside_stages_and_reverse_computed_callees() {
    for source in [
        "mod m; fn run() { factory(f(f(a, b), c))(d).<r }",
        "mod m; macro wrap : . -> . { impl implementation } fn run() { wrap(f(f(a, b), c))(d) }",
        "mod m; macro wrap : . -> . { trailing product; impl implementation } fn run() { wrap(f(f(a, b), c))(d) { () } }",
    ] {
        let parsed = crate::pass::parser::parse(source).unwrap();
        let written = calls(&parsed);
        for expected in ["f(a, b)", "f(f(a, b), c)"] {
            assert!(
                written.iter().any(|call| {
                    &source[call.span().start as usize..call.span().end as usize] == expected
                }),
                "{expected} in {source}"
            );
        }
    }
}

#[test]
fn ufcs_refactor_conflict_and_incomplete_never_return_shorter_prefix() {
    let body = "f(f(f((), ()), ()), ())";
    let source =
        format!("mod pkg/main; fn f(a: ., b: .) -> . {{ a }} pub fn run() -> . {{ {body} }}");
    let (_, parsed, mut analysis) = fixture(&source, None);
    let pristine = analysis.position_index.clone();
    let original = selected(&source, body);
    assert!(edit(&source, &parsed, &analysis, "pkg/main", original).is_some());
    let root = calls(&parsed)
        .into_iter()
        .max_by_key(|call| call.span().end - call.span().start)
        .unwrap();
    for damage in 0..3 {
        analysis.position_index = pristine.clone();
        match damage {
            0 => analysis.position_index.merge(PositionIndex::new()),
            1 | 2 => {
                let (callee, _) = parts(root).unwrap();
                analysis.position_index.record_binder(
                    "pkg/main",
                    callee.span(),
                    ResolvedBinder::Local {
                        name: "f".into(),
                        decl_span: (damage == 2).then_some(Span::new(0, 1)),
                    },
                );
            }
            _ => unreachable!(),
        }
        let inner = selected(&source, "f((), ())").start;
        assert!(
            edit(
                &source,
                &parsed,
                &analysis,
                "pkg/main",
                Range::new(inner, inner)
            )
            .is_none()
        );
    }
}

#[test]
fn ufcs_refactor_multiline_separators_unicode_and_opaque_operands() {
    let body = "f(\r\n, f(\r\n, (// 🦀 owned\r\n())\r\n, (())\r\n)\r\n, ()\r\n)";
    let source =
        format!("mod pkg/main; fn f(a: ., b: .) -> . {{ a }} pub fn run() -> . {{ {body} }}");
    let (_, parsed, analysis) = fixture(&source, None);
    let action = edit(
        &source,
        &parsed,
        &analysis,
        "pkg/main",
        selected(&source, body),
    )
    .unwrap();
    assert!(action.new_text.contains("// 🦀 owned\r\n"));
    assert!(action.new_text.contains("(())"));
    fixture(&source.replacen(body, &action.new_text, 1), None);
}

#[test]
fn ufcs_refactor_request_scaling() {
    for (count, background) in [(8, 0), (24, 0), (48, 0), (24, 256)] {
        let mut body = "()".to_owned();
        for i in 0..count {
            body = if i % 2 == 0 {
                format!("f({body}, ())")
            } else {
                format!("f((), {body})")
            };
        }
        let mut source =
            format!("mod pkg/main; fn f(a: ., _b: .) -> . {{ a }} pub fn run() -> . {{ {body} }}");
        for i in 0..background {
            source.push_str(&format!(" pub fn extra{i}() -> . {{ f(f((), ()), ()) }}"));
        }
        let (_, parsed, analysis) = fixture(&source, None);
        let ordinary_calls = calls(&parsed).len();
        let selection = selected(&source, &body);
        let mut times = Vec::new();
        for _ in 0..32 {
            let start = std::time::Instant::now();
            let action = edit(&source, &parsed, &analysis, "pkg/main", selection).unwrap();
            times.push(start.elapsed());
            assert_eq!(action.new_text.matches(".>").count(), count);
        }
        times.sort();
        eprintln!(
            "ufcs request: selected_calls={count} module_calls={ordinary_calls} module_items={} source_bytes={} samples={} min={:?} median={:?} max={:?}",
            parsed.items.len(),
            source.len(),
            times.len(),
            times[0],
            times[16],
            times[31]
        );
    }
}

#[test]
fn ufcs_refactor_runtime_golden_uses_the_action_output() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../test-data/goldens/00_success/exec_ufcs_refactor_effects/workdir");
    let source = std::fs::read_to_string(root.join("testapi/main.kio")).unwrap();
    let module = crate::pass::parser::parse(&source).unwrap();
    let analysis = analyze_workspace_at_with_overlay_lsp_cancellable_with_memos(
        &root,
        &crate::package_collection::SourceOverlay::empty(),
        &crate::lsp::cancel::CancellationToken::new(),
        &LspUserMacroMemos::for_editor(),
    )
    .unwrap()
    .unwrap();
    for direction in [
        "first",
        "last",
        "mixed",
        "packet first",
        "packet last",
        "packet mixed",
    ] {
        let prefix = source
            .split(&format!("line(\"{direction} prefix\");"))
            .nth(1)
            .unwrap()
            .lines()
            .nth(1)
            .unwrap()
            .trim();
        let prefix = prefix
            .strip_prefix("line(")
            .unwrap()
            .strip_suffix(");")
            .unwrap();
        let action = edit(
            &source,
            &module,
            &analysis,
            "testapi/main",
            selected(&source, prefix),
        );
        if !direction.starts_with("packet ") {
            assert!(action.is_none(), "curried stages are outside the refactor");
            continue;
        }
        let action = action.unwrap();
        // The formatter removes redundant receiver parentheses in the golden;
        // canonical formatting otherwise preserves the exact action result.
        let rendered = action.new_text[1..action.new_text.len() - 1]
            .replacen("(mark(", "mark(", 1)
            .replacen(")).>", ").>", 1);
        assert!(
            source.contains(&format!("line({rendered})")),
            "{direction}: {}",
            action.new_text
        );
    }
}

#[test]
fn ufcs_refactor_preserves_enclosing_tight_ufcs_context() {
    for operator in [".<", ".<<"] {
        let body = "f(f((), ()), ())";
        let source = format!(
            "mod pkg/main; fn f(a: ., _b: .) -> . {{ a }} fn g(a: .) -> . {{ a }} pub fn run() -> . {{ g{operator}{body} }}"
        );
        let (_, module, analysis) = fixture(&source, None);
        let action = edit(
            &source,
            &module,
            &analysis,
            "pkg/main",
            selected(&source, body),
        )
        .unwrap();
        let changed = source.replacen(body, &action.new_text, 1);
        let (_, parsed, _) = fixture(&changed, None);
        let Item::FnDef(run) = parsed.items.last().unwrap() else {
            panic!("run")
        };
        let Expr::Ufcs {
            receiver,
            callee,
            flavor,
            ..
        } = &run.body
        else {
            panic!("outer g must remain a splice")
        };
        let Expr::Path { segments, .. } = &**callee else {
            panic!("g callee")
        };
        assert_eq!(
            segments.last().unwrap().name,
            "g",
            "edited enclosing expression: {changed}"
        );
        assert_eq!(flavor.token(), operator);
        let Expr::Ufcs {
            receiver, callee, ..
        } = &**receiver
        else {
            panic!("g receiver must be entire f spine")
        };
        let Expr::Path { segments, .. } = &**callee else {
            panic!("f callee")
        };
        assert_eq!(segments.last().unwrap().name, "f");
        assert!(matches!(&**receiver, Expr::Ufcs { .. }));
    }
}

#[test]
fn ufcs_refactor_distinct_same_spelled_identity_full_and_focused() {
    let provider = "mod pkg/provider; pub fn f(a: ., _b: .) -> . { a }";
    let different = "f(p.f((), ()), ())";
    let same = "p.f(p.f((), ()), ())";
    let source = format!(
        "mod pkg/main; import pkg/provider as p; fn f(a: ., _b: .) -> . {{ a }} pub fn run() -> . {{ {different} }} pub fn matching() -> . {{ {same} }}"
    );
    let (dir, module, full) = fixture(&source, Some(provider));
    let focused = crate::cmd::check::analyze_module_at_with_overlay_lsp_cancellable_with_memos(
        dir.path(),
        &crate::package_collection::SourceOverlay::empty(),
        &dir.path().join("pkg/main.kio"),
        &crate::lsp::cancel::CancellationToken::new(),
        &LspUserMacroMemos::for_editor(),
    )
    .unwrap()
    .unwrap();
    for analysis in [&full, &focused] {
        assert!(
            edit(
                &source,
                &module,
                analysis,
                "pkg/main",
                selected(&source, different)
            )
            .is_none()
        );
        let action = edit(
            &source,
            &module,
            analysis,
            "pkg/main",
            selected(&source, same),
        )
        .unwrap();
        assert_eq!(action.new_text, "(().>p.f(()).>p.f(()))");
    }
}

#[test]
fn ufcs_refactor_uncertain_sibling_cannot_truncate_caret_chain() {
    let inner = "f(f((), ()), ())";
    let whole = format!("f(g((), ()), {inner})");
    let source = format!(
        "mod pkg/main; fn f(a: ., _b: .) -> . {{ a }} fn g(a: ., _b: .) -> . {{ a }} pub fn run() -> . {{ {whole} }}"
    );
    let (_, module, mut analysis) = fixture(&source, None);
    let point = selected(&source, inner).start;
    let selected = Range::new(point, point);
    let intact = edit(&source, &module, &analysis, "pkg/main", selected).unwrap();
    assert_eq!(intact.new_text.matches(".>").count(), 3);
    let start = source.rfind("g(").unwrap() as u32;
    analysis.position_index.record_binder(
        "pkg/main",
        Span::new(start, start + 1),
        ResolvedBinder::Local {
            name: "g".into(),
            decl_span: Some(Span::new(0, 1)),
        },
    );
    assert!(edit(&source, &module, &analysis, "pkg/main", selected).is_none());
}
