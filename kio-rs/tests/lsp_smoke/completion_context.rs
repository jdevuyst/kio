use super::*;

fn open_change_and_complete(kind: &str, marked: &str) -> Value {
    let cursor = marked.find('|').expect("completion marker");
    let source = marked.replacen('|', "", 1);
    let dir = TempDir::new("completion-context-protocol");
    dir.write("outer.pkg.kio", "package outer;\n");
    let path = dir.write(&format!("workspace/app{kind}.kio"), &source);
    if kind != ".pkg" {
        dir.write("workspace/app.pkg.kio", "package app;\n");
    }
    let workspace = path.parent().expect("workspace");
    let uri = path_to_file_uri(&path);
    let (line, character) = source_position(&source, cursor);
    let (mut lsp, stderr_rx) =
        LspProcess::spawn_with_env_and_captured_stderr(&[("KIO_DEBUG_TIMING", "lsp")]);
    lsp.initialize(&path_to_file_uri(workspace));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": source }],
        }),
    );
    let result = send_completion(&mut lsp, &uri, line, character);
    assert_eq!(lsp.shutdown(), 0);
    assert_scheduled_root(stderr_rx, workspace);
    result
}

pub(super) fn assert_scheduled_root(stderr_rx: Receiver<String>, workspace: &Path) {
    let stderr = stderr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("LSP stderr after shutdown");
    let expected = fs::canonicalize(workspace).expect("canonical workspace");
    let schedules: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("lsp-timing: schedule "))
        .collect();
    assert!(!schedules.is_empty(), "missing analysis schedule: {stderr}");
    for line in schedules {
        let package = line
            .split_once(" package=")
            .and_then(|(_, rest)| rest.rsplit_once(" overlays="))
            .map(|(package, _)| Path::new(package))
            .expect("scheduled package root");
        assert_eq!(package, expected, "analysis escaped workspace: {line}");
    }
}

fn exact_labels(result: &Value) -> Vec<&str> {
    assert_eq!(
        result.get("isIncomplete").and_then(Value::as_bool),
        Some(false)
    );
    let mut labels = completion_labels(result);
    labels.sort_unstable();
    labels
}

#[test]
fn lsp_protocol_neutral_block_completion_tracks_lexical_scope() {
    let mut failures = Vec::new();
    for (body, expected, forbidden) in [
        (
            "sequence! bind { let local <- parent; |",
            vec!["local", "parent", "rec"],
            vec!["run"],
        ),
        (
            "sequence! bind { parent; |",
            vec!["parent", "rec"],
            vec!["run"],
        ),
        (
            "sequence! bind { let local <- parent; rec(|",
            vec!["cont", "poly"],
            vec![],
        ),
        ("sequence! bind { parent; rec |", vec!["run"], vec![]),
        (
            "sequence! bind { let local = parent; |",
            vec!["local", "parent", "rec"],
            vec!["run"],
        ),
        (
            "sequence! bind { let local <- |parent; local }",
            vec!["parent", "rec"],
            vec!["local", "run"],
        ),
        (
            "let prior = sequence! bind { let local <- parent; local }; |",
            vec!["prior", "parent", "rec"],
            vec!["local", "run"],
        ),
        ("scope! { parent; |", vec!["parent", "rec"], vec!["run"]),
        (
            "sequence! bind { .() { |",
            vec!["parent"],
            vec!["rec", "run"],
        ),
    ] {
        let marked = format!("module app; rec(loop) fn run[A](parent: .) {{ {body}");
        let result = open_change_and_complete("", &marked);
        let labels = completion_labels(&result);
        if expected.iter().any(|word| !labels.contains(word))
            || forbidden.iter().any(|word| labels.contains(word))
        {
            failures.push(format!("{marked}: {labels:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn lsp_protocol_completion_closed_contexts_are_exact_after_open_and_change() {
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
            ".dep",
            "dependency app; source { path \"nested/app.pkg.kio\"; |",
            vec!["git", "ref"],
        ),
        (
            ".dep",
            "dependency app; source { ref \"main\"; path \"nested/app.pkg.kio\"; |",
            vec!["git"],
        ),
        (
            ".dep",
            "dependency app; source { git \"repo\"; ref \"main\"; |",
            vec!["path"],
        ),
        (
            ".dep",
            "dependency app; source { git \"repo\"; path \"nested/app.pkg.kio\"; ref \"main\"; |",
            vec![],
        ),
        (
            ".lock",
            "lock app; resolved { path \"nested/app.pkg.kio\"; |",
            vec!["commit", "git", "ref", "sig"],
        ),
        (
            ".lock",
            "lock app; resolved { git \"repo\"; ref \"main\"; commit \"c\"; sig \"s\"; |",
            vec!["path"],
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
        assert_eq!(
            exact_labels(&open_change_and_complete(kind, source)),
            expected,
            "{source}"
        );
    }
}

#[test]
fn lsp_protocol_git_manifest_completion_retracts_after_real_edits() {
    for (kind, header, required) in [
        (
            "dep",
            "dependency app; source",
            "git \"repo\"; ref \"main\";",
        ),
        (
            "lock",
            "lock app; resolved",
            "git \"repo\"; ref \"main\"; commit \"c\"; sig \"s\";",
        ),
    ] {
        let dir = TempDir::new("git-manifest-completion-edits");
        dir.write("app.pkg.kio", "package app;\n");
        let initial = format!("{header} {{ {required} ");
        let path = dir.write(&format!("app.{kind}.kio"), &initial);
        let uri = path_to_file_uri(&path);
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": "kio", "version": 1, "text": initial,
            }}),
        );
        for (index, (source, expected)) in [
            (initial.clone(), vec!["path"]),
            (format!("{initial}path \"nested/app.pkg.kio\"; "), vec![]),
            (initial.clone(), vec!["path"]),
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                lsp.send_notification(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri, "version": index + 1},
                        "contentChanges": [{"text": source}],
                    }),
                );
            }
            let (line, character) = source_position(&source, source.len());
            assert_eq!(
                exact_labels(&send_completion(&mut lsp, &uri, line, character)),
                expected,
                "{kind} edit {index}: {source}"
            );
        }
        assert_eq!(lsp.shutdown(), 0);
    }
    for (kind, source) in [
        (
            ".dep",
            "dependency app; source { path \"nested/|app.pkg.kio\"; }",
        ),
        (".dep", "dependency app; source { // pa|th\n }"),
        (
            ".lock",
            "lock app; resolved { path \"nested/|app.pkg.kio\"; }",
        ),
        (".lock", "lock app; resolved { // pa|th\n }"),
    ] {
        assert!(
            exact_labels(&open_change_and_complete(kind, source)).is_empty(),
            "{source}"
        );
    }
}

