use super::*;

fn open_case(source: &str, provider: &str, code: i32) -> (TempDir, LspProcess, String, Value) {
    let dir = TempDir::new("name-fix");
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
    assert_eq!(diagnostic["code"], code, "{publish}");
    (dir, lsp, uri, diagnostic)
}

fn assert_no_eager_fix(lsp: &mut LspProcess, uri: &str, diagnostic: &Value) {
    assert!(diagnostic["data"]["fixes"].is_null(), "{diagnostic}");
    for kind in ["quickfix", "source.fixAll"] {
        let response = send_code_actions_for_kind(lsp, uri, std::slice::from_ref(diagnostic), kind);
        if kind == "source.fixAll" {
            assert_eq!(response["result"], json!([]), "{response}");
        } else {
            assert!(
                response["result"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|action| action["edit"].is_null() && action["isPreferred"] != true),
                "{response}"
            );
        }
    }
}

fn assert_repair(
    lsp: &mut LspProcess,
    uri: &str,
    diagnostic: &Value,
    source: &str,
    typo: &str,
    replacement: &str,
) {
    let response = send_code_action(lsp, uri, diagnostic);
    let actions: Vec<_> = response["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| !action["edit"].is_null())
        .collect();
    assert_eq!(actions.len(), 1, "{response}");
    assert_eq!(actions[0]["isPreferred"], true);
    let (edits, version) = action_edits_for_uri(actions[0], uri);
    assert_eq!(version, Some(1));
    assert_eq!(edits.len(), 1);
    let repaired = apply_lsp_text_edits(source, edits);
    assert_eq!(repaired, source.replacen(typo, replacement, 1));
    send_full_text_change(lsp, uri, 2, &repaired);
    let cleared = wait_for_publish_version(lsp, uri, 2);
    assert_eq!(cleared["diagnostics"], json!([]), "{cleared}");
    assert_eq!(send_code_action(lsp, uri, diagnostic)["result"], json!([]));
}

#[test]
fn ordinary_value_and_type_alias_ties_keep_help_without_choosing_an_edit() {
    for source in [
        "module main; fn helper() { () } fn heaper() { () } pub fn main() { heper() }",
        "module main; pub type Helper = .; pub type Heaper = .; pub fn main(value: Heper) { value }",
    ] {
        let (_dir, mut lsp, uri, diagnostic) = open_case(source, "module support;", 13);
        assert!(
            diagnostic["data"]["help"]
                .as_str()
                .unwrap()
                .contains("did you mean")
        );
        assert_no_eager_fix(&mut lsp, &uri, &diagnostic);
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn ordinary_alias_and_qualified_heads_replace_only_the_selected_identifier() {
    for (source, typo, replacement) in [
        (
            "module main; pub type Helper = .; pub fn main(value: Hepler) { value }",
            "Hepler",
            "Helper",
        ),
        (
            "module main; import support as support; pub fn main() { suport. // 😀 keep member\n helper() }",
            "suport",
            "support",
        ),
        (
            "module main; pub newtype Wrapper : . { constructor mk; projector get; }; type Alias = Wrapper; pub fn main() -> Wrapper { Alia.mk(()) }",
            "Alia.",
            "Alias.",
        ),
        (
            "module main; pub newtype Wrapper : . { constructor mk; projector get; }; type Alias = Wrapper; pub fn main() -> Wrapper { ().>Alia.mk }",
            "Alia.",
            "Alias.",
        ),
    ] {
        let (_dir, mut lsp, uri, diagnostic) =
            open_case(source, "module support; pub fn helper() { () }", 13);
        assert_repair(&mut lsp, &uri, &diagnostic, source, typo, replacement);
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn label_reference_ties_and_noncompact_paths_have_no_machine_edit() {
    for (source, provider) in [
        (
            "module main; labels { helper: ., heaper: . }; pub fn main() { let _ = {heper = ()}; () }",
            "module support;",
        ),
        (
            "module main; import support as support; pub fn main() { let _ = {support . hepler = ()}; () }",
            "module support; pub labels { helper: . };",
        ),
        (
            "module main; import support as support; pub fn main() { let _ = {support. // 😀 preserve\nhepler = ()}; () }",
            "module support; pub labels { helper: . };",
        ),
    ] {
        let (_dir, mut lsp, uri, diagnostic) = open_case(source, provider, 13);
        assert!(
            diagnostic["data"]["help"]
                .as_str()
                .unwrap()
                .contains("did you mean")
        );
        assert_no_eager_fix(&mut lsp, &uri, &diagnostic);
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn unique_label_reference_and_selective_import_edits_apply_and_recheck() {
    for (source, provider, code, typo, replacement) in [
        (
            "module main; pub labels { helper: . }; pub fn main() -> Helper { {hepler = ()} }",
            "module support;",
            13,
            "hepler",
            "helper",
        ),
        (
            "module main; import support as support; pub fn main() -> support.Helper { {support.hepler = ()} }",
            "module support; pub labels { helper: . };",
            13,
            "hepler",
            "helper",
        ),
        (
            "module main;\r\nimport support({ // 😀 preserve braces\r\n hepler // and trailing comment\r\n});\r\npub fn main() { let _ = {helper = ()}; () }\r\n",
            "module support; pub labels { helper: . };",
            12,
            "hepler",
            "helper",
        ),
    ] {
        let (_dir, mut lsp, uri, diagnostic) = open_case(source, provider, code);
        assert_repair(&mut lsp, &uri, &diagnostic, source, typo, replacement);
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn selective_label_import_preserves_visibility_namespace_and_tie_boundaries() {
    for provider in [
        "module support; labels { helper: . };",
        "module support; pub(support) labels { helper: . };",
        "module support; pub labels { unrelated: . };",
        "module support; pub labels { helper: ., heaper: . };",
        "module support; pub fn heper() { () }",
    ] {
        let source = "module main; import support({heper}); pub fn main() { () }";
        let (_dir, mut lsp, uri, diagnostic) = open_case(source, provider, 12);
        assert_no_eager_fix(&mut lsp, &uri, &diagnostic);
        assert_eq!(lsp.shutdown(), 0);
    }
}

fn provider_growth_rejects_label_fix(selective: bool, typo: &str, added: &str) {
    let (source, code) = if selective {
        (
            format!("module main; import support({{{typo}}}); pub fn main() {{ () }}"),
            12,
        )
    } else {
        (
            format!(
                "module main; import support as support; pub fn main() {{ let _ = {{support.{typo} = ()}}; () }}"
            ),
            13,
        )
    };
    let (dir, mut lsp, uri, diagnostic) =
        open_case(&source, "module support; pub labels { helper: . };", code);
    let initial = send_code_action(&mut lsp, &uri, &diagnostic);
    assert_eq!(initial["result"].as_array().unwrap().len(), 1, "{initial}");
    let provider_uri = path_to_file_uri(&dir.path().join("support.kio"));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": provider_uri, "languageId": "kio", "version": 1,
            "text": format!("module support; pub labels {{ helper: ., {added}: . }};")}}),
    );
    for kind in ["quickfix", "source.fixAll"] {
        let response =
            send_code_actions_for_kind(&mut lsp, &uri, std::slice::from_ref(&diagnostic), kind);
        assert_eq!(response["result"], json!([]), "{response}");
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn label_reference_provider_growth_invalidates_equal_and_nearer_candidates() {
    provider_growth_rejects_label_fix(false, "heper", "heaper");
    provider_growth_rejects_label_fix(false, "helpre", "helpr");
}

#[test]
fn label_reference_fix_survives_unrelated_provider_growth() {
    let source = "module main; import support as support; \
                  pub fn main() { let _ = {support.heper = ()}; () }";
    let (dir, mut lsp, uri, diagnostic) =
        open_case(source, "module support; pub labels { helper: . };", 13);
    let initial = send_code_action(&mut lsp, &uri, &diagnostic);
    assert_eq!(initial["result"].as_array().unwrap().len(), 1, "{initial}");
    let provider_uri = path_to_file_uri(&dir.path().join("support.kio"));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": provider_uri, "languageId": "kio", "version": 1,
            "text": "module support; pub labels { helper: ., unrelated: . };"}}),
    );
    assert_repair(&mut lsp, &uri, &diagnostic, source, "heper", "helper");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn selective_label_import_provider_growth_invalidates_equal_and_nearer_candidates() {
    provider_growth_rejects_label_fix(true, "heper", "heaper");
    provider_growth_rejects_label_fix(true, "helpre", "helpr");
}
