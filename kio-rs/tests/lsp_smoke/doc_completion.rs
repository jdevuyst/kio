use super::*;

fn replace_and_complete(
    lsp: &mut LspProcess,
    uri: &str,
    version: i32,
    marked: &str,
) -> (String, Value) {
    let offset = marked.find('¦').unwrap();
    let source = marked.replacen('¦', "", 1);
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": source }],
        }),
    );
    let (line, character) = source_position(&source, offset);
    let result = send_completion(lsp, uri, line, character);
    (source, result)
}

#[test]
fn structured_doc_completion_protocol_edits_and_retraction() {
    let dir = TempDir::new("doc-completion-protocol");
    dir.write("app.pkg.kio", "package app;\n");
    let path = dir.write("app.kio", "module app;\n");
    let uri = path_to_file_uri(&path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, "module app;\n");
    for (index, (body, expected)) in [
        ("[`@¦", vec!["signature", "source", "type"]),
        ("[`@ty¦pe`]", vec!["type"]),
        ("é😀 [`va¦lue`]", vec!["value"]),
        ("[`@type ¦`]", vec!["documented", "value"]),
        ("[`@signature Sh¦ape`]", vec!["Shape"]),
        ("ordinary va¦lue", vec![]),
        ("`[`va¦lue`]`", vec![]),
        ("\\[`va¦lue`]", vec![]),
        ("[text](https://example.test/[`va¦lue`])", vec![]),
        ("[`va¦lue`](destination)", vec![]),
        ("[`va¦lue`]\r\n/// [value]: https://example.test", vec![]),
        (
            "[`@type va¦lue`]\r\n/// [value]: https://example.test",
            vec![],
        ),
        ("[`@unknown va¦lue`]", vec![]),
        ("[`@type.va¦lue`]", vec![]),
        ("```kio {ignore}\r\n/// va¦\r\n/// ```", vec![]),
        ("```kio {@}\r\n/// va¦\r\n/// ```", vec!["value"]),
    ]
    .into_iter()
    .enumerate()
    {
        let marked = format!(
            "module app;\r\ntype Shape = .;\r\n/// {body}\r\nfn documented(parameter: .) -> . {{ parameter }}\r\nfn value() -> . {{ () }}\r\n"
        );
        let (source, response) = replace_and_complete(&mut lsp, &uri, index as i32 + 2, &marked);
        let labels = completion_labels(&response);
        // Ordinary Kio completion returns its complete lexical menu.
        if body.starts_with("```kio {@}") {
            assert!(labels.contains(&"value"), "{body}: {response}");
            assert!(!labels.contains(&"parameter"), "{response}");
        } else {
            assert_eq!(labels, expected, "{body}: {response}");
        }
        for item in response["items"].as_array().unwrap() {
            let changed = apply_lsp_text_edits(&source, &[item["textEdit"].clone()]);
            let tokens = |text: &str| {
                kio_lang::pass::lexer::lex(text)
                    .unwrap()
                    .iter()
                    .map(|token| {
                        (
                            format!("{:?}", token.kind),
                            text[token.span.start as usize..token.span.end as usize].to_owned(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(tokens(&changed), tokens(&source), "{body}: {item}");
            assert_eq!(
                changed.matches("\r\n").count(),
                source.matches("\r\n").count()
            );
        }
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn structured_doc_completion_protocol_current_provider_scope() {
    let dir = TempDir::new("doc-completion-provider");
    dir.write("app.pkg.kio", "package app;\n");
    let provider = "module app/provider;\n/// Current provider docs.\npub fn value() -> . { () }\npub type Shape = .;\nfn private() -> . { () }\n";
    let provider_path = dir.write("app/provider.kio", provider);
    let source = "module app; import app/provider as p; import app/provider(value);\n/// [`p.`]\nfn documented() -> . { () }\n";
    let path = dir.write("app.kio", source);
    let uri = path_to_file_uri(&path);
    let provider_uri = path_to_file_uri(&provider_path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    open_and_wait_for_analysis(&mut lsp, &provider_uri, provider);
    for (index, (body, expected)) in [
        ("[`p.¦`]", vec!["p.Shape", "p.value"]),
        (
            "[`app/provider.¦`]",
            vec!["app/provider.Shape", "app/provider.value"],
        ),
        ("[`@type p.¦`]", vec!["p.value"]),
        ("[`va¦lue`]", vec!["value"]),
    ]
    .into_iter()
    .enumerate()
    {
        let marked = source.replace("[`p.`]", body);
        let (_, response) = replace_and_complete(&mut lsp, &uri, index as i32 + 2, &marked);
        assert_eq!(completion_labels(&response), expected, "{response}");
    }
    let duplicate = source.replacen(
        "import app/provider as p;",
        "import app/provider as p; import app/provider as p;",
        1,
    );
    let (_, result) =
        replace_and_complete(&mut lsp, &uri, 8, &duplicate.replace("[`p.`]", "[`p.¦`]"));
    assert!(
        completion_labels(&result).is_empty(),
        "ambiguous alias: {result}"
    );
    assert_eq!(result["isIncomplete"], true);
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": provider_uri, "version": 2 },
            "contentChanges": [{ "text": provider.replace("value", "replacement") }],
        }),
    );
    let (_, response) =
        replace_and_complete(&mut lsp, &uri, 10, &source.replace("[`p.`]", "[`p.¦`]"));
    assert_eq!(completion_labels(&response), ["p.Shape", "p.replacement"]);
    let (_, response) =
        replace_and_complete(&mut lsp, &uri, 11, &source.replace("[`p.`]", "[`va¦`]"));
    assert!(
        completion_labels(&response).is_empty(),
        "stale import: {response}"
    );
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": provider_uri, "version": 3 },
            "contentChanges": [{ "text": "module renamed; pub fn value() -> . { () }" }],
        }),
    );
    let (_, response) =
        replace_and_complete(&mut lsp, &uri, 12, &source.replace("[`p.`]", "[`p.¦`]"));
    assert!(completion_labels(&response).is_empty());
    assert_eq!(response["isIncomplete"], true);
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn structured_doc_completion_protocol_requires_module_attachment() {
    let dir = TempDir::new("doc-completion-attachment");
    dir.write("app.pkg.kio", "package app;\n");
    let path = dir.write("app.kio", "module app;\n");
    let uri = path_to_file_uri(&path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, "module app;\n");
    for (index, marked) in [
        "module app; fn value() {} fn outer() {\n/// [`va¦lue`]\nvalue() }",
        "module app; fn value() {} fn outer() {\n/// [`va¦lue`]",
        "module app; fn outer() {\n/// [`@ty¦pe`]\n() }",
        "module app; fn value() {} fn outer() {\n/// ```kio {@}\n/// va¦\n/// ```\n() }",
    ]
    .into_iter()
    .enumerate()
    {
        let (_, result) = replace_and_complete(&mut lsp, &uri, index as i32 + 2, marked);
        assert!(completion_labels(&result).is_empty(), "{marked}: {result}");
    }
    for (index, marked) in [
        "module app; fn value() { () }\n/// [`va¦lue`]",
        "module app; fn value() {}\n/// [`va¦lue`]",
        "module app; fn value() {}\n/// [`va¦",
    ]
    .into_iter()
    .enumerate()
    {
        let (_, result) = replace_and_complete(&mut lsp, &uri, index as i32 + 10, marked);
        assert_eq!(completion_labels(&result), ["value"], "{marked}: {result}");
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn structured_doc_imported_operator_edit_passes_batch_checker() {
    let dir = TempDir::new("doc-completion-operator");
    dir.write(
        "app.pkg.kio",
        "package app; build { cache (); docs { md \".\" } } bridge { app; app/** }",
    );
    let provider = "module app/provider; pub fn pair(a: ., b: .) -> . { a } pub fn identity(value: .) -> . { value } pub op _ + _ { impl pair }; pub op + _ { impl identity }; op _ - _ { impl pair };";
    let provider_path = dir.write("app/provider.kio", provider);
    let source =
        "module app; import app/provider(op _ + _);\n/// [`+`]\nfn documented() -> . { () }\n";
    let path = dir.write("app.kio", source);
    let uri = path_to_file_uri(&path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    open_and_wait_for_analysis(&mut lsp, &path_to_file_uri(&provider_path), provider);
    for (index, (selector, expected)) in [
        ("+¦", vec!["+"]),
        ("op _ +¦", vec!["op _ + _"]),
        ("pa¦", vec![]),
        ("-¦", vec![]),
    ]
    .into_iter()
    .enumerate()
    {
        let marked = source.replace("[`+`]", &format!("[`{selector}`]"));
        let (current, result) = replace_and_complete(&mut lsp, &uri, index as i32 + 2, &marked);
        assert_eq!(completion_labels(&result), expected, "{selector}: {result}");
        for item in result["items"].as_array().unwrap() {
            let applied = apply_lsp_text_edits(&current, &[item["textEdit"].clone()]);
            fs::write(&path, applied).unwrap();
            let checked = Command::new(test_binary!("kio"))
                .args(["doc", "check"])
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                checked.status.success(),
                "inserted {item}: {}",
                String::from_utf8_lossy(&checked.stderr)
            );
        }
    }
    let ambiguous = source
        .replace("op _ + _);", "op _ + _, op + _);")
        .replace("[`+`]", "[`+¦`]");
    let (_, result) = replace_and_complete(&mut lsp, &uri, 9, &ambiguous);
    assert!(
        completion_labels(&result).is_empty(),
        "ambiguous token alias: {result}"
    );
    let hidden = source
        .replace("op _ + _);", "op _ - _);")
        .replace("[`+`]", "[`-¦`]");
    let (_, result) = replace_and_complete(&mut lsp, &uri, 10, &hidden);
    assert!(
        completion_labels(&result).is_empty(),
        "private operator: {result}"
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn structured_doc_completion_withholds_cross_provider_identity_collisions() {
    let dir = TempDir::new("doc-completion-import-identities");
    dir.write("app.pkg.kio", "package app;\n");
    let left = "module app/left;\n/// Left declaration.\npub fn value() -> . { () }\npub fn pair(a: ., b: .) -> . { a }\npub op _ + _ { impl pair }\n";
    let right = "module app/right;\n/// Right declaration.\npub fn value(arg: .) -> . { arg }\npub fn identity(arg: .) -> . { arg }\npub op + _ { impl identity }\n";
    let left_path = dir.write("app/left.kio", left);
    let right_path = dir.write("app/right.kio", right);
    let source = "module app; import app/left as left; import app/right as right; import app/left(value, op _ + _);\n/// [`value`]\nfn documented() -> . { () }\n";
    let path = dir.write("app.kio", source);
    let uri = path_to_file_uri(&path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    open_and_wait_for_analysis(&mut lsp, &path_to_file_uri(&left_path), left);
    open_and_wait_for_analysis(&mut lsp, &path_to_file_uri(&right_path), right);
    let mut version = 2;
    for imports in [
        "import app/left(value); import app/right(value);",
        "import app/right(value); import app/left(value);",
    ] {
        let marked = source
            .replace("import app/left(value, op _ + _);", imports)
            .replace("[`value`]", "[`va¦lue`]");
        let (_, result) = replace_and_complete(&mut lsp, &uri, version, &marked);
        version += 1;
        assert!(completion_labels(&result).is_empty(), "{imports}: {result}");
        assert_eq!(result["isIncomplete"], true);
    }
    let operators = source.replace(
        "import app/left(value, op _ + _);",
        "import app/left(op _ + _); import app/right(op + _);",
    );
    for (selector, expected) in [
        ("+¦", vec![]),
        ("op _ +¦", vec!["op _ + _"]),
        ("op +¦", vec!["op + _"]),
    ] {
        let marked = operators.replace("[`value`]", &format!("[`{selector}`]"));
        let (_, result) = replace_and_complete(&mut lsp, &uri, version, &marked);
        version += 1;
        assert_eq!(completion_labels(&result), expected, "{selector}: {result}");
    }
    for (imports, local, expected_doc) in [
        (
            "import app/left(value); import app/left(value);",
            "",
            "Left declaration.",
        ),
        (
            "import app/left(value); import app/right(value);",
            "/// Local declaration.\nfn value() -> . { () }\n",
            "Local declaration.",
        ),
    ] {
        let marked = format!(
            "{}{local}",
            source
                .replace("import app/left(value, op _ + _);", imports)
                .replace("[`value`]", "[`va¦lue`]")
        );
        let (_, result) = replace_and_complete(&mut lsp, &uri, version, &marked);
        version += 1;
        assert_eq!(completion_labels(&result), ["value"], "{result}");
        assert_eq!(result["items"][0]["documentation"]["value"], expected_doc);
    }
    assert_eq!(lsp.shutdown(), 0);
}
