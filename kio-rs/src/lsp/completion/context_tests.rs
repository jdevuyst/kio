use super::*;
use crate::lsp::util::test_file_uri;

fn complete_marked(kind: &str, marked: &str) -> (String, CompletionList) {
    let cursor = marked.find('|').unwrap();
    let source = marked.replacen('|', "", 1);
    let uri = test_file_uri(format!("/app{kind}.kio"));
    let index = LineIndex::new(&source);
    let position = index.to_position(cursor as u32);
    let result = handle_completion(
        &uri,
        &Position::new(position.line, position.character),
        Some(&source),
        None,
    )
    .expect("Kio-family completion response");
    (source, result)
}

#[test]
fn completed_import_selector_keeps_alias_choice_at_recovered_close() {
    for selector in [
        "type Entry",
        "fn value",
        "macro choose",
        "op _ + _",
        "op_var [* *]",
    ] {
        for close in ["", "}"] {
            let marked = format!("mod app; import dep/api {{{selector} |{close}");
            let (_, result) = complete_marked("", &marked);
            assert!(!result.is_incomplete, "{marked}: {result:?}");
            assert_eq!(
                result
                    .items
                    .iter()
                    .map(|item| item.label.as_str())
                    .collect::<Vec<_>>(),
                vec!["as"],
                "{marked}"
            );
        }
    }
    for marked in [
        "mod app; import dep/api {|",
        "mod app; import dep/api {type Entry; |",
    ] {
        let (_, result) = complete_marked("", marked);
        assert!(
            result.is_incomplete,
            "missing provider remains incomplete: {marked}"
        );
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["fn", "type", "lit", "macro", "op", "op_macro", "op_var"],
            "{marked}: {result:?}"
        );
    }
    let (_, result) = complete_marked("", "mod app; import dep/api {type Entry as |");
    assert!(!result.is_incomplete);
    assert!(
        result.items.is_empty(),
        "alias destination remains a binder"
    );
}

#[test]
fn parser_context_file_identity_precedes_comment_and_binder_suppression() {
    for (kind, header, marked) in [
        (
            ".pkg",
            "pkg app;",
            "build { target js { baseline |latest } }",
        ),
        (".dep", "dep app;", "source { |path \"app.pkg.kio\"; }"),
        (
            ".lock",
            "lock app;",
            "resolved { |git \"url\"; ref \"main\"; commit \"sha\"; sig \"digest\"; }",
        ),
        (
            ".sig",
            "sig app v(1);",
            "|v(1) { nonbreaking { add { mod main { pub type A = .; } } } }",
        ),
        ("", "mod app;", "|fn run() { () }"),
    ] {
        for marked in [
            format!("{header} {marked}"),
            format!("{header} {}", marked.split('}').next().unwrap()),
            format!("{header} // co|mment"),
            format!("{header} /// do|cumentation"),
            header.replacen("app", "ap|p", 1),
        ] {
            let cursor = marked.find('|').unwrap();
            let source = marked.replacen('|', "", 1);
            let position = LineIndex::new(&source).to_position(cursor as u32);
            let position = Position::new(position.line, position.character);
            for (uri, matches) in [
                (test_file_uri(format!("/app{kind}.kio")), true),
                (test_file_uri(format!("/other{kind}.kio")), false),
                (format!("untitled:other{kind}.kio").parse().unwrap(), true),
            ] {
                assert_eq!(
                    handle_completion(&uri, &position, Some(&source), None).is_some(),
                    matches,
                    "{uri:?}: {marked}"
                );
            }
        }
        let prefix = header.trim_end_matches(';');
        let result = handle_completion(
            &test_file_uri(format!("/other{kind}.kio")),
            &Position::new(0, prefix.len() as u32),
            Some(prefix),
            None,
        );
        assert!(result.is_some(), "unestablished header: {prefix}");
    }
}

