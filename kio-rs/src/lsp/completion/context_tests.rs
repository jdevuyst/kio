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
fn parser_context_recursive_label_payload_recovery_keeps_type_scope() {
    for (marked, expected, excluded) in [
        (
            "module app; import pkg/dep(Imported); rec labels { list[A] <U> : Pair(A, bogus[B]: |",
            vec!["A", "U", "List", "Imported"],
            vec!["B", "Bogus"],
        ),
        (
            "module app; import pkg/dep(Imported); rec labels Tree[A] = { list[B] <U> : Pair(B, bogus[C]: |",
            vec!["A", "B", "U", "Tree", "List", "Imported"],
            vec!["C", "Bogus"],
        ),
        (
            "module app; rec labels { first[A]: Pair(A, bogus[B]: ), second[C]: |",
            vec!["C", "First", "Second"],
            vec!["A", "B", "Bogus"],
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
        "module app; rec labels { malformed[A B]: |",
        "module app; rec labels { list[A] <U> : Pair(A, bogus[B]: // A|",
        "module app; rec labels { list[A] <U> : Pair(A, bogus[B]: \"A|",
        "module app; rec labels { first[A]: Pair(A, bogus[B]: ), se|cond[C]: . };",
    ] {
        assert!(complete_marked("", marked).1.items.is_empty(), "{marked}");
    }
}

#[test]
fn parser_context_closed_vocabularies_cover_file_kinds_and_retract_fields() {
    for (kind, source, expected) in [
        ("", "|", vec!["module"]),
        (".pkg", "|", vec!["package"]),
        (".dep", "|", vec!["dependency"]),
        (".lock", "|", vec!["lock"]),
        (".sig", "|", vec!["signature"]),
        (
            ".pkg",
            "package app; build { cache (); |",
            vec!["docs", "target"],
        ),
        (
            ".pkg",
            "package app; build { target rust {} |",
            vec!["cache", "docs", "target"],
        ),
        (
            ".dep",
            "dependency app; source { |",
            vec!["git", "path", "ref"],
        ),
        (
            ".lock",
            "lock app; resolved { |",
            vec!["commit", "git", "path", "ref", "sig"],
        ),
        (
            ".sig",
            "signature app v(1); v(1) { |",
            vec!["breaking", "nonbreaking", "with"],
        ),
        (
            "",
            "module app; newtype A : . { constructor mk; |",
            vec!["projector", "pub"],
        ),
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
        ("", "module app; host |", vec!["fn", "pub", "type"]),
        ("", "module app; elab make: . { impl(|", vec!["fills"]),
        (
            ".dep",
            "dependency app; source { path \"app.pkg.kio\"; } rehost dep/api |",
            vec!["to"],
        ),
        (
            ".dep",
            "dependency app; source { path \"app.pkg.kio\"; } retype dep/api.Type |",
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
        "module app; fn target(x: .) { x } fn run() { target(|",
        "module app; fn target(x: .) { x } rec(group) fn run(x: .) { .(y: .) { target(|",
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
            "module app/main; type Item = .; fn identity(value: .) -> . {{ value }} fn choose[A](value: A) -> A {{ value }} fn run(value: .) -> . {{ {callee}(value) }}"
        );
        let file = root.join("app/main.kio");
        let files = std::collections::BTreeMap::from([
            (
                root.join("app.pkg.kio"),
                "package app; bridge { app/**; }".to_owned(),
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
            "package app; build { target rust {} target |",
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
            "package app; build { target ru|st {} }",
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
            "package app; build { target | {} target rust {} }",
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
fn parser_context_rec_annotations_require_an_eligible_structural_owner() {
    let mut invalid = Vec::new();
    for marked in [
        "module app; fn run() { rec(|",
        "module app; rec(drive) fn run(x: .) -> . { .(y: .) { rec(|",
    ] {
        let (_, result) = complete_marked("", marked);
        if !result.items.is_empty() {
            invalid.push((marked, result.items));
        }
    }
    let (_, result) = complete_marked("", "module app; rec(drive) fn run(x: .) -> . { rec(|");
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
                "module app; newtype Box[A] : A {{ constructor make; projector read; }}; type Alias[A] = Box(A); fn run() {{ {head}.|"
            ),
        );
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["make", "read"]
        );
    }
    let (_, result) = complete_marked(
        "",
        "module app; fn run() { Later.| } newtype Later : . { constructor make; projector read; };",
    );
    assert!(
        result.items.is_empty(),
        "later nominal is not in scope: {:?}",
        result.items
    );
    let (_, result) = complete_marked(
        "",
        "module app; labels Record = { field: . }; fn run() { Record.|",
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
        "module app; fn earlier() { () } fn run() { \"lit|eral\" }",
        "module app; fn earlier() { () } fn run() { 12|3 }",
        "module app; fn earlier() { () } fn run() { .t| }",
        "module app; fn earlier() { () } fn run() { // ear|lier\n () }",
        "module app; fn earlier() { () } fn run(new|name: .) { () }",
        "module app; fn earlier() { () } fn run() { let new|name = (); () }",
    ] {
        let (_, result) = complete_marked("", source);
        assert!(result.items.is_empty(), "{source}: {:?}", result.items);
    }
}

#[test]
fn parser_context_incomplete_scope_and_type_namespace_are_exact() {
    for (source, included, excluded) in [
        (
            "module app; fn earlier() { () } fn run(value: .) { let local = value; |",
            vec!["earlier", "value", "local"],
            vec!["run"],
        ),
        (
            "module app; type Earlier = .; fn run[A](value: A) -> A|",
            vec!["Earlier", "A"],
            vec!["run", "value"],
        ),
        (
            "module app; fn run() { .(local: .) { | } }",
            vec!["local"],
            vec!["run"],
        ),
        (
            "module app; rec(loop) fn run(value: .) -> . { rec ru|n(value) }",
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
    let (source, result) = complete_marked(".pkg", "package app; build { ca|che (); }");
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
