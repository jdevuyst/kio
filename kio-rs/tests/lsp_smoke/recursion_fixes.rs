use super::*;

fn assert_recursion_repair(body: &str, repaired_body: &str) {
    let dir = TempDir::new("recursion-call-fix");
    dir.write_pkg_root_package();
    dir.write(
        "pkg.kio",
        "module pkg; host fn drive[S][R](step: S -> (S | R), state: S) -> R;\n",
    );
    let source = format!(
        "module pkg/main;\nimport pkg as host;\npub rec(host.drive) fn again[A](x: A) -> . {{ {body} }}\n"
    );
    let repaired = source.replace(body, repaired_body);
    let file = dir.write("pkg/main.kio", &source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostics = publish["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{publish}");
    assert_eq!(diagnostics[0]["code"], 11, "{publish}");
    let response = send_code_action(&mut lsp, &uri, &diagnostics[0]);
    let actions = response["result"].as_array().unwrap();
    assert_eq!(
        actions.len(),
        1,
        "expected one validated recursion repair: {response}"
    );
    let (edits, version) = action_edits_for_uri(&actions[0], &uri);
    assert_eq!(version, Some(1), "{response}");
    let applied = apply_lsp_text_edits(&source, edits);
    assert_eq!(applied, repaired, "{response}");
    send_full_text_change(&mut lsp, &uri, 2, &applied);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert_eq!(cleared["diagnostics"], json!([]), "{cleared}");
    let stale = send_code_action(&mut lsp, &uri, &diagnostics[0]);
    assert_eq!(stale["result"], json!([]), "{stale}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn missing_tail_marker() {
    assert_recursion_repair("again(x)", "rec again(x)");
}

#[test]
fn missing_marker_preserves_unicode_comments_and_tabs() {
    assert_recursion_repair(
        "// 😀 recursive call\n\tagain(x)",
        "// 😀 recursive call\n\trec again(x)",
    );
}

#[test]
fn missing_non_tail_marker() {
    assert_recursion_repair(
        "let result = again(x); result",
        "let result = rec(cont) again(x); result",
    );
}

#[test]
fn missing_polymorphic_marker() {
    assert_recursion_repair("again(A & A, (x, x))", "rec(poly) again(A & A, (x, x))");
}

#[test]
fn missing_polymorphic_non_tail_marker() {
    assert_recursion_repair(
        "let result = again(A & A, (x, x)); result",
        "let result = rec(poly, cont) again(A & A, (x, x)); result",
    );
}

#[test]
fn mode_missing_poly() {
    assert_recursion_repair("rec again(A & A, (x, x))", "rec(poly) again(A & A, (x, x))");
}

#[test]
fn mode_missing_cont() {
    assert_recursion_repair(
        "let result = rec again(x); result",
        "let result = rec(cont) again(x); result",
    );
}

#[test]
fn mode_redundant_poly() {
    assert_recursion_repair("rec(poly) again(x)", "rec again(x)");
}

#[test]
fn mode_redundant_cont() {
    assert_recursion_repair("rec(cont) again(x)", "rec again(x)");
}

#[test]
fn mode_replace_poly_with_cont_atomically() {
    assert_recursion_repair(
        "let result = rec(poly) again(x); result",
        "let result = rec(cont) again(x); result",
    );
}

#[test]
fn mode_replace_cont_with_poly_atomically() {
    assert_recursion_repair(
        "rec(cont) again(A & A, (x, x))",
        "rec(poly) again(A & A, (x, x))",
    );
}

#[test]
fn mode_repairs_preserve_unicode_comments_and_tabs() {
    assert_recursion_repair(
        "// 😀 call\nrec(// keep 😀\n\tpoly// keep suffix\n) again(x)",
        "// 😀 call\nrec// keep 😀\n\t// keep suffix\n again(x)",
    );
    assert_recursion_repair(
        "let result = rec(// keep 😀\n\tpoly) again(x); result",
        "let result = rec(// keep 😀\n\tcont) again(x); result",
    );
}

#[test]
fn mode_removal_keeps_adjacent_keyword_and_callee_separate() {
    assert_recursion_repair("rec(poly)again(x)", "rec again(x)");
}

#[test]
fn mode_combined_annotations_are_repaired_without_losing_the_other_mode() {
    for (body, repaired) in [
        (
            "let result = rec(cont) again(A & A, (x, x)); result",
            "let result = rec(poly, cont) again(A & A, (x, x)); result",
        ),
        (
            "let result = rec(poly) again(A & A, (x, x)); result",
            "let result = rec(poly, cont) again(A & A, (x, x)); result",
        ),
        (
            "let result = rec again(A & A, (x, x)); result",
            "let result = rec(poly, cont) again(A & A, (x, x)); result",
        ),
        ("rec(poly, cont) again(x)", "rec  again(x)"),
        (
            "rec(poly, cont) again(A & A, (x, x))",
            "rec(poly, ) again(A & A, (x, x))",
        ),
    ] {
        assert_recursion_repair(body, repaired);
    }
}

#[test]
fn mode_repairs_that_expose_other_errors_are_withheld() {
    for (capability, body) in [
        ("host.drive", "rec(poly) again(())"),
        ("absent", "rec(poly) again(x)"),
        ("host.drive", "rec(poly) again(x); rec(poly) again(x)"),
        (
            "host.drive",
            "let callback = .() -> . { rec(poly) again(x) }; callback()",
        ),
        ("host.drive", "rec(escape) again(x)"),
    ] {
        let source = format!(
            "module pkg/main; import pkg as host; pub rec({capability}) fn again[A](x: A) -> . {{ {body} }}"
        );
        recursion_actions(&source, |diagnostic, response| {
            assert_eq!(
                response["result"],
                json!([]),
                "{body}: {diagnostic}; {response}"
            );
        });
    }
}

fn recursion_actions(source: &str, inspect: impl FnOnce(&Value, &Value)) {
    let dir = TempDir::new("recursion-fix-refusal");
    dir.write_pkg_root_package();
    dir.write(
        "pkg.kio",
        "module pkg; host fn drive[S][R](step: S -> (S | R), state: S) -> R;\n",
    );
    let file = dir.write("pkg/main.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostic = &publish["diagnostics"][0];
    assert!(diagnostic.is_object(), "{publish}");
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    inspect(diagnostic, &response);
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn invalid_marker_repairs_are_withheld() {
    for (capability, body) in [
        ("host.drive", "again(())"),
        ("absent", "again(x)"),
        ("host.drive", "again(x); again(x)"),
        (
            "host.drive",
            "let callback = .() -> . { again(x) }; callback()",
        ),
        ("host.drive", "rec(escape) again(x)"),
    ] {
        let source = format!(
            "module pkg/main; import pkg as host; pub rec({capability}) fn again[A](x: A) -> . {{ {body} }}"
        );
        recursion_actions(&source, |diagnostic, response| {
            assert_eq!(
                response["result"],
                json!([]),
                "{body}: {diagnostic}; {response}"
            );
        });
    }
}

#[test]
fn missing_definition_guidance_does_not_choose_a_capability() {
    for modifier in ["", "pure "] {
        let source =
            format!("module pkg/main; pub {modifier}fn again(x: .) -> . {{ rec again(x) }}");
        recursion_actions(&source, |diagnostic, response| {
            assert!(
                diagnostic["data"]["help"]
                    .as_str()
                    .unwrap()
                    .contains("where `path` names your chosen loop capability"),
                "{diagnostic}"
            );
            assert_eq!(response["result"], json!([]), "{response}");
        });
    }
    recursion_actions(
        "module pkg/main; pub fn again(x: .) -> . { again(x) }",
        |diagnostic, _response| {
            assert!(
                diagnostic["data"]["notes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|note| note.as_str().unwrap().contains("own name is not in scope")),
                "{diagnostic}"
            );
            assert!(
                diagnostic["data"]["fixes"]
                    .as_array()
                    .is_none_or(Vec::is_empty),
                "definition must not invent a loop capability: {diagnostic}"
            );
        },
    );
}

#[test]
fn shadowed_member_names_do_not_get_recursion_repairs() {
    let dir = TempDir::new("recursion-fix-shadowing");
    dir.write_pkg_root_package();
    dir.write(
        "pkg.kio",
        "module pkg; host fn drive[S][R](step: S -> (S | R), state: S) -> R;\n",
    );
    let source = "module pkg/main; import pkg as host; pub rec(host.drive) fn again[A](x: A) -> A { let again = .(value: A) { value }; again(x) }";
    let file = dir.write("pkg/main.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let position = source_position(source, source.rfind("again(x)").unwrap());
    let hover = wait_for_hover(&mut lsp, &uri, position.0, position.1);
    assert_eq!(hover_type(&hover), "A -> A", "{hover}");
    assert_eq!(lsp.shutdown(), 0);
}
