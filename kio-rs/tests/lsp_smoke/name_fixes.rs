use super::*;

fn open_case(source: &str, provider: &str, code: i32) -> (TempDir, LspProcess, String, Value) {
    let dir = TempDir::new("name-fix");
    dir.write("probe.pkg.kio", "pkg probe;\n");
    dir.write("support.kio", provider);
    let path = dir.write("main.kio", "mod main;\n");
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
        "mod main; fn helper() { () } fn heaper() { () } pub fn main() { heper() }",
        "mod main; pub type Helper = .; pub type Heaper = .; pub fn main(value: Heper) { value }",
    ] {
        let (_dir, mut lsp, uri, diagnostic) = open_case(source, "mod support;", 13);
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
            "mod main; pub type Helper = .; pub fn main(value: Hepler) { value }",
            "Hepler",
            "Helper",
        ),
        (
            "mod main; import support as support; pub fn main() { suport. // 😀 keep member\n helper() }",
            "suport",
            "support",
        ),
        (
            "mod main; pub type { Wrapper: . { mk; get; }; }; type Alias = Wrapper; pub fn main() -> Wrapper { Alia.mk(()) }",
            "Alia.",
            "Alias.",
        ),
        (
            "mod main; pub type { Wrapper: . { mk; get; }; }; type Alias = Wrapper; pub fn main() -> Wrapper { ().>Alia.mk }",
            "Alia.",
            "Alias.",
        ),
    ] {
        let (_dir, mut lsp, uri, diagnostic) =
            open_case(source, "mod support; pub fn helper() { () }", 13);
        assert_repair(&mut lsp, &uri, &diagnostic, source, typo, replacement);
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn nominal_reference_ties_keep_help_without_choosing_an_edit() {
    let source =
        "mod main; pub type { Helper: .; Heaper: . } pub fn main(value: Heper) -> Helper { value }";
    let (_dir, mut lsp, uri, diagnostic) = open_case(source, "mod support;", 13);
    assert!(
        diagnostic["data"]["help"]
            .as_str()
            .unwrap()
            .contains("did you mean")
    );
    assert_no_eager_fix(&mut lsp, &uri, &diagnostic);
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn unique_nominal_reference_and_selective_import_edits_apply_and_recheck() {
    for (source, provider, code, typo, replacement) in [
        (
            "mod main; pub type { Helper: . } pub fn main(value: Hepler) -> Helper { value }",
            "mod support;",
            13,
            "Hepler",
            "Helper",
        ),
        (
            "mod main; import support as support; pub fn main(value: suport.Helper) -> support.Helper { value }",
            "mod support; pub type { Helper: . }",
            13,
            "suport",
            "support",
        ),
        (
            "mod main; import support as support; pub fn main(value: suport. // 😀 preserve\nHelper) -> support.Helper { value }",
            "mod support; pub type { Helper: . }",
            13,
            "suport",
            "support",
        ),
        (
            "mod main;\r\nimport support { // 😀 preserve selection\r\n type Hepler // and trailing comment\r\n}\r\npub fn main(value: Helper) -> Helper { value }\r\n",
            "mod support; pub type { Helper: . }",
            12,
            "Hepler",
            "Helper",
        ),
    ] {
        let (_dir, mut lsp, uri, diagnostic) = open_case(source, provider, code);
        assert_repair(&mut lsp, &uri, &diagnostic, source, typo, replacement);
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn selective_nominal_import_preserves_visibility_and_tie_boundaries() {
    for provider in [
        "mod support; type { Helper: . }",
        "mod support; pub(support) type { Helper: . }",
        "mod support; pub type { Unrelated: . }",
        "mod support; pub type { Helper: .; Heaper: . }",
    ] {
        let source = "mod main; import support {type Heper} pub fn main() { () }";
        let (_dir, mut lsp, uri, diagnostic) = open_case(source, provider, 12);
        assert_no_eager_fix(&mut lsp, &uri, &diagnostic);
        assert_eq!(lsp.shutdown(), 0);
    }
}

fn provider_growth_rejects_nominal_import_fix(typo: &str, added: &str) {
    let source = format!(
        "mod main; import support {{type {typo}}} pub fn main(value: Helper) -> Helper {{ value }}"
    );
    let (dir, mut lsp, uri, diagnostic) =
        open_case(&source, "mod support; pub type { Helper: . }", 12);
    let initial = send_code_action(&mut lsp, &uri, &diagnostic);
    assert_eq!(initial["result"].as_array().unwrap().len(), 1, "{initial}");
    let provider_uri = path_to_file_uri(&dir.path().join("support.kio"));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": provider_uri, "languageId": "kio", "version": 1,
            "text": format!("mod support; pub type {{ Helper: .; {added}: . }}")}}),
    );
    for kind in ["quickfix", "source.fixAll"] {
        let response =
            send_code_actions_for_kind(&mut lsp, &uri, std::slice::from_ref(&diagnostic), kind);
        assert_eq!(response["result"], json!([]), "{response}");
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn nominal_reference_fix_survives_unrelated_provider_growth() {
    let source = "mod main; import support {type Helper} \
                  pub fn main(value: Heper) -> Helper { value }";
    let (dir, mut lsp, uri, diagnostic) =
        open_case(source, "mod support; pub type { Helper: . }", 13);
    let initial = send_code_action(&mut lsp, &uri, &diagnostic);
    let initial_actions = initial["result"].as_array().unwrap();
    assert_eq!(initial_actions.len(), 2, "{initial}");
    let replacement = initial_actions
        .iter()
        .find(|action| !action["edit"].is_null())
        .expect("one eager nominal spelling repair");
    assert_eq!(replacement["title"], "Replace with `Helper`");
    assert_eq!(replacement["isPreferred"], true);
    let (edits, version) = action_edits_for_uri(replacement, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(edits.len(), 1);
    assert_eq!(
        apply_lsp_text_edits(source, edits),
        source.replacen("Heper", "Helper", 1)
    );
    let scaffold = initial_actions
        .iter()
        .find(|action| action["data"]["kind"] == "addStub")
        .expect("separate lazy declaration scaffold");
    assert_eq!(scaffold["title"], "Add `Heper` declaration");
    assert_eq!(scaffold["isPreferred"], false);
    assert!(scaffold["edit"].is_null());
    assert_eq!(scaffold["data"]["name"], "Heper");
    assert_eq!(scaffold["data"]["documentVersion"], 1);
    let provider_uri = path_to_file_uri(&dir.path().join("support.kio"));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": provider_uri, "languageId": "kio", "version": 1,
            "text": "mod support; pub type { Helper: .; Unrelated: . }"}}),
    );
    let after_growth = send_code_action(&mut lsp, &uri, &diagnostic);
    assert_eq!(after_growth["result"], initial["result"]);
    assert_repair(&mut lsp, &uri, &diagnostic, source, "Heper", "Helper");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn selective_nominal_import_provider_growth_invalidates_equal_and_nearer_candidates() {
    provider_growth_rejects_nominal_import_fix("Heper", "Heaper");
    provider_growth_rejects_nominal_import_fix("Helpre", "Helpr");
}
