use super::*;

#[test]
fn lsp_host_unit_default_retains_function_types() {
    let dir = TempDir::new("host-unit-default");
    dir.write_pkg_root_package();
    let source = "module pkg/test;\n\
host fn implicit();\n\
host fn explicit() -> .;\n\
host fn staged()[A];\n\
pub fn run() {\n\
  let first = implicit;\n\
  let second = explicit;\n\
  let third = staged;\n\
  first(); second(); third((), .)\n\
}\n";
    let uri = path_to_file_uri(&dir.write("pkg/test.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    for (use_site, expected) in [
        ("implicit;", ". -> ."),
        ("explicit;", ". -> ."),
        ("staged;", ". -> [A] ."),
    ] {
        let (line, character) = source_position(source, source.find(use_site).unwrap());
        let hover = wait_for_hover(&mut lsp, &uri, line, character);
        assert_eq!(hover_type(&hover), expected, "{use_site}: {hover}");
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_type_only_values_recover_and_preserve_binding_types() {
    let dir = TempDir::new("type-only-values");
    dir.write_pkg_root_package();
    let source = "module pkg/test;\n\
pub fn mark[A] { () }\n\
pub fn delayed[A]() { () }\n\
pub fn identity[A] -> A -> A { .(value: A) { value } }\n\
pub fn run() {\n\
  let saved = mark;\n\
  let ready = saved(.);\n\
  let anonymous = .[B] { .(value: B) { value } };\n\
  let wait = delayed(.);\n\
  ready;\n\
  anonymous(., ());\n\
  identity(., ());\n\
  wait()\n\
}\n";
    let broken = source.replacen("mark[A]", "mark[A", 1);
    let uri = path_to_file_uri(&dir.write("pkg/test.kio", &broken));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": broken}}),
    );
    let initial = wait_for_publish_version(&mut lsp, &uri, 1);
    assert!(
        initial["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["severity"] == 1 && diagnostic["code"] == 11),
        "{initial}"
    );
    send_full_text_change(&mut lsp, &uri, 2, source);
    let recovered = wait_for_publish_version(&mut lsp, &uri, 2);
    assert_eq!(recovered["diagnostics"], json!([]), "{recovered}");

    for (use_site, length, expected) in [
        ("mark;", 4, "[A] ."),
        ("saved(.)", 5, "[A] ."),
        ("delayed(.)", 7, "[A] . -> ."),
        ("anonymous(., ())", 9, "[B] B -> B"),
        ("ready;", 5, "."),
        ("wait()", 4, ". -> ."),
    ] {
        let offset = source.find(use_site).unwrap();
        let (line, character) = source_position(source, offset);
        let hover = wait_for_hover(&mut lsp, &uri, line, character);
        assert_eq!(hover_type(&hover), expected, "{use_site}: {hover}");
        assert_eq!(hover_range(&hover), source_range(source, offset, length));
    }

    for (use_site, use_skip, declaration, declaration_skip, length) in [
        ("mark;", 0, "fn mark", 3, 4),
        ("value: A", 7, "identity[A", 9, 1),
        ("value: B", 7, ".[B]", 2, 1),
    ] {
        let offset = source.find(use_site).unwrap() + use_skip;
        let (line, character) = source_position(source, offset);
        let definition = send_definition(&mut lsp, &uri, line, character);
        assert_eq!(definition["uri"], uri, "{definition}");
        assert_eq!(
            json_range(&definition["range"]),
            source_range(
                source,
                source.find(declaration).unwrap() + declaration_skip,
                length
            ),
            "{use_site}: {definition}"
        );
    }

    let offset = source.find("saved(.)").unwrap();
    let (line, character) = source_position(source, offset + 2);
    let completion = send_completion(&mut lsp, &uri, line, character);
    let item = completion["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"] == "saved")
        .unwrap_or_else(|| panic!("{completion}"));
    assert_eq!(item["detail"], "[A] .", "{item}");
    assert_eq!(
        json_range(&item["textEdit"]["range"]),
        source_range(source, offset, "saved".len())
    );
    assert!(!completion_labels(&completion).contains(&"ready"));
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_expression_hover_preserves_type_structure_without_redundant_parentheses() {
    let dir = TempDir::new("type-display-hover");
    dir.write_pkg_root_package();
    let source = "module pkg/test;\n\
fn right[A][B][C](x: A & B & C) -> A & B & C { x }\n\
fn left[A][B][C](x: (A & B) & C) -> (A & B) & C { x }\n\
fn sum_right[A][B][C](x: A | B | C) -> A | B | C { x }\n\
fn sum_left[A][B][C](x: (A | B) | C) -> (A | B) | C { x }\n\
fn mixed[A][B][C](x: A & (B | C)) -> A & (B | C) { x }\n\
fn arrow[A][B][C](x: (A & B) -> C) -> (A & B) -> C { x }\n\
fn poly(x: [A] A -> A) -> [A] A -> A { x }\n";
    let file = dir.write("pkg/test.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    for (line, expected) in [
        "A & B & C",
        "(A & B) & C",
        "A | B | C",
        "(A | B) | C",
        "A & (B | C)",
        "(A & B) -> C",
        "[A] A -> A",
    ]
    .into_iter()
    .enumerate()
    {
        let text = source.lines().nth(line + 1).unwrap();
        let column = text.rfind("{ x }").unwrap() + 2;
        let hover = wait_for_hover(&mut lsp, &uri, (line + 1) as u64, column as u64);
        assert_eq!(hover_type(&hover), expected, "{hover}");
        assert_eq!(
            hover_range(&hover),
            (
                (line + 1) as u64,
                column as u64,
                (line + 1) as u64,
                column as u64 + 1
            )
        );
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_type_display_is_shared_by_inlay_completion_and_signature_help() {
    let dir = TempDir::new("type-display-consumers");
    dir.write_pkg_root_package();
    let source = "module pkg/test;\n\
fn packet(x: . & . & .) -> . & . & . { x }\n\
fn run() -> . & . & . { let triple = ((), (), ()); packet(triple) }\n";
    let file = dir.write("pkg/test.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let use_offset = source.rfind("triple").unwrap();
    let pos = source_position(source, use_offset);
    let hover = wait_for_hover(&mut lsp, &uri, pos.0, pos.1);
    assert_eq!(hover_type(&hover), ". & . & .", "{hover}");

    let hints = send_inlay_hints(&mut lsp, &uri);
    assert!(
        hints
            .as_array()
            .unwrap()
            .iter()
            .any(|hint| hint["label"] == ": . & . & ."),
        "{hints}"
    );

    let pos = source_position(source, use_offset + 3);
    let completion = send_completion(&mut lsp, &uri, pos.0, pos.1);
    let item = completion["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"] == "triple")
        .unwrap_or_else(|| panic!("{completion}"));
    assert_eq!(item["detail"], ". & . & .", "{item}");

    let pos = source_position(source, use_offset);
    let signature = wait_for_signature_help(&mut lsp, &uri, pos.0, pos.1);
    assert_eq!(
        signature["signatures"][0]["label"], "fn packet(x: . & . & .) -> . & . & .",
        "{signature}"
    );
    assert_eq!(lsp.shutdown(), 0);
}
