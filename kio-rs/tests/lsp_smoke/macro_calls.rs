use super::*;

const PROVIDER: &str = "mod provider; import __comptime__; pure fn implementation(_ct: __Comptime__, _source: __Type__, value: __Checked_term__) -> __Checked_term__ { value } pub macro identity : [Source] Source -> Source { impl implementation; }; pub fn ordinary(value: .) { value }";

fn token_type_at(lsp: &mut LspProcess, uri: &str, source: &str, needle: &str) -> u64 {
    let wanted = source_position(source, source.find(needle).unwrap());
    let result = send_semantic_tokens_full(lsp, uri);
    let mut line = 0;
    let mut column = 0;
    for cell in result["data"].as_array().unwrap().as_chunks::<5>().0 {
        let delta_line = cell[0].as_u64().unwrap();
        line += delta_line;
        column = if delta_line == 0 {
            column + cell[1].as_u64().unwrap()
        } else {
            cell[1].as_u64().unwrap()
        };
        if (line, column) == wanted {
            return cell[3].as_u64().unwrap();
        }
    }
    panic!("missing token {needle}: {result}");
}

#[test]
fn lsp_macro_tokens_follow_alias_shadowing_and_import_edits() {
    let dir = TempDir::new("macro-token-scope");
    dir.write("app.pkg.kio", "pkg app; bridge { app; }");
    dir.write("provider.kio", PROVIDER);
    let source = "mod app; import provider {macro identity as selected} pub fn run() { selected(()); (.(selected: . -> .) { selected(()) })(.(value: .) { value }) }";
    let uri = path_to_file_uri(&dir.write("app.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    assert_eq!(token_type_at(&mut lsp, &uri, source, "selected(());"), 12);
    assert_eq!(token_type_at(&mut lsp, &uri, source, "selected(()) }"), 5);
    let edited = source.replace("macro identity", "fn ordinary");
    send_full_text_change(&mut lsp, &uri, 2, &edited);
    assert_eq!(token_type_at(&mut lsp, &uri, &edited, "selected(());"), 5);
    send_full_text_change(&mut lsp, &uri, 3, source);
    assert_eq!(token_type_at(&mut lsp, &uri, source, "selected(());"), 12);
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_macro_import_kind_fixes_apply_and_refuse_stale_diagnostics() {
    for (selection, expected_selection) in [
        ("fn identity as selected", "macro identity as selected"),
        (
            "macro // keep\n ordinary as selected",
            "fn // keep\n ordinary as selected",
        ),
    ] {
        let dir = TempDir::new("macro-import-kind-fix");
        dir.write("app.pkg.kio", "pkg app; bridge { app; }");
        dir.write("provider.kio", PROVIDER);
        let source =
            format!("mod app; import provider {{{selection}}} pub fn run() {{ selected(()) }}");
        let uri = path_to_file_uri(&dir.write("app.kio", &source));
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        lsp.send_notification("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}));
        let published = wait_for_publish_version(&mut lsp, &uri, 1);
        let diagnostic = published["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|diagnostic| {
                diagnostic["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("is not a"))
            })
            .unwrap_or_else(|| panic!("missing kind mismatch: {published}"))
            .clone();
        let response = send_code_action(&mut lsp, &uri, &diagnostic);
        let action = response["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["kind"] == "quickfix")
            .unwrap_or_else(|| panic!("{response}"));
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1));
        let applied = apply_lsp_text_edits(&source, edits);
        assert_eq!(applied, source.replace(selection, expected_selection));
        send_full_text_change(&mut lsp, &uri, 2, &applied);
        assert_eq!(
            wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"],
            json!([])
        );
        assert_eq!(
            send_code_action(&mut lsp, &uri, &diagnostic)["result"],
            json!([])
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_unused_macro_import_removal_owns_its_tag_and_preserves_other_kinds() {
    for (selection, expected) in [
        ("macro identity; fn ordinary", " fn ordinary"),
        ("fn ordinary; macro identity as selected", "fn ordinary"),
    ] {
        let dir = TempDir::new("unused-macro-import");
        dir.write("app.pkg.kio", "pkg app; bridge { app; }");
        dir.write("provider.kio", PROVIDER);
        let source =
            format!("mod app; import provider {{{selection}}} pub fn run() {{ ordinary(()) }}");
        let uri = path_to_file_uri(&dir.write("app.kio", &source));
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        lsp.send_notification("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}));
        let published = wait_for_publish_version(&mut lsp, &uri, 1);
        let diagnostic = published["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|diagnostic| diagnostic["message"] == "unused import")
            .unwrap_or_else(|| panic!("{published}"));
        let response = send_code_action(&mut lsp, &uri, diagnostic);
        let action = response["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["title"] == "Remove unused import")
            .unwrap_or_else(|| panic!("{response}"));
        let (edits, _) = action_edits_for_uri(action, &uri);
        let applied = apply_lsp_text_edits(&source, edits);
        assert_eq!(applied, source.replace(selection, expected));
        send_full_text_change(&mut lsp, &uri, 2, &applied);
        assert_eq!(
            wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"],
            json!([])
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}