#[test]
fn parser_context_recursive_nominal_payload_recovery_keeps_type_scope() {
    for (marked, expected, excluded) in [
        (
            "mod app; import pkg/dep {type Imported} rec type { List[A] <U> : Pair[A][bogus[B]: |",
            vec!["A", "U", "List", "Imported"],
            vec!["B", "Bogus"],
        ),
        (
            "mod app; import pkg/dep {type Imported} rec type Tree[A] = { List[B] <U> : Pair[B][bogus[C]: |",
            vec!["B", "U", "Tree", "List", "Imported"],
            vec!["A", "C", "Bogus"],
        ),
        (
            "mod app; rec type { First[A]: Pair[A][bogus[B]: ]; Second[C]: |",
            vec!["C", "First", "Second"],
            vec!["A", "B", "Bogus"],
        ),
        (
            "mod app; import pkg/dep {type Pair} type Tree[A] = { List[B]: B; } & Pair[|",
            vec!["A", "List", "Pair"],
            vec!["B", "Tree"],
        ),
    ] {
        let (_, result) = complete_marked("", marked);
        let labels: Vec<_> = result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        for expected in expected {
            assert!(
                labels.contains(&expected),
                "{marked}: missing {expected}: {labels:?}"
            );
        }
        for excluded in excluded {
            assert!(
                !labels.contains(&excluded),
                "{marked}: leaked {excluded}: {labels:?}"
            );
        }
    }
    for marked in [
        "mod app; rec type { Malformed[A B]: |",
        "mod app; rec type { List[A] <U> : Pair[A][bogus[B]: // A|",
        "mod app; rec type { List[A] <U> : Pair[A][bogus[B]: \"A|",
        "mod app; rec type { First[A]: Pair[A][bogus[B]: ]; Se|cond[C]: . };",
    ] {
        assert!(complete_marked("", marked).1.items.is_empty(), "{marked}");
    }
}

#[test]
fn parser_context_closed_vocabularies_cover_file_kinds_and_retract_fields() {
    for (kind, source, expected) in [
        ("", "|", vec!["mod"]),
        (".pkg", "|", vec!["pkg"]),
        (".dep", "|", vec!["dep"]),
        (".lock", "|", vec!["lock"]),
        (".sig", "|", vec!["sig"]),
        (
            ".pkg",
            "pkg app; build { cache (); |",
            vec!["docs", "target"],
        ),
        (
            ".pkg",
            "pkg app; build { target rust {} |",
            vec!["cache", "docs", "target"],
        ),
        (".dep", "dep app; source { |", vec!["git", "path", "ref"]),
        (
            ".lock",
            "lock app; resolved { |",
            vec!["commit", "git", "path", "ref", "sig"],
        ),
        (
            ".sig",
            "sig app v(1); v(1) { |",
            vec!["breaking", "nonbreaking", "with"],
        ),
        ("", "mod app; type { A: . { mk; |", vec!["get", "pub"]),
    ] {
        let (_, result) = complete_marked(kind, source);
        let mut labels: Vec<_> = result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        labels.sort_unstable();
        assert_eq!(labels, expected, "{source}");
        assert!(
            !result.is_incomplete,
            "closed vocabulary is complete: {source}"
        );
    }
}