#[test]
fn lsp_protocol_completion_suppresses_non_name_contexts_after_open_and_change() {
    for source in [
        "module app; fn earlier() { () } fn run() { \"lit|eral\" }",
        "module app; fn earlier() { () } fn run() { 12|3 }",
        "module app; fn earlier() { () } fn run() { .t| }",
        "module app; fn earlier() { () } fn run() { // ear|lier\n () }",
        "module app; fn earlier() { () } fn run(new|name: .) { () }",
        "module app; fn earlier() { () } fn run() { let new|name = (); () }",
    ] {
        assert!(
            exact_labels(&open_change_and_complete("", source)).is_empty(),
            "{source}"
        );
    }
}

#[test]
fn lsp_protocol_completion_preserves_scope_and_recursive_identity_after_open_and_change() {
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
        let result = open_change_and_complete("", source);
        let labels = exact_labels(&result);
        for label in included {
            assert!(labels.contains(&label), "{source}: {labels:?}");
        }
        for label in excluded {
            assert!(!labels.contains(&label), "{source}: {labels:?}");
        }
    }
}

#[test]
fn lsp_protocol_completion_edit_replaces_the_whole_atom_after_open_and_change() {
    let source = "package app; build { ca|che (); }";
    let cursor = source.find('|').expect("completion marker");
    let source = source.replacen('|', "", 1);
    let result = open_change_and_complete(".pkg", "package app; build { ca|che (); }");
    let item = result["items"]
        .as_array()
        .expect("completion items")
        .iter()
        .find(|item| item["label"] == "cache")
        .expect("cache completion");
    let edit = &item["textEdit"];
    let range = &edit["range"];
    let start_line = range["start"]["line"].as_u64().expect("start line");
    let start_character = range["start"]["character"]
        .as_u64()
        .expect("start character");
    let end_line = range["end"]["line"].as_u64().expect("end line");
    let end_character = range["end"]["character"].as_u64().expect("end character");
    assert_eq!((start_line, start_character), (0, 21));
    assert_eq!((end_line, end_character), (0, 26));
    assert_eq!(&source[21..26], "cache");
    assert_eq!(edit["newText"].as_str(), Some("cache"));
    assert_eq!(cursor, 23);
}
