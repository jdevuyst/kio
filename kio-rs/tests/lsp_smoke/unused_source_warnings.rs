use super::*;

fn open(dir: &TempDir, source: &str) -> (LspProcess, String) {
    dir.write_pkg_root_package();
    let file = dir.write("pkg/main.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    (lsp, uri)
}

fn assert_warning_range(diagnostic: &Value, source: &str, name: &str) {
    let (start_line, start_character, end_line, end_character) =
        source_range(source, source.find(name).unwrap(), name.len());
    assert_eq!(
        diagnostic["range"],
        json!({
            "start": {"line": start_line, "character": start_character},
            "end": {"line": end_line, "character": end_character},
        })
    );
    assert_eq!(diagnostic["severity"], 2);
    assert_eq!(diagnostic["tags"], json!([1]));
}

#[test]
fn selective_import_removal_is_exact_and_rechecks_cleanly() {
    let dir = TempDir::new("unused-source-import");
    dir.write(
        "pkg/provider.kio",
        "module pkg/provider; pub fn used() -> . { () } pub fn unused() -> . { () }",
    );
    let source =
        "module pkg/main;\nimport pkg/provider(used, unused);\npub fn run() -> . { used() }\n";
    let (mut lsp, uri) = open(&dir, source);
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostics = publish["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{publish}");
    assert_warning_range(&diagnostics[0], source, "unused");
    let response = send_code_action(&mut lsp, &uri, &diagnostics[0]);
    let actions = response["result"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "{response}");
    assert_eq!(actions[0]["title"], "Remove unused import");
    assert_eq!(actions[0]["kind"], "quickfix");
    let (edits, version) = action_edits_for_uri(&actions[0], &uri);
    assert_eq!(version, Some(1));
    let repaired = apply_lsp_text_edits(source, edits);
    assert_eq!(repaired, source.replace(", unused", ""));
    send_full_text_change(&mut lsp, &uri, 2, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert_eq!(cleared["diagnostics"], json!([]), "{cleared}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn private_removal_owns_docs_and_allows_a_new_unused_import_warning() {
    let dir = TempDir::new("unused-source-private");
    dir.write(
        "pkg/provider.kio",
        "module pkg/provider; pub fn used() -> . { () }",
    );
    let owned =
        "/// Attached declaration documentation.\nfn dead() -> . { let local = (); used() }\n";
    let source = format!(
        "module pkg/main;\nimport pkg/provider(used);\n// Preserve this independent comment.\n\n{owned}pub fn run() -> . {{ () }}\n"
    );
    let (mut lsp, uri) = open(&dir, &source);
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostics = publish["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 2, "{publish}");
    let private = diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic["message"] == "private function `dead` is transitively unused"
        })
        .expect("private declaration warning");
    assert_warning_range(private, &source, "dead");
    let local = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["message"] == "unused binding `local`")
        .expect("unchanged local warning");
    assert!(local.get("tags").is_none());
    let response = send_code_action(&mut lsp, &uri, private);
    let actions = response["result"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "{response}");
    assert_eq!(
        actions[0]["title"],
        "Remove transitively unused private function"
    );
    let (edits, version) = action_edits_for_uri(&actions[0], &uri);
    assert_eq!(version, Some(1));
    let repaired = apply_lsp_text_edits(&source, edits);
    assert_eq!(repaired, source.replace(owned, ""));
    send_full_text_change(&mut lsp, &uri, 2, &repaired);
    let follow_on = wait_for_publish_version(&mut lsp, &uri, 2);
    let diagnostics = follow_on["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{follow_on}");
    assert_eq!(diagnostics[0]["message"], "unused import");
    assert_eq!(diagnostics[0]["severity"], 2);
    let response = send_code_action(&mut lsp, &uri, &diagnostics[0]);
    let actions = response["result"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "{response}");
    let (edits, _) = action_edits_for_uri(&actions[0], &uri);
    let repaired = apply_lsp_text_edits(&repaired, edits);
    send_full_text_change(&mut lsp, &uri, 3, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 3);
    assert_eq!(cleared["diagnostics"], json!([]), "{cleared}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn active_executable_docs_withhold_only_the_new_source_warnings() {
    let dir = TempDir::new("unused-source-doc-uncertainty");
    dir.write(
        "pkg/provider.kio",
        "module pkg/provider; pub fn unused() -> . { () }",
    );
    let source = "module pkg/main;\nimport pkg/provider(unused);\n/// ```kio {@}\n/// ()\n/// ```\nfn dead() -> . { let local = (); () }\n";
    let (mut lsp, uri) = open(&dir, source);
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostics = publish["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{publish}");
    assert_eq!(diagnostics[0]["message"], "unused binding `local`");
    assert_eq!(diagnostics[0]["severity"], 2);
    assert!(diagnostics[0].get("tags").is_none());
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn forged_unused_removal_cannot_delete_a_public_declaration() {
    let dir = TempDir::new("unused-source-forged-removal");
    let source = "module pkg/main;\nfn dead() -> . { () }\npub fn live() -> . { () }\n";
    let (mut lsp, uri) = open(&dir, source);
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostics = publish["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{publish}");
    let original = &diagnostics[0];
    let valid = send_code_action(&mut lsp, &uri, original);
    assert_eq!(valid["result"].as_array().unwrap().len(), 1, "{valid}");

    let mut forged = original.clone();
    forged["data"]["fixes"][0]["edits"][0]["range"]["end"] = json!({"line": 3, "character": 0});
    let response = send_code_action(&mut lsp, &uri, &forged);
    assert_eq!(response["result"], json!([]), "{response}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn forged_unused_fix_all_cannot_delete_an_intervening_public_declaration() {
    let dir = TempDir::new("unused-source-forged-fix-all");
    let source = "module pkg/main;\nfn first() -> . { () }\npub fn live() -> . { () }\nfn second() -> . { () }\n";
    let (mut lsp, uri) = open(&dir, source);
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostics = publish["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 2, "{publish}");
    let valid = send_code_actions_for_kind(&mut lsp, &uri, diagnostics, "source.fixAll");
    assert_eq!(valid["result"].as_array().unwrap().len(), 1, "{valid}");
    let (edits, _) = action_edits_for_uri(&valid["result"][0], &uri);
    assert_eq!(
        apply_lsp_text_edits(source, edits),
        "module pkg/main;\npub fn live() -> . { () }\n"
    );
    let mut forged = diagnostics.clone();
    let first = forged
        .iter_mut()
        .find(|warning| warning["range"]["start"]["line"] == 1)
        .unwrap();
    first["data"]["fixes"][0]["edits"][0]["range"]["end"] = json!({"line": 3, "character": 0});
    let response = send_code_actions_for_kind(&mut lsp, &uri, &forged, "source.fixAll");
    assert_eq!(response["result"], json!([]), "{response}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn private_function_removal_owns_its_optional_separator_but_not_comments() {
    for (declaration, retained) in [
        ("fn dead() -> . { () }\r\n", ""),
        ("fn dead() -> . { () };\r\n", ""),
        (
            "fn dead() -> . { () } // keep between\r\n; // keep after\r\n",
            " // keep between\r\n // keep after\r\n",
        ),
    ] {
        let dir = TempDir::new("unused-source-optional-separator");
        let source = format!("module pkg/main;\r\n{declaration}pub fn live() -> . {{ () }}\r\n");
        let (mut lsp, uri) = open(&dir, &source);
        let publish = wait_for_publish_version(&mut lsp, &uri, 1);
        let diagnostics = publish["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{publish}");
        let response = send_code_action(&mut lsp, &uri, &diagnostics[0]);
        let actions = response["result"].as_array().unwrap();
        assert_eq!(actions.len(), 1, "{declaration}: {response}");
        let (edits, _) = action_edits_for_uri(&actions[0], &uri);
        let repaired = apply_lsp_text_edits(&source, edits);
        assert_eq!(repaired, source.replace(declaration, retained));
        send_full_text_change(&mut lsp, &uri, 2, &repaired);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert_eq!(cleared["diagnostics"], json!([]), "{cleared}");
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn private_type_removal_preserves_the_explicit_underscore_opt_out() {
    let dir = TempDir::new("unused-source-private-type");
    let owned = "/// Attached private type documentation.\ntype Dead = .;\n";
    let source = format!("module pkg/main;\n{owned}type _Kept = .;\npub fn run() -> . {{ () }}\n");
    let (mut lsp, uri) = open(&dir, &source);
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostics = publish["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{publish}");
    assert_warning_range(&diagnostics[0], &source, "Dead");
    let response = send_code_action(&mut lsp, &uri, &diagnostics[0]);
    let actions = response["result"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "{response}");
    assert_eq!(
        actions[0]["title"],
        "Remove transitively unused private type"
    );
    let (edits, _) = action_edits_for_uri(&actions[0], &uri);
    let repaired = apply_lsp_text_edits(&source, edits);
    assert_eq!(repaired, source.replace(owned, ""));
    send_full_text_change(&mut lsp, &uri, 2, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert_eq!(cleared["diagnostics"], json!([]), "{cleared}");
    assert_eq!(lsp.shutdown(), 0);
}
