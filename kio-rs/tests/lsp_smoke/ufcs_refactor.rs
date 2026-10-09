use super::*;

fn actions(
    lsp: &mut LspProcess,
    uri: &str,
    source: &str,
    selection: &str,
    only: Option<&str>,
) -> Value {
    let start = source.rfind(selection).unwrap();
    let (sl, sc, el, ec) = source_range(source, start, selection.len());
    let mut context = json!({"diagnostics": []});
    if let Some(only) = only {
        context["only"] = json!([only]);
    }
    let id = lsp.send_request(
        "textDocument/codeAction",
        json!({
            "textDocument": {"uri": uri},
            "range": {"start": {"line": sl, "character": sc}, "end": {"line": el, "character": ec}},
            "context": context,
        }),
    );
    lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(id))
}

fn open(dir: &TempDir, source: &str) -> (LspProcess, String) {
    dir.write_pkg_root_package();
    let file = dir.write("pkg/main.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    let capabilities = lsp.initialize_with_capabilities(&path_to_file_uri(dir.path()), json!({}));
    assert!(
        capabilities["codeActionProvider"]["codeActionKinds"]
            .as_array()
            .unwrap()
            .contains(&json!("refactor.rewrite"))
    );
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    ready(&mut lsp, &uri, source);
    (lsp, uri)
}

fn ready(lsp: &mut LspProcess, uri: &str, source: &str) {
    let (line, character, _, _) = source_range(source, source.find("run").unwrap(), 3);
    assert!(!wait_for_hover(lsp, uri, line, character).is_null());
}

fn wait_for_action(lsp: &mut LspProcess, uri: &str, source: &str, body: &str) -> Value {
    let mut response = Value::Null;
    for _ in 0..20 {
        response = actions(lsp, uri, source, body, Some("refactor.rewrite"));
        if response["result"]
            .as_array()
            .is_some_and(|actions| !actions.is_empty())
        {
            return response;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the restored provider must authorize the chain: {response}")
}

#[test]
fn ufcs_refactor_applies_without_diagnostics_and_obeys_kind_filters() {
    let dir = TempDir::new("ufcs-refactor-kinds");
    let body = "f(f((), f((), ())), ())";
    let source =
        format!("module pkg/main; fn f(a: ., _b: .) -> . {{ a }} pub fn run() -> . {{ {body} }}");
    let (mut lsp, uri) = open(&dir, &source);
    for kind in [None, Some("refactor"), Some("refactor.rewrite")] {
        let response = actions(&mut lsp, &uri, &source, body, kind);
        let actions = response["result"].as_array().unwrap();
        assert_eq!(actions.len(), 1, "{response}");
        assert_eq!(actions[0]["title"], "Convert nested calls to UFCS");
        assert_eq!(actions[0]["kind"], "refactor.rewrite");
        assert!(actions[0].get("diagnostics").is_none());
        let (edits, version) = action_edits_for_uri(&actions[0], &uri);
        assert_eq!(version, Some(1));
        assert_eq!(edits.len(), 1);
        assert_eq!(
            apply_lsp_text_edits(&source, edits),
            source.replace(body, "(().>>f(()).>>f(()).>f(()))")
        );
    }
    for kind in ["quickfix", "source.fixAll", "refactor.extract"] {
        let response = actions(&mut lsp, &uri, &source, body, Some(kind));
        assert_eq!(response["result"], json!([]), "{response}");
    }
    let response = actions(&mut lsp, &uri, &source, body, Some("refactor.rewrite"));
    let (edits, _) = action_edits_for_uri(&response["result"][0], &uri);
    let edited = apply_lsp_text_edits(&source, edits);
    send_full_text_change(&mut lsp, &uri, 2, &edited);
    ready(&mut lsp, &uri, &edited);
    let response = actions(&mut lsp, &uri, &edited, "().>>f(()).>>f(()).>f(())", None);
    assert_eq!(response["result"], json!([]), "{response}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn ufcs_refactor_imported_host_calls_require_current_provider_and_open_version() {
    let dir = TempDir::new("ufcs-refactor-provider");
    let provider = "module pkg/provider; host type Text role(str); host fn string_concat(a: Text, b: Text) -> Text;";
    let provider_file = dir.write("pkg/provider.kio", provider);
    let provider_uri = path_to_file_uri(&provider_file);
    let body = "p.string_concat(\"a\", string_concat(\"b\", p.string_concat(\"c\", \"d\")))";
    let source = format!(
        "module pkg/main; import pkg/provider(Text, string_concat); import pkg/provider as p; pub fn run() -> Text {{ {body} }}"
    );
    let (mut lsp, uri) = open(&dir, &source);
    let response = actions(&mut lsp, &uri, &source, body, None);
    assert_eq!(
        response["result"].as_array().unwrap().len(),
        1,
        "{response}"
    );
    let (edits, version) = action_edits_for_uri(&response["result"][0], &uri);
    assert_eq!(version, Some(1));
    let edited = apply_lsp_text_edits(&source, edits);
    assert!(edited.contains(
        "\"d\".>>p.string_concat(\"c\").>>string_concat(\"b\").>>p.string_concat(\"a\")"
    ));

    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {
            "uri": provider_uri, "languageId": "kio", "version": 1, "text": provider,
        }}),
    );
    let invalid_provider = provider.replace("b: Text", "b: .");
    send_full_text_change(&mut lsp, &provider_uri, 2, &invalid_provider);
    let stale = actions(&mut lsp, &uri, &source, body, None);
    assert_eq!(stale["result"], json!([]), "{stale}");
    send_full_text_change(&mut lsp, &provider_uri, 3, provider);
    send_full_text_change(&mut lsp, &uri, 2, &source);
    ready(&mut lsp, &uri, &source);
    let response = wait_for_action(&mut lsp, &uri, &source, body);
    let (edits, version) = action_edits_for_uri(&response["result"][0], &uri);
    assert_eq!(version, Some(2));
    assert_eq!(apply_lsp_text_edits(&source, edits), edited);
    send_full_text_change(&mut lsp, &uri, 3, &edited);
    ready(&mut lsp, &uri, &edited);
    lsp.send_notification(
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri}}),
    );
    let closed = actions(&mut lsp, &uri, &source, body, None);
    assert_eq!(closed["result"], json!([]), "{closed}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn ufcs_refactor_ambiguous_selection_and_gap_comments_withhold() {
    for body in [
        "f(f((), ()), f((), ()))",
        "f( // keep attachment\n f((), ()), ())",
    ] {
        let dir = TempDir::new("ufcs-refactor-negative");
        let source = format!(
            "module pkg/main; fn f(a: ., _b: .) -> . {{ a }} pub fn run() -> . {{ {body} }}"
        );
        let (mut lsp, uri) = open(&dir, &source);
        let response = actions(&mut lsp, &uri, &source, body, None);
        assert_eq!(response["result"], json!([]), "{response}");
        assert_eq!(lsp.shutdown(), 0);
    }
}