#[test]
fn parser_context_required_named_continuations_remain_available() {
    let mut failures = Vec::new();
    for (kind, source, expected) in [
        ("", "mod app; host |", vec!["fn", "pub", "type"]),
        ("", "mod app; macro make: . { impl(|", vec!["fills"]),
        (
            ".dep",
            "dep app; source { path \"app.pkg.kio\"; } rehost dep/api |",
            vec!["to"],
        ),
        (
            ".dep",
            "dep app; source { path \"app.pkg.kio\"; } retype dep/api.Type |",
            vec!["to"],
        ),
    ] {
        let (_, result) = complete_marked(kind, source);
        let mut labels: Vec<_> = result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        labels.sort_unstable();
        if labels != expected || result.is_incomplete {
            failures.push((
                source,
                labels.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn parser_context_argument_keywords_require_the_recursive_owner() {
    for source in [
        "mod app; fn target(x: .) { x } fn run() { target(|",
        "mod app; fn target(x: .) { x } rec(group) fn run(x: .) { .(y: .) { target(|",
    ] {
        let (_, result) = complete_marked("", source);
        assert!(
            result.items.iter().all(|item| item.label != "rec"),
            "{source}: {:?}",
            result.items
        );
    }
}

#[test]
#[cfg(feature = "repl-core")]
fn parser_context_argument_head_requires_current_public_type() {
    let root = crate::lsp::util::test_file_path("/kio-lsp-tests/completion-call-types");
    let mut failures = Vec::new();
    for callee in ["identity", "choose"] {
        let source = format!(
            "mod app/main; type Item = .; fn identity(value: .) -> . {{ value }} fn choose[A](value: A) -> A {{ value }} fn run(value: .) -> . {{ {callee}(value) }}"
        );
        let file = root.join("app/main.kio");
        let files = std::collections::BTreeMap::from([
            (
                root.join("app.pkg.kio"),
                "pkg app; bridge { app/**; }".to_owned(),
            ),
            (file.clone(), source.clone()),
        ]);
        let mut session = crate::repl_core::session::Session::new_in_memory(root.clone(), files);
        let loaded = crate::repl_core::commands::Command::Load("app/main".to_owned())
            .run(&mut session, crate::repl_core::highlight::Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );
        let uri = test_file_uri(&file);
        let cursor = source.rfind(&format!("{callee}(value)")).unwrap() + callee.len() + 1;
        let position = LineIndex::new(&source).to_position(cursor as u32);
        for fresh in [true, false] {
            call_types::INDEX_LOOKUPS.with(|count| count.set(0));
            let text = if fresh {
                source.clone()
            } else {
                format!("{source} ")
            };
            let result = handle_completion(
                &uri,
                &Position::new(position.line, position.character),
                Some(&text),
                session.analysis(),
            )
            .unwrap();
            let types = result.items.iter().any(|item| item.label == "Item");
            let expected_types = !fresh || callee == "choose";
            assert_eq!(
                call_types::INDEX_LOOKUPS.with(|count| count.get()),
                usize::from(fresh)
            );
            if types != expected_types || result.is_incomplete == fresh {
                failures.push(format!(
                    "{callee} fresh={fresh}: Item={types}, incomplete={}",
                    result.is_incomplete
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn parser_context_target_ids_exclude_other_written_targets() {
    let mut failures = Vec::new();
    for (source, expected) in [
        (
            "pkg app; build { target rust {} target |",
            &[
                "go",
                "haskell",
                "java",
                "js",
                "kio-prime",
                "python",
                "swift",
                "ts",
            ][..],
        ),
        (
            "pkg app; build { target ru|st {} }",
            &[
                "go",
                "haskell",
                "java",
                "js",
                "kio-prime",
                "python",
                "rust",
                "swift",
                "ts",
            ][..],
        ),
        (
            "pkg app; build { target | {} target rust {} }",
            &[
                "go",
                "haskell",
                "java",
                "js",
                "kio-prime",
                "python",
                "swift",
                "ts",
            ][..],
        ),
    ] {
        let (_, result) = complete_marked(".pkg", source);
        let mut labels: Vec<_> = result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        labels.sort_unstable();
        if labels != expected || result.is_incomplete {
            failures.push(format!("{source}: {labels:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn named_target_completion_follows_backend_not_name() {
    for marked in [
        "pkg app; build { target rust {} target (|",
        "pkg app; build { target (rust, \"native\") {} target (|",
        "pkg app; build { target (ru|st, \"native\") {} }",
    ] {
        let (_, result) = complete_marked(".pkg", marked);
        let mut labels: Vec<_> = result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        labels.sort_unstable();
        assert_eq!(
            labels,
            [
                "go",
                "haskell",
                "java",
                "js",
                "kio-prime",
                "python",
                "rust",
                "swift",
                "ts"
            ],
            "{marked}"
        );
        assert!(!result.is_incomplete, "{marked}");
    }
    for (marked, expected) in [
        (
            "pkg app; build { target (rust, \"web\") { | } }",
            vec!["baseline", "namespace", "out", "thread_safety"],
        ),
        (
            "pkg app; build { target (js, \"rust\") { | } }",
            vec!["baseline", "namespace", "out"],
        ),
        (
            "pkg app; build { target (rust, \"native\") { out \"out\"; | } }",
            vec!["baseline", "namespace", "thread_safety"],
        ),
    ] {
        let (_, result) = complete_marked(".pkg", marked);
        let mut labels: Vec<_> = result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        labels.sort_unstable();
        assert_eq!(labels, expected, "{marked}");
        assert!(!result.is_incomplete, "{marked}");
    }
    for marked in [
        "pkg app; build { target (js, |",
        "pkg app; build { target (js, \"we|b\") {} }",
    ] {
        let (_, result) = complete_marked(".pkg", marked);
        assert!(result.items.is_empty(), "{marked}: {result:?}");
    }
    let (_, result) = complete_marked(
        ".pkg",
        "pkg app; build { target (js, \"web\") {} target | }",
    );
    assert!(result.items.iter().any(|item| item.label == "js"));
    let (_, result) = complete_marked(
        ".pkg",
        "pkg app; build { target (js, \"rust\") {} target | }",
    );
    assert!(!result.items.iter().any(|item| item.label == "rust"));
}

#[test]
fn parser_context_rec_annotations_require_an_eligible_structural_owner() {
    let mut invalid = Vec::new();
    for (marked, expected) in [
        ("mod app; fn run() { recur(|", vec![]),
        (
            "mod app; rec(drive) fn run(x: .) -> . { .(y: .) { recur(|",
            vec!["escape"],
        ),
    ] {
        let (_, result) = complete_marked("", marked);
        let labels = result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        if labels != expected {
            invalid.push((marked, result.items));
        }
    }
    let (_, result) = complete_marked("", "mod app; rec(drive) fn run(x: .) -> . { recur(|");
    if result.items.iter().any(|item| item.label == "poly") {
        invalid.push(("monomorphic group", result.items));
    }
    assert!(invalid.is_empty(), "{invalid:?}");
}

#[test]
fn parser_context_qualified_nominals_keep_scope_order_and_identity_aliases() {
    for head in ["Box", "Alias"] {
        let (_, result) = complete_marked(
            "",
            &format!(
                "mod app; type {{ Box[A]: A {{ mk; get; }}; }}; type Alias[A] = Box[A]; fn run() {{ {head}.|"
            ),
        );
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["get", "mk"]
        );
    }
    let (_, result) = complete_marked(
        "",
        "mod app; fn run() { Later.| } type { Later: . { mk; get; }; };",
    );
    assert!(
        result.items.is_empty(),
        "later nominal is not in scope: {:?}",
        result.items
    );
    let (_, result) = complete_marked(
        "",
        "mod app; type Record = { Field: .; }; fn run() { Record.|",
    );
    assert_eq!(
        result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        ["get", "mk"]
    );
}

#[test]
fn parser_context_suppresses_literals_comments_and_binder_introductions() {
    for source in [
        "mod app; fn earlier() { () } fn run() { \"lit|eral\" }",
        "mod app; fn earlier() { () } fn run() { 12|3 }",
        "mod app; fn earlier() { () } fn run() { .t| }",
        "mod app; fn earlier() { () } fn run() { // ear|lier\n () }",
        "mod app; fn earlier() { () } fn run(new|name: .) { () }",
        "mod app; fn earlier() { () } fn run() { let new|name = (); () }",
    ] {
        let (_, result) = complete_marked("", source);
        assert!(result.items.is_empty(), "{source}: {:?}", result.items);
    }
}

#[test]
fn parser_context_incomplete_scope_and_type_namespace_are_exact() {
    for (source, included, excluded) in [
        (
            "mod app; fn earlier() { () } fn run(value: .) { let local = value; |",
            vec!["earlier", "value", "local"],
            vec!["run"],
        ),
        (
            "mod app; type Earlier = .; fn run[A](value: A) -> A|",
            vec!["Earlier", "A"],
            vec!["run", "value"],
        ),
        (
            "mod app; fn run() { .(local: .) { | } }",
            vec!["local"],
            vec!["run"],
        ),
        (
            "mod app; rec(loop) fn run(value: .) -> . { recur ru|n(value) }",
            vec!["run"],
            vec!["value"],
        ),
    ] {
        let (_, result) = complete_marked("", source);
        let labels: Vec<_> = result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        for label in included {
            assert!(labels.contains(&label), "{source}: {labels:?}");
        }
        for label in excluded {
            assert!(!labels.contains(&label), "{source}: {labels:?}");
        }
    }
}

#[test]
fn parser_context_edit_replaces_the_whole_atom() {
    let (source, result) = complete_marked(".pkg", "pkg app; build { ca|che (); }");
    let item = result
        .items
        .iter()
        .find(|item| item.label == "cache")
        .unwrap();
    let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
        panic!("exact atom edit");
    };
    let index = LineIndex::new(&source);
    let start = index.position_to_offset(LspPosition {
        line: edit.range.start.line,
        character: edit.range.start.character,
    }) as usize;
    let end = index.position_to_offset(LspPosition {
        line: edit.range.end.line,
        character: edit.range.end.character,
    }) as usize;
    assert_eq!(&source[start..end], "cache");
    let applied = format!("{}{}{}", &source[..start], edit.new_text, &source[end..]);
    crate::pass::parser::parse_package_file(&applied, None).expect("inserted completion parses");
}
