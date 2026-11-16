use super::*;

fn open_module_path_case(
    importer: &str,
    source: &str,
    providers: &[(&str, &str)],
) -> (TempDir, LspProcess, String, Value) {
    let dir = TempDir::new("module-path-typo-fix");
    dir.write("probe.pkg.kio", "pkg probe;\n");
    for (name, contents) in providers {
        dir.write(&format!("{name}.kio"), contents);
    }
    let path = dir.write(&format!("{importer}.kio"), &format!("mod {importer};\n"));
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
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("not found in package"),
        "{publish}"
    );
    (dir, lsp, uri, diagnostic)
}

#[test]
fn module_path_typo_repairs_written_tokens_and_rechecks() {
    for (importer, source, expected, provider, edit_count) in [
        (
            "main",
            "mod main;\r\nimport utilx/ // 😀 keep this\r\n\thelpe as chosen;\r\npub fn main() { chosen.helper() }\r\n",
            "mod main;\r\nimport utils/ // 😀 keep this\r\n\thelper as chosen;\r\npub fn main() { chosen.helper() }\r\n",
            "utils/helper",
            2,
        ),
        (
            "nested/main",
            "mod nested/main; import vendor/util {fn helper} pub fn main() { helper() }",
            "mod nested/main; import vendor/utils {fn helper} pub fn main() { helper() }",
            "vendor/utils",
            1,
        ),
        (
            "main",
            "mod main; import a/bc as chosen; pub fn main() { chosen.helper() }",
            "mod main; import a/b/c as chosen; pub fn main() { chosen.helper() }",
            "a/b/c",
            1,
        ),
        (
            "main",
            "mod main; import a/b/c {fn helper} pub fn main() { helper() }",
            "mod main; import a/bc {fn helper} pub fn main() { helper() }",
            "a/bc",
            1,
        ),
        (
            "main",
            "mod main; import _heper {fn helper} pub fn main() { helper() }",
            "mod main; import _helper {fn helper} pub fn main() { helper() }",
            "_helper",
            1,
        ),
    ] {
        let provider_source = format!("mod {provider}; pub fn helper() {{ () }}");
        let (_dir, mut lsp, uri, diagnostic) =
            open_module_path_case(importer, source, &[(provider, &provider_source)]);
        assert_eq!(
            diagnostic["data"]["help"],
            format!("did you mean `{provider}`?")
        );
        let response = send_code_action(&mut lsp, &uri, &diagnostic);
        let actions = response["result"].as_array().unwrap();
        assert_eq!(actions.len(), 1, "{response}");
        assert_eq!(actions[0]["kind"], "quickfix");
        assert_eq!(
            actions[0]["title"],
            format!("Replace module path with `{provider}`")
        );
        let (edits, version) = action_edits_for_uri(&actions[0], &uri);
        assert_eq!(version, Some(1));
        assert_eq!(edits.len(), edit_count);
        let repaired = apply_lsp_text_edits(source, edits);
        assert_eq!(repaired, expected);
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
fn module_path_typo_keeps_unsafe_trivia_and_distant_names_prose_only() {
    for (written, provider) in [
        ("a / bc", "a/b/c"),
        ("a/ // 😀 keep\nbc", "a/b/c"),
        ("a/b / c", "a/bc"),
        ("missing", "unrelated"),
    ] {
        let source =
            format!("mod main; import {written} as chosen; pub fn main() {{ chosen.helper() }}");
        let provider_source = format!("mod {provider}; pub fn helper() {{ () }}");
        let (_dir, mut lsp, uri, diagnostic) =
            open_module_path_case("main", &source, &[(provider, &provider_source)]);
        assert!(diagnostic["data"]["fixes"].is_null(), "{diagnostic}");
        assert_eq!(
            send_code_action(&mut lsp, &uri, &diagnostic)["result"],
            json!([])
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn module_path_typo_does_not_choose_between_equally_near_paths() {
    let source = "mod main; import heper as chosen; pub fn main() { chosen.helper() }";
    let (_dir, mut lsp, uri, diagnostic) = open_module_path_case(
        "main",
        source,
        &[
            ("helper", "mod helper; pub fn helper() { () }"),
            ("heaper", "mod heaper; pub fn helper() { () }"),
        ],
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
fn module_path_typo_authenticates_candidates_after_module_growth() {
    for (typo, added, still_current) in [
        ("heper", "heaper", false),
        ("helpre", "helpr", false),
        ("heper", "unrelated", true),
    ] {
        let source =
            format!("mod main; import {typo} as chosen; pub fn main() {{ chosen.helper() }}");
        let (dir, mut lsp, uri, diagnostic) = open_module_path_case(
            "main",
            &source,
            &[("helper", "mod helper; pub fn helper() { () }")],
        );
        let initial = send_code_action(&mut lsp, &uri, &diagnostic);
        assert_eq!(initial["result"].as_array().unwrap().len(), 1, "{initial}");
        let provider_source = format!("mod {added}; pub fn helper() {{ () }}");
        let added_path = dir.write(&format!("{added}.kio"), &provider_source);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": path_to_file_uri(&added_path), "languageId": "kio", "version": 1,
                "text": provider_source}}),
        );
        let response = send_code_action(&mut lsp, &uri, &diagnostic);
        assert_eq!(
            response["result"].as_array().unwrap().len(),
            usize::from(still_current),
            "{response}"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn module_path_typo_withholds_repairs_that_expose_another_error() {
    for (source, provider_source) in [
        (
            "mod main; import supprt {fn missing} pub fn main() { missing() }",
            "mod support; pub fn helper() { () }",
        ),
        (
            "mod main; import supprt as chosen; pub fn main() { chosen.helper() }",
            "mod support; import main as consumer; pub fn helper() { consumer.main() }",
        ),
    ] {
        let (_dir, mut lsp, uri, diagnostic) =
            open_module_path_case("main", source, &[("support", provider_source)]);
        assert_eq!(
            diagnostic["data"]["fixes"][0]["edits"][0]["replacement"],
            "support"
        );
        assert_eq!(
            send_code_action(&mut lsp, &uri, &diagnostic)["result"],
            json!([])
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}
