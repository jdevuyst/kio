use super::*;

#[test]
fn lsp_block_snippets_require_client_capability() {
    let dir = TempDir::new("block-editor-plain-completion");
    dir.write("app.pkg.kio", "package app; bridge { app; provider; }");
    let provider = include_str!("../fixtures/trailing-blocks/provider.kio");
    let provider_file = dir.write("provider.kio", provider);
    let valid = "module app; import provider(two); fn run() { two! { () } fallback { () } }";
    let file = dir.write("app.kio", valid);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &path_to_file_uri(&provider_file), provider);
    open_and_wait_for_analysis(&mut lsp, &uri, valid);
    let incomplete = "module app; import provider(two); fn run() { tw";
    send_full_text_change(&mut lsp, &uri, 2, incomplete);
    assert!(
        !wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let pos = source_position(incomplete, incomplete.len());
    let completion = send_completion(&mut lsp, &uri, pos.0, pos.1);
    let item = completion["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"] == "two!")
        .unwrap_or_else(|| panic!("{completion}"));
    assert!(item["insertTextFormat"].is_null(), "{item}");
    assert!(item["insertText"].is_null(), "{item}");
    assert_eq!(item["textEdit"]["newText"], "two!");
    assert_eq!(
        apply_lsp_text_edits(incomplete, &[item["textEdit"].clone()]),
        "module app; import provider(two); fn run() { two!"
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_block_editor_regions_use_current_selected_headers() {
    let dir = TempDir::new("block-editor-regions");
    dir.write("app.pkg.kio", "package app; bridge { app; provider; }");
    let provider = include_str!("../fixtures/trailing-blocks/provider.kio")
        .replace("elab two :", "elab renamed :");
    let provider_file = dir.write("provider.kio", &provider);
    let provider_uri = path_to_file_uri(&provider_file);
    let valid =
        "module app; import provider(renamed); fn run() { renamed! { () } fallback { () } }";
    let file = dir.write("app.kio", valid);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize_with_capabilities(
        &path_to_file_uri(dir.path()),
        json!({
            "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } }
        }),
    );
    open_and_wait_for_analysis(&mut lsp, &provider_uri, &provider);
    open_and_wait_for_analysis(&mut lsp, &uri, valid);

    // Preserve a confirmed provider header before replacing the valid body
    // with an incomplete one; a quiet diagnostics stream is not a barrier.
    let pos = source_position(valid, valid.find("{ () }").unwrap() + 2);
    let ready = wait_for_signature_help(&mut lsp, &uri, pos.0, pos.1);
    let label = ready["signatures"][0]["label"].as_str().unwrap_or("");
    assert!(
        label.starts_with("renamed!") && label.contains("fallback: product block"),
        "selected provider header is not ready: {ready}"
    );

    let incomplete = "module app; import provider(renamed); fn run() { ren";
    send_full_text_change(&mut lsp, &uri, 2, incomplete);
    assert!(
        !wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let pos = source_position(incomplete, incomplete.len());
    let completion = send_completion(&mut lsp, &uri, pos.0, pos.1);
    let snippet = completion["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"] == "renamed!")
        .unwrap_or_else(|| panic!("missing selected elaborator snippet: {completion}"));
    assert_eq!(snippet["insertTextFormat"], 2, "{snippet}");
    assert_eq!(
        snippet["textEdit"]["newText"],
        "renamed! ${1} { ${2} } fallback { ${3} }$0"
    );
    let mut edit = snippet["textEdit"].clone();
    edit["newText"] = json!(
        snippet["textEdit"]["newText"]
            .as_str()
            .unwrap()
            .replace("${1}", "")
            .replace("${2}", "()")
            .replace("${3}", "()")
            .replace("$0", "")
    );
    let applied = format!("{} }}", apply_lsp_text_edits(incomplete, &[edit]));
    send_full_text_change(&mut lsp, &uri, 3, &applied);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 3)["diagnostics"],
        json!([])
    );

    for (version, source, expected) in [
        (
            4,
            "module app; import provider(renamed); fn run() { renamed! { () } fal",
            vec!["fallback"],
        ),
        (
            5,
            "module app; import provider(renamed); fn run() { renamed! { () } fallback { () } ",
            vec![],
        ),
    ] {
        send_full_text_change(&mut lsp, &uri, version, source);
        assert!(
            !wait_for_publish_version(&mut lsp, &uri, i64::from(version))["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let pos = source_position(source, source.len());
        let response = send_completion(&mut lsp, &uri, pos.0, pos.1);
        assert_eq!(
            completion_labels(&response),
            expected,
            "{source}: {response}"
        );
    }
    send_full_text_change(&mut lsp, &uri, 6, valid);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 6)["diagnostics"],
        json!([])
    );
    for (offset, active) in [
        (valid.find("{ () }").unwrap() + 2, 1),
        (valid.rfind("{ () }").unwrap() + 2, 2),
    ] {
        let pos = source_position(valid, offset);
        let id = lsp.send_request(
            "textDocument/signatureHelp",
            json!({"textDocument": {"uri": uri}, "position": {"line": pos.0, "character": pos.1}}),
        );
        let response = lsp.recv_matching(|value| value["id"].as_i64() == Some(id));
        assert_eq!(response["result"]["activeParameter"], active, "{response}");
        let label = response["result"]["signatures"][0]["label"]
            .as_str()
            .unwrap();
        assert!(
            label.contains("prefix values") && label.contains("fallback: product block"),
            "{label}"
        );
    }
    let changed = provider.replace("product fallback;", "product alternate;");
    send_full_text_change(&mut lsp, &provider_uri, 2, &changed);
    let _ = wait_for_publish_version(&mut lsp, &uri, 6);
    let incomplete = "module app; import provider(renamed); fn run() { renamed! { () } alt";
    send_full_text_change(&mut lsp, &uri, 7, incomplete);
    let pos = source_position(incomplete, incomplete.len());
    let response = send_completion(&mut lsp, &uri, pos.0, pos.1);
    let labels = completion_labels(&response);
    assert!(!labels.contains(&"fallback"), "{response}");
    assert!(
        labels.is_empty() || labels == vec!["alternate"],
        "{response}"
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_block_label_repairs_and_missing_block_scaffolds_reanalyse() {
    let dir = TempDir::new("block-editor-fixes");
    dir.write("app.pkg.kio", "package app; bridge { app; provider; }");
    let provider = format!(
        "{}\npub elab finish : (. & Sequence(Box, .)) -> . {{ trailing product; trailing sequence after; impl first_checked }}",
        include_str!("../fixtures/trailing-blocks/provider.kio")
    );
    let provider_file = dir.write("provider.kio", &provider);
    let provider_uri = path_to_file_uri(&provider_file);
    let source = "module app; import provider(two); fn run() { two! { () } wrong { () } }";
    let file = dir.write("app.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &provider_uri, &provider);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let published = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostic = &published["diagnostics"][0];
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["title"] == "Use trailing block label `fallback`")
        .unwrap_or_else(|| panic!("{response}"));
    assert_eq!(action["isPreferred"], true);
    let (edits, _) = action_edits_for_uri(action, &uri);
    let repaired = apply_lsp_text_edits(source, edits);
    assert!(repaired.contains("fallback { () }"));
    send_full_text_change(&mut lsp, &uri, 2, &repaired);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"],
        json!([])
    );

    let missing = "module app; import provider(finish, box_pure); fn run() { finish! { () } }";
    send_full_text_change(&mut lsp, &uri, 3, missing);
    let published = wait_for_publish_version(&mut lsp, &uri, 3);
    let diagnostic = &published["diagnostics"][0];
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["title"] == "Add missing trailing block scaffolds")
        .unwrap_or_else(|| panic!("{response}"));
    assert_eq!(action["isPreferred"], false);
    let (edits, _) = action_edits_for_uri(action, &uri);
    let scaffold = apply_lsp_text_edits(missing, edits);
    assert!(scaffold.contains("after { }"), "{scaffold}");
    send_full_text_change(&mut lsp, &uri, 4, &scaffold);
    let still_incomplete = wait_for_publish_version(&mut lsp, &uri, 4);
    assert_eq!(
        still_incomplete["diagnostics"][0]["code"], 14,
        "{still_incomplete}"
    );
    let filled = scaffold.replace("after { }", "after { box_pure(()) }");
    send_full_text_change(&mut lsp, &uri, 5, &filled);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 5)["diagnostics"],
        json!([])
    );

    send_full_text_change(&mut lsp, &uri, 6, missing);
    let published = wait_for_publish_version(&mut lsp, &uri, 6);
    let diagnostic = &published["diagnostics"][0];
    let changed = provider.replace("sequence after;", "sequence later;");
    send_full_text_change(&mut lsp, &provider_uri, 2, &changed);
    let _ = wait_for_publish_version(&mut lsp, &uri, 6);
    let stale = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(stale["result"].as_array().unwrap().is_empty(), "{stale}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_trailing_labels_retract_after_provider_only_edits() {
    let dir = TempDir::new("trailing-label-provider-edit");
    dir.write("app.pkg.kio", "package app; bridge { app; provider; }");
    let provider = include_str!("../fixtures/trailing-blocks/provider.kio");
    let provider_file = dir.write("provider.kio", provider);
    let provider_uri = path_to_file_uri(&provider_file);
    let source =
        "module app; import provider(two); fn run(value: .) { two! { value } fallback { () } }";
    let file = dir.write("app.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &provider_uri, provider);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let label_pos = source_position(source, source.find("fallback").unwrap());
    let value_pos = source_position(source, source.find("{ value }").unwrap() + 2);
    assert!(
        hover_type(&wait_for_hover(&mut lsp, &uri, label_pos.0, label_pos.1))
            .contains("trailing product fallback")
    );
    let methods = [
        "textDocument/hover",
        "textDocument/definition",
        "textDocument/references",
        "textDocument/documentHighlight",
        "textDocument/prepareRename",
        "textDocument/rename",
    ];
    let query = |lsp: &mut LspProcess, method: &str| {
        let id = lsp.send_request(
            method,
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": label_pos.0, "character": label_pos.1 },
                "context": { "includeDeclaration": true },
                "newName": "replacement",
            }),
        );
        lsp.recv_matching(|v| v["id"].as_i64() == Some(id))
    };
    let mut stale = Vec::new();
    for (version, changed) in [
        (
            2,
            provider.replace("product fallback;", "product alternate;"),
        ),
        (4, provider.replace(" trailing product fallback;", "")),
    ] {
        send_full_text_change(&mut lsp, &provider_uri, version, &changed);
        let diagnostic = wait_for_publish_version(&mut lsp, &uri, 1);
        assert!(
            !diagnostic["diagnostics"].as_array().unwrap().is_empty(),
            "the unchanged consumer must reject the changed descriptor: {diagnostic}"
        );
        for method in methods {
            let response = query(&mut lsp, method);
            if response.get("error").is_some() || !response["result"].is_null() {
                stale.push((version, method, response));
            }
        }
        assert_eq!(
            hover_type(&send_hover(&mut lsp, &uri, value_pos.0, value_pos.1)),
            ".",
            "ordinary unchanged-consumer navigation retains its existing policy"
        );
        send_full_text_change(&mut lsp, &provider_uri, version + 1, provider);
        let restored = wait_for_publish_version(&mut lsp, &uri, 1);
        assert_eq!(restored["diagnostics"], json!([]), "{restored}");
        for method in methods {
            let response = query(&mut lsp, method);
            assert!(response.get("error").is_none(), "{method}: {response}");
            assert!(!response["result"].is_null(), "{method}: {response}");
        }
    }
    assert_eq!(lsp.shutdown(), 0);
    assert!(
        stale.is_empty(),
        "stale continuation-label responses: {stale:#?}"
    );
}

