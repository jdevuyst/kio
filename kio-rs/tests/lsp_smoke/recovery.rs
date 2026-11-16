use super::*;

fn symbols(lsp: &mut LspProcess, uri: &str) -> Value {
    let id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let response = lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(id));
    assert!(response.get("error").is_none(), "{response:?}");
    response.get("result").cloned().unwrap_or(Value::Null)
}

fn change_header(lsp: &mut LspProcess, uri: &str, version: i32, old: &str, replacement: &str) {
    lsp.send_notification("textDocument/didChange", json!({
        "textDocument": { "uri": uri, "version": version },
        "contentChanges": [{
            "range": { "start": { "line": 1, "character": 0 },
                "end": { "line": 1 + old.bytes().filter(|byte| *byte == b'\n').count(), "character": 0 } },
            "text": replacement,
        }],
    }));
}

#[test]
fn incomplete_declaration_edit_traces_retain_symbols_and_completion() {
    let dir = TempDir::new("declaration-recovery");
    let header = "mod pkg/main;\n";
    let following = "fn after() -> . { () }\nfn probe() -> . { after() }\n";
    let complete = format!("{header}{following}");
    let path = dir.write("pkg/main.kio", &complete);
    dir.write(
        "pkg/provider.kio",
        "mod pkg/provider; pub fn available() { () }\n",
    );
    dir.write_pkg_root_package();
    let uri = path_to_file_uri(&path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, &complete);
    let original = symbols(&mut lsp, &uri);
    let mut version = 1;
    for unfinished in [
        "macro op",
        "macro operation",
        "macro operation :",
        "import pkg/provider {macro as ",
        "import pkg/provider {macro as",
        "import pkg/provider {",
        "type { Name: .",
        "type {",
        "type Result = {",
        "op_macro _ &&",
        "op_var [*",
        "fn witness() { f[ }",
        "fn witness() { f( }",
    ] {
        let inserted = format!("{unfinished}\n");
        let source = format!("{header}{inserted}{following}");
        version += 1;
        change_header(&mut lsp, &uri, version, "", &inserted);
        let diagnostics = wait_for_publish_version(&mut lsp, &uri, version.into());
        assert!(
            diagnostics["diagnostics"]
                .as_array()
                .is_some_and(|items| items.iter().any(|diagnostic| diagnostic["severity"] == 1)),
            "unfinished declaration must remain invalid: {source}: {diagnostics}"
        );
        let edited = symbols(&mut lsp, &uri);
        let names: Vec<_> = edited
            .as_array()
            .expect("recovered outline")
            .iter()
            .filter_map(|symbol| symbol["name"].as_str())
            .collect();
        assert!(
            names.contains(&"after") && names.contains(&"probe"),
            "{source}: {edited}"
        );
        let completion = send_completion(&mut lsp, &uri, 3, 20);
        assert!(
            completion_labels(&completion).contains(&"after"),
            "{source}: {completion}"
        );
        lsp.send_notification(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        );
        version += 1;
        lsp.send_notification(
            "textDocument/didOpen",
            json!({ "textDocument": {
            "uri": uri, "languageId": "kio", "version": version, "text": source,
        } }),
        );
        wait_for_analysis_event(&lsp.analysis_events, &uri, version);
        assert_eq!(
            symbols(&mut lsp, &uri),
            edited,
            "fresh/edited outline: {unfinished}"
        );
        assert_eq!(
            send_completion(&mut lsp, &uri, 3, 20),
            completion,
            "fresh/edited completion: {unfinished}"
        );
        version += 1;
        change_header(&mut lsp, &uri, version, &inserted, "");
        let diagnostics = wait_for_publish_version(&mut lsp, &uri, version.into());
        assert!(
            diagnostics["diagnostics"]
                .as_array()
                .is_some_and(|items| items.iter().all(|diagnostic| diagnostic["severity"] != 1)),
            "{diagnostics}"
        );
        assert_eq!(
            symbols(&mut lsp, &uri),
            original,
            "restored outline: {unfinished}"
        );
    }
    assert_eq!(lsp.shutdown(), 0);
}
