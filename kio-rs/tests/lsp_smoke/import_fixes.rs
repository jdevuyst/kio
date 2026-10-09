use super::*;

fn open_import_case(source: &str, provider: &str) -> (TempDir, LspProcess, String, Value) {
    let dir = TempDir::new("import-typo-fix");
    dir.write("probe.pkg.kio", "package probe;\n");
    dir.write("support.kio", provider);
    let path = dir.write("main.kio", "module main;\n");
    let uri = path_to_file_uri(&path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostic = publish["diagnostics"][0].clone();
    assert_eq!(diagnostic["code"], 12, "{publish}");
    (dir, lsp, uri, diagnostic)
}

#[test]
fn selective_import_typo_repairs_only_the_written_name_and_rechecks() {
    for (source, provider, typo, replacement) in [
        (
            "module main;\r\nimport support(\r\n\t// 😀 suggested name\r\n\thepler, // preserve the other selection\r\n\tkeep\r\n);\r\npub fn main() { helper(); keep() }\r\n",
            "module support; pub fn helper() { () } pub fn keep() { () }",
            "hepler",
            "helper",
        ),
        (
            "module main;\nimport support(Hepler);\npub fn main(value: Helper) -> Helper { value }\n",
            "module support; pub type Helper = .;",
            "Hepler",
            "Helper",
        ),
    ] {
        let (_dir, mut lsp, uri, diagnostic) = open_import_case(source, provider);
        assert_eq!(
            diagnostic["data"]["help"],
            format!("did you mean `{replacement}`?")
        );
        let response = send_code_action(&mut lsp, &uri, &diagnostic);
        let actions = response["result"].as_array().unwrap();
        assert_eq!(actions.len(), 1, "{response}");
        assert_eq!(actions[0]["kind"], "quickfix");
        assert_eq!(actions[0]["title"], format!("Replace with `{replacement}`"));
        let (edits, version) = action_edits_for_uri(&actions[0], &uri);
        assert_eq!(version, Some(1));
        assert_eq!(edits.len(), 1);
        let repaired = apply_lsp_text_edits(source, edits);
        assert_eq!(repaired, source.replacen(typo, replacement, 1));
        send_full_text_change(&mut lsp, &uri, 2, &repaired);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert_eq!(cleared["diagnostics"], json!([]), "{cleared}");
        assert_eq!(
            send_code_action(&mut lsp, &uri, &diagnostic)["result"],
            json!([])
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn selective_import_typo_does_not_offer_invisible_or_distant_names() {
    for provider in [
        "module support; fn helper() { () }",
        "module support; pub(support) fn helper() { () }",
        "module support; pub fn unrelated() { () }",
    ] {
        let source = "module main; import support(hepler); pub fn main() { () }";
        let (_dir, mut lsp, uri, diagnostic) = open_import_case(source, provider);
        assert!(diagnostic["data"]["fixes"].is_null(), "{diagnostic}");
        assert_eq!(
            send_code_action(&mut lsp, &uri, &diagnostic)["result"],
            json!([])
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn selective_import_typo_does_not_choose_between_equally_near_names() {
    let source = "module main; import support(heper); pub fn main() { () }";
    let (_dir, mut lsp, uri, diagnostic) = open_import_case(
        source,
        "module support; pub fn helper() { () } pub fn heaper() { () }",
    );
    assert_eq!(diagnostic["data"]["help"], "did you mean `heaper`?");
    assert!(diagnostic["data"]["fixes"].is_null(), "{diagnostic}");
    for kind in ["quickfix", "source.fixAll"] {
        let response =
            send_code_actions_for_kind(&mut lsp, &uri, std::slice::from_ref(&diagnostic), kind);
        assert_eq!(response["result"], json!([]), "{response}");
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn selective_import_typo_withholds_repairs_that_expose_another_error() {
    for source in [
        "module main; import support(hepler); fn helper() { () } pub fn main() { helper() }",
        "module main; import support(helper, hepler); pub fn main() { helper() }",
        "module main; import support(hepler); pub fn main() { hepler() }",
    ] {
        let (_dir, mut lsp, uri, diagnostic) =
            open_import_case(source, "module support; pub fn helper() { () }");
        assert_eq!(
            diagnostic["data"]["fixes"][0]["edits"][0]["replacement"],
            "helper"
        );
        assert_eq!(
            send_code_action(&mut lsp, &uri, &diagnostic)["result"],
            json!([])
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn selective_import_typo_rechecks_current_provider_overlay() {
    let source = "module main; import support(hepler); pub fn main() { helper() }";
    let (dir, mut lsp, uri, diagnostic) =
        open_import_case(source, "module support; pub fn helper() { () }");
    let initial = send_code_action(&mut lsp, &uri, &diagnostic);
    assert_eq!(initial["result"].as_array().unwrap().len(), 1, "{initial}");
    let provider_uri = path_to_file_uri(&dir.path().join("support.kio"));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": provider_uri, "languageId": "kio", "version": 1,
            "text": "module support; fn helper() { () }"}}),
    );
    let stale_provider = send_code_action(&mut lsp, &uri, &diagnostic);
    assert_eq!(stale_provider["result"], json!([]), "{stale_provider}");
    assert_eq!(lsp.shutdown(), 0);
}

fn assert_provider_growth_invalidates_fix(typo: &str, added: &str) {
    let source = format!("module main; import support({typo}); pub fn main() {{ () }}");
    let (dir, mut lsp, uri, diagnostic) =
        open_import_case(&source, "module support; pub fn helper() { () }");
    let initial = send_code_action(&mut lsp, &uri, &diagnostic);
    assert_eq!(initial["result"].as_array().unwrap().len(), 1, "{initial}");
    let provider_uri = path_to_file_uri(&dir.path().join("support.kio"));
    lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": provider_uri, "languageId": "kio", "version": 1,
                "text": format!("module support; pub fn helper() {{ () }} pub fn {added}() {{ () }}")}}),
        );
    let stale_provider = send_code_action(&mut lsp, &uri, &diagnostic);
    assert_eq!(stale_provider["result"], json!([]), "{stale_provider}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn selective_import_typo_rejects_stale_fixes_after_provider_growth_ties() {
    assert_provider_growth_invalidates_fix("heper", "heaper");
}

#[test]
fn selective_import_typo_rejects_stale_fixes_after_provider_growth_nearer() {
    assert_provider_growth_invalidates_fix("helpre", "helpr");
}