#[test]
fn lsp_trailing_labels_follow_exact_declarations_and_rename_reanalyses() {
    let dir = TempDir::new("trailing-label-identity");
    dir.write("app.pkg.kio", "package app; bridge { app; provider; }");
    let provider = format!(
        "{}\n{}\n{}\n",
        include_str!("../fixtures/trailing-blocks/provider.kio"),
        "pub elab alternate : (. & .) -> . { trailing product; trailing product fallback; impl first_checked }",
        "pub elab three : (. & . & .) -> . { trailing product; trailing product fallback; trailing product final; impl first_checked }"
    );
    let provider_file = dir.write("provider.kio", &provider);
    let provider_uri = path_to_file_uri(&provider_file);
    let source = "module app;\nimport provider(two, alternate, three);\nfn run(fallback: .) {\n  let chosen = two! { fallback } fallback { () };\n  alternate! { chosen } fallback { () };\n  three! { () } fallback { () } final { () }\n}\n";
    let file = dir.write("app.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &provider_uri, &provider);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let label_offset = source.find("fallback { () }").unwrap();
    let label_pos = source_position(source, label_offset);
    let declaration_offset = provider.find("fallback; impl first_checked").unwrap();
    let label_hover = wait_for_hover(&mut lsp, &uri, label_pos.0, label_pos.1);
    assert!(
        hover_type(&label_hover).contains("trailing product fallback"),
        "{label_hover}"
    );
    assert!(hover_type(&label_hover).contains("`two!`"), "{label_hover}");
    assert_eq!(
        hover_range(&label_hover),
        source_range(source, label_offset, 8)
    );
    let definition = send_definition(&mut lsp, &uri, label_pos.0, label_pos.1);
    assert_eq!(definition["uri"], provider_uri);
    assert_eq!(
        json_range(&definition["range"]),
        source_range(&provider, declaration_offset, 8)
    );

    let head_pos = source_position(source, source.find("two!").unwrap());
    let head_definition = send_definition(&mut lsp, &uri, head_pos.0, head_pos.1);
    assert_eq!(head_definition["uri"], provider_uri);
    assert_eq!(
        json_range(&head_definition["range"]),
        source_range(&provider, provider.find("two :").unwrap(), 3)
    );
    let head_rename = send_rename(&mut lsp, &uri, head_pos.0, head_pos.1, "renamed_two");
    assert!(head_rename.get("error").is_none(), "{head_rename}");
    let mut head_edits = head_rename["result"]["documentChanges"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|change| {
            change["edits"].as_array().unwrap().iter().map(|edit| {
                (
                    change["textDocument"]["uri"].as_str().unwrap().to_owned(),
                    json_range(&edit["range"]),
                )
            })
        })
        .collect::<Vec<_>>();
    head_edits.sort();
    let mut expected_head_edits = vec![
        (
            provider_uri.clone(),
            source_range(&provider, provider.find("two :").unwrap(), 3),
        ),
        (
            uri.clone(),
            source_range(source, source.find("two,").unwrap(), 3),
        ),
        (
            uri.clone(),
            source_range(source, source.find("two!").unwrap(), 3),
        ),
    ];
    expected_head_edits.sort();
    assert_eq!(
        head_edits, expected_head_edits,
        "head edits retain the selective import"
    );

    let id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": label_pos.0, "character": label_pos.1 },
            "context": { "includeDeclaration": true },
        }),
    );
    let refs = lsp.recv_matching(|v| v["id"].as_i64() == Some(id));
    let mut actual = refs["result"]
        .as_array()
        .expect("label references")
        .iter()
        .map(|location| {
            (
                location["uri"].as_str().unwrap().to_owned(),
                json_range(&location["range"]),
            )
        })
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = vec![
        (uri.clone(), source_range(source, label_offset, 8)),
        (
            provider_uri.clone(),
            source_range(&provider, declaration_offset, 8),
        ),
    ];
    expected.sort();
    assert_eq!(
        actual, expected,
        "same-spelled value and other elaborators are distinct"
    );

    let third_pos = source_position(source, source.rfind("fallback { () }").unwrap());
    let collision = send_rename(&mut lsp, &uri, third_pos.0, third_pos.1, "final");
    assert!(
        collision["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("same elaborator"),
        "{collision}"
    );
    let renamed = send_rename(&mut lsp, &uri, label_pos.0, label_pos.1, "otherwise");
    assert!(renamed.get("error").is_none(), "{renamed}");
    let changes = renamed["result"]["documentChanges"]
        .as_array()
        .expect("versioned rename");
    assert_eq!(changes.len(), 2, "{renamed}");
    let mut updated_source = String::new();
    let mut updated_provider = String::new();
    for change in changes {
        let target = change["textDocument"]["uri"].as_str().unwrap();
        let edits = change["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 1, "{change}");
        if target == uri {
            updated_source = apply_lsp_text_edits(source, edits);
        } else {
            assert_eq!(target, provider_uri);
            updated_provider = apply_lsp_text_edits(&provider, edits);
        }
    }
    assert!(updated_source.contains("fn run(fallback: .)"));
    assert!(updated_source.contains("alternate! { chosen } fallback"));
    assert!(updated_source.contains("three! { () } fallback"));
    send_full_text_change(&mut lsp, &provider_uri, 2, &updated_provider);
    send_full_text_change(&mut lsp, &uri, 2, &updated_source);
    let renamed_pos = source_position(&updated_source, updated_source.find("otherwise").unwrap());
    let hover = wait_for_hover(&mut lsp, &uri, renamed_pos.0, renamed_pos.1);
    assert!(
        hover_type(&hover).contains("trailing product otherwise"),
        "{hover}"
    );
    let definition = send_definition(&mut lsp, &uri, renamed_pos.0, renamed_pos.1);
    assert_eq!(
        json_range(&definition["range"]),
        source_range(
            &updated_provider,
            updated_provider.find("otherwise").unwrap(),
            9
        )
    );

    let invalid = updated_source.replacen("otherwise", "missing", 1);
    send_full_text_change(&mut lsp, &uri, 3, &invalid);
    let diagnostics = wait_for_publish_version(&mut lsp, &uri, 3);
    let diagnostic = diagnostics["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| {
            d["message"]
                .as_str()
                .unwrap_or("")
                .contains("expected trailing block label `otherwise`")
        })
        .unwrap_or_else(|| panic!("{diagnostics}"));
    assert_eq!(
        json_range(&diagnostic["range"]),
        source_range(&invalid, invalid.find("missing").unwrap(), 7)
    );
    assert!(
        diagnostic["relatedInformation"]
            .as_array()
            .unwrap()
            .iter()
            .any(|related| related["location"]["uri"] == provider_uri)
    );
    let missing_pos = source_position(&invalid, invalid.find("missing").unwrap());
    assert!(
        send_hover(&mut lsp, &uri, missing_pos.0, missing_pos.1).is_null(),
        "failed current analysis must not reuse the old continuation label"
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_neutral_blocks_format_reanalyse_and_complete_lexical_binders() {
    let dir = TempDir::new("trailing-blocks");
    dir.write("app.pkg.kio", "package app; bridge { app; provider; }");
    dir.write(
        "provider.kio",
        include_str!("../fixtures/trailing-blocks/provider.kio"),
    );
    let compact_source = include_str!("../fixtures/trailing-blocks/app.kio")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let source = compact_source.as_str();
    let file = dir.write("app.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let (line, character) = source_position(source, source.find("; local").unwrap() + 3);
    assert_eq!(
        hover_type(&wait_for_hover(&mut lsp, &uri, line, character)),
        "."
    );

    let edits = send_formatting(&mut lsp, &uri);
    let edits = edits.as_array().expect("format edits");
    assert!(!edits.is_empty());
    let formatted = apply_lsp_text_edits(source, edits);
    assert!(formatted.contains("let local = packet!"));
    assert!(formatted.contains("fallback {"));
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{"text": formatted}],
        }),
    );
    let (line, character) = source_position(&formatted, formatted.rfind("local").unwrap());
    assert_eq!(
        hover_type(&wait_for_hover(&mut lsp, &uri, line, character)),
        "."
    );
    assert_eq!(send_formatting(&mut lsp, &uri), json!([]));
    let tokens = send_semantic_tokens_full(&mut lsp, &uri);
    let mut line = 0_u64;
    let mut column = 0_u64;
    let mut decoded = Vec::new();
    for token in tokens["data"]
        .as_array()
        .expect("semantic tokens")
        .chunks_exact(5)
    {
        let delta_line = token[0].as_u64().unwrap();
        let delta_start = token[1].as_u64().unwrap();
        if delta_line == 0 {
            column += delta_start;
        } else {
            line += delta_line;
            column = delta_start;
        }
        decoded.push((
            line,
            column,
            token[2].as_u64().unwrap(),
            token[3].as_u64().unwrap(),
            token[4].as_u64().unwrap(),
        ));
    }
    for marker in ["enter!", "packet!", "two!", "sequence!"] {
        let (line, column) = source_position(&formatted, formatted.find(marker).unwrap());
        assert!(
            decoded.contains(&(line, column, (marker.len() - 1) as u64, 5, 4)),
            "{marker}: {decoded:?}"
        );
    }
    let (line, column) = source_position(&formatted, formatted.find("fallback").unwrap());
    assert!(decoded.contains(&(line, column, 8, 7, 0)), "{decoded:?}");

    for (version, source, present, absent) in [
        (
            3,
            "module app; import provider(enter); fn run(parent: .) { enter! { let local = parent; loc",
            "local",
            "other",
        ),
        (
            4,
            "module app; import provider(enter); fn run(parent: .) { enter! { let other = parent; loc",
            "parent",
            "local",
        ),
    ] {
        lsp.send_notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": source}],
            }),
        );
        let (line, character) = source_position(source, source.len());
        let completion = send_completion(&mut lsp, &uri, line, character);
        let names = completion_labels(&completion);
        assert!(names.contains(&present), "{source}: {completion}");
        assert!(!names.contains(&absent), "{source}: {completion}");
        assert_eq!(send_formatting(&mut lsp, &uri), json!([]));
    }
    assert_eq!(lsp.shutdown(), 0);
}
