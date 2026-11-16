use super::*;

#[test]
fn inspection_tracks_resolved_function_contract_and_provider_edits() {
    let dir = TempDir::new("inspection-contract");
    dir.write_pkg_root_package();
    let provider = "mod pkg/provider; pub pure fn apply(x: .) -> . { x }";
    let provider_uri = path_to_file_uri(&dir.write("pkg/provider.kio", provider));
    let source = "mod pkg/test;\nimport pkg/provider { fn apply as selected }\npub fn run() { let saved = selected; selected(()); saved(()) }\n";
    let uri = path_to_file_uri(&dir.write("pkg/test.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &provider_uri, provider);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let hover_at = |lsp: &mut LspProcess, text: &str| {
        let (line, character) = source_position(source, source.find(text).unwrap());
        send_hover(lsp, &uri, line, character)
    };
    let selected = hover_at(&mut lsp, "selected;");
    assert!(selected.to_string().contains("pure fn:"), "{selected}");
    let local = hover_at(&mut lsp, "saved(())");
    assert!(!local.to_string().contains("pure fn"), "{local}");
    let (line, character) = source_position(
        source,
        source.find("selected(())").unwrap() + "selected(".len(),
    );
    let (completion_line, completion_character) =
        source_position(source, source.find("selected(())").unwrap() + "sel".len());
    let completion = send_completion(&mut lsp, &uri, completion_line, completion_character);
    let item = completion["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"] == "selected")
        .unwrap_or_else(|| panic!("{completion}"));
    assert!(
        item["detail"].as_str().unwrap().contains("pure fn apply"),
        "{item}"
    );
    let signature = send_signature_help(&mut lsp, &uri, line, character);
    assert!(
        signature.to_string().contains("pure fn apply"),
        "{signature}"
    );
    let updated = provider.replace("pure fn", "fn");
    send_full_text_change(&mut lsp, &provider_uri, 2, &updated);
    wait_for_analysis_event(&lsp.analysis_events, &provider_uri, 2);
    let changed = hover_at(&mut lsp, "selected;");
    assert!(changed.to_string().contains("fn:"), "{changed}");
    assert!(!changed.to_string().contains("pure fn:"), "{changed}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn dependency_package_inspection_uses_current_selected_declaration() {
    let dir = TempDir::new("inspection-dependency");
    dir.write("app/app.pkg.kio", "pkg app; bridge { app/**; }");
    dir.write(
        "app/dep.dep.kio",
        "dep dep; source { path \"../dep/dep.pkg.kio\"; }",
    );
    dir.write("dep/dep.pkg.kio", "pkg dep; bridge { api; }");
    dir.write("dep/api.kio", "mod api; pub pure fn apply(x: .) -> . { x }");
    let provider = "mod dep/api; pub pure fn apply(x: .) -> . { x }";
    let provider_uri = path_to_file_uri(&dir.write("app/dep/api.kio", provider));
    let source =
        "mod app/main; import dep/api { fn apply as selected } pub fn run() { selected(()) }";
    let uri = path_to_file_uri(&dir.write("app/app/main.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &provider_uri, provider);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let offset = source.find("selected(())").unwrap();
    let (line, character) = source_position(source, offset);
    let hover = send_hover(&mut lsp, &uri, line, character);
    assert!(hover.to_string().contains("pure fn:"), "{hover}");
    let (line, character) = source_position(source, offset + "selected(".len());
    let signature = send_signature_help(&mut lsp, &uri, line, character);
    assert!(
        signature.to_string().contains("pure fn apply"),
        "{signature}"
    );
    let updated = provider.replace("pure fn", "fn");
    send_full_text_change(&mut lsp, &provider_uri, 2, &updated);
    wait_for_analysis_event(&lsp.analysis_events, &provider_uri, 2);
    let (line, character) = source_position(source, offset);
    let hover = send_hover(&mut lsp, &uri, line, character);
    assert!(hover.to_string().contains("fn:"), "{hover}");
    assert!(!hover.to_string().contains("pure fn:"), "{hover}");
    assert_eq!(lsp.shutdown(), 0);
}
