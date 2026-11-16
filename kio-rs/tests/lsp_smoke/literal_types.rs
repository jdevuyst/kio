use super::*;

fn literal_diagnostic(published: &Value, category: &str) -> Value {
    published["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|diagnostic| {
            let message = diagnostic["message"].as_str().unwrap().to_lowercase();
            message.contains("cannot determine") && message.contains(category)
        })
        .unwrap_or_else(|| panic!("missing {category} literal diagnostic: {published}"))
        .clone()
}

fn named_action<'a>(response: &'a Value, title: &str) -> &'a Value {
    response["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["title"] == title)
        .unwrap_or_else(|| panic!("missing {title}: {response}"))
}

fn assert_clean_analysis(lsp: &mut LspProcess, uri: &str, version: i32) {
    wait_for_analysis_event(&lsp.analysis_events, uri, version);
    let id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": uri}}),
    );
    loop {
        let message = lsp
            .recv()
            .expect("LSP server closed before the request barrier");
        if message["method"] == "textDocument/publishDiagnostics" && message["params"]["uri"] == uri
        {
            assert_eq!(message["params"]["diagnostics"], json!([]), "{message}");
        }
        if message["id"] == id {
            assert!(message.get("error").is_none(), "{message}");
            break;
        }
    }
}

#[test]
fn lsp_missing_dot_fixes_preserve_literal_spelling_and_refuse_stale_edits() {
    let dir = TempDir::new("literal-dot-fixes");
    dir.write("app.pkg.kio", "pkg app; bridge { app; }");
    let source_for = |literal: &str| {
        format!("mod app; host type Float role(f64); pub fn run() -> Float {{ {literal} }}")
    };
    let initial = source_for("-4_2E-0[Float]");
    let file = dir.write("app.kio", &initial);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": initial}}),
    );
    for (ordinal, (literal, fixed, code)) in [
        ("-4_2E-0[Float]", "-4_2.0E-0[Float]", 11),
        ("42e-3", "42.0e-3", 11),
        ("42[Float]", "42.0[Float]", 14),
        ("42", "42.0", 14),
        ("4_2E+3", "4_2.0E+3", 11),
        ("4_2E+3[Float]", "4_2.0E+3[Float]", 11),
    ]
    .into_iter()
    .enumerate()
    {
        let version = 1 + 2 * ordinal as i32;
        let source = source_for(literal);
        if ordinal != 0 {
            send_full_text_change(&mut lsp, &uri, version, &source);
        }
        let published = wait_for_publish_version(&mut lsp, &uri, i64::from(version));
        let diagnostic = published["diagnostics"]
            .as_array()
            .unwrap()
            .first()
            .unwrap_or_else(|| panic!("{literal}: {published}"));
        assert_eq!(diagnostic["code"], code);
        let response = send_code_action(&mut lsp, &uri, diagnostic);
        let action = named_action(&response, "Use floating-point spelling");
        assert_eq!(action["isPreferred"], false, "{action}");
        let (edits, target_version) = action_edits_for_uri(action, &uri);
        assert_eq!(target_version, Some(i64::from(version)));
        assert_eq!(edits.len(), 1, "{action}");
        assert_eq!(edits[0]["newText"], ".0", "{action}");
        let applied = apply_lsp_text_edits(&source, edits);
        assert_eq!(applied, source_for(fixed));
        send_full_text_change(&mut lsp, &uri, version + 1, &applied);
        assert_eq!(
            wait_for_publish_version(&mut lsp, &uri, i64::from(version + 1))["diagnostics"],
            json!([]),
            "{applied}"
        );
        assert_eq!(
            send_code_action(&mut lsp, &uri, diagnostic)["result"],
            json!([])
        );
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_expected_types_select_literal_carriers_without_an_ambient_choice() {
    for (role, literal) in [
        ("i64", "42"),
        ("f64", "42.0"),
        ("bool", ".t"),
        ("str", "\"hello\""),
    ] {
        let dir = TempDir::new("expected-literal-selection");
        dir.write("app.pkg.kio", "pkg app; bridge { app; }");
        let source =
            format!("mod app; host type Known role({role}); pub fn run() -> Known {{ {literal} }}");
        let uri = path_to_file_uri(&dir.write("app.kio", &source));
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
        );
        assert_clean_analysis(&mut lsp, &uri, 1);
        let extended = source.replace(
            "pub fn run",
            &format!("host type Other role({role}); pub fn run"),
        );
        send_full_text_change(&mut lsp, &uri, 2, &extended);
        assert_clean_analysis(&mut lsp, &uri, 2);
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_explicit_literal_annotations_are_checked_against_expected_types() {
    for (role, literal) in [
        ("i64", "42"),
        ("f64", "42.0"),
        ("bool", ".t"),
        ("str", "\"hello\""),
    ] {
        let dir = TempDir::new("annotated-literal-mismatch");
        dir.write("app.pkg.kio", "pkg app; bridge { app; }");
        let source = format!(
            "mod app; host type Known role({role}); host type Other role({role}); pub fn run() -> Known {{ {literal}[Other] }}"
        );
        let uri = path_to_file_uri(&dir.write("app.kio", &source));
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
        );
        let published = wait_for_publish_version(&mut lsp, &uri, 1);
        let diagnostic = &published["diagnostics"][0];
        assert_eq!(diagnostic["code"], 14, "{published}");
        let message = diagnostic["message"].as_str().unwrap();
        assert!(message.contains("type mismatch"), "{published}");
        assert!(
            message.contains("Known") && message.contains("Other"),
            "{published}"
        );
        assert_eq!(
            send_code_action(&mut lsp, &uri, diagnostic)["result"],
            json!([])
        );
        let annotated = source.replace(&format!("{literal}[Other]"), &format!("{literal}[Known]"));
        send_full_text_change(&mut lsp, &uri, 2, &annotated);
        assert_eq!(
            wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"],
            json!([])
        );
        assert_eq!(
            send_code_action(&mut lsp, &uri, diagnostic)["result"],
            json!([])
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_unanchored_exponent_keeps_its_literal_annotation_requirement() {
    let dir = TempDir::new("integer-exponent-fix");
    dir.write("app.pkg.kio", "pkg app; bridge { app; }");
    let source = "mod app; host type Float role(f64); pub fn run() -> . { let _value = 1e12; () }";
    let uri = path_to_file_uri(&dir.write("app.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let published = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostic = &published["diagnostics"][0];
    assert_eq!(diagnostic["code"], 11, "{published}");
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("exponent notation"),
        "{published}"
    );
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    assert_eq!(response["result"], json!([]));
    let fix = diagnostic["data"]["fixes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fix| fix["title"] == "Use floating-point spelling")
        .expect("the lexical diagnostic retains its point insertion");
    let edits = fix["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["replacement"], ".0");
    let manual_edit = json!({"range": edits[0]["range"], "newText": edits[0]["replacement"]});
    let applied = apply_lsp_text_edits(source, &[manual_edit]);
    assert_eq!(applied, source.replace("1e12", "1.0e12"));
    send_full_text_change(&mut lsp, &uri, 2, &applied);
    let underdetermined = literal_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 2), "float");
    let annotation = send_code_action(&mut lsp, &uri, &underdetermined);
    let action = named_action(&annotation, "Annotate this literal with Float");
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(2));
    let annotated = apply_lsp_text_edits(&applied, edits);
    assert_eq!(annotated, applied.replace("1.0e12", "1.0e12[Float]"));
    send_full_text_change(&mut lsp, &uri, 3, &annotated);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 3)["diagnostics"],
        json!([])
    );
    assert_eq!(
        send_code_action(&mut lsp, &uri, diagnostic)["result"],
        json!([])
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_literal_annotation_choices_are_explicit_and_source_current() {
    let dir = TempDir::new("literal-type-choices");
    dir.write("app.pkg.kio", "pkg app; bridge { app; }");
    let source = concat!(
        "mod app;\n",
        "pub host type Int role(i64);\n",
        "host type Narrow role(i32);\n",
        "pub fn run() -> . { let _value = 42; () }\n",
    );
    let file = dir.write("app.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let diagnostic = literal_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), "integer");
    let response = send_code_action(&mut lsp, &uri, &diagnostic);
    for name in ["Int", "Narrow"] {
        let action = named_action(&response, &format!("Annotate this literal with {name}"));
        assert_eq!(action["isPreferred"], false, "{action}");
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["newText"], format!("[{name}]"));
    }
    let current = format!("// retained policy comment\n{source}");
    send_full_text_change(&mut lsp, &uri, 2, &current);
    let current_diagnostic =
        literal_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 2), "integer");
    assert_eq!(
        send_code_action(&mut lsp, &uri, &diagnostic)["result"],
        json!([])
    );
    let response = send_code_action(&mut lsp, &uri, &current_diagnostic);
    let action = named_action(&response, "Annotate this literal with Int");
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(2));
    let applied = apply_lsp_text_edits(&current, edits);
    assert_eq!(applied, current.replace("42", "42[Int]"));
    send_full_text_change(&mut lsp, &uri, 3, &applied);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 3)["diagnostics"],
        json!([])
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_shadowed_literal_type_omits_invalid_annotation_fixes() {
    let dir = TempDir::new("literal-only-type-fix");
    dir.write("app.pkg.kio", "pkg app; bridge { app; }");
    let source_for = |literal: &str| {
        format!(
            "mod app; host type Int role(i64); pub fn run[Int](value: Int) -> . {{ let literal = {literal}; let _pair = (value, literal); () }}"
        )
    };
    let initial = source_for("42");
    let uri = path_to_file_uri(&dir.write("app.kio", &initial));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": initial}}),
    );
    for (ordinal, literal) in ["42", "42[ // type policy\n _]", "((42))"]
        .into_iter()
        .enumerate()
    {
        let version = 1 + ordinal as i32;
        if ordinal != 0 {
            send_full_text_change(&mut lsp, &uri, version, &source_for(literal));
        }
        let diagnostic = literal_diagnostic(
            &wait_for_publish_version(&mut lsp, &uri, i64::from(version)),
            "integer",
        );
        assert_eq!(
            send_code_action(&mut lsp, &uri, &diagnostic)["result"],
            json!([])
        );
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_literal_annotation_fix_uses_an_existing_qualified_import_past_a_type_binder() {
    let dir = TempDir::new("qualified-literal-only-type-fix");
    dir.write("app.pkg.kio", "pkg app; bridge { app; provider; other; }");
    let provider = "mod provider; host type Int role(i64);";
    let provider_uri = path_to_file_uri(&dir.write("provider.kio", provider));
    dir.write("other.kio", "mod other; host type Int role(i32);");
    let source_for = |literal: &str| {
        format!(
            "mod app; import provider {{type Int}} import other as provider; import provider as numbers; pub fn identity(value: Int) -> Int {{ value }} pub fn other_identity(value: provider.Int) -> provider.Int {{ value }} pub fn run[Int](value: Int) -> . {{ let literal = {literal}; let _pair = (value, literal); () }}"
        )
    };
    let initial = source_for("42");
    let uri = path_to_file_uri(&dir.write("app.kio", &initial));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &provider_uri, provider);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": initial}}),
    );
    for (ordinal, (literal, fixed)) in [
        ("42", "42[numbers.Int]"),
        (
            "42[ // type policy\n _]",
            "42[ // type policy\n numbers.Int]",
        ),
        ("((42))", "((42[numbers.Int]))"),
    ]
    .into_iter()
    .enumerate()
    {
        let version = 1 + 2 * ordinal as i32;
        let source = source_for(literal);
        if ordinal != 0 {
            send_full_text_change(&mut lsp, &uri, version, &source);
        }
        let diagnostic = literal_diagnostic(
            &wait_for_publish_version(&mut lsp, &uri, i64::from(version)),
            "integer",
        );
        let response = send_code_action(&mut lsp, &uri, &diagnostic);
        let action = named_action(&response, "Annotate this literal with Int");
        assert_eq!(action["isPreferred"], false);
        let (edits, target_version) = action_edits_for_uri(action, &uri);
        assert_eq!(target_version, Some(i64::from(version)));
        assert_eq!(edits.len(), 1);
        let applied = apply_lsp_text_edits(&source, edits);
        assert_eq!(applied, source_for(fixed));
        assert!(!applied.contains("default "), "{applied}");
        send_full_text_change(&mut lsp, &uri, version + 1, &applied);
        assert_eq!(
            wait_for_publish_version(&mut lsp, &uri, i64::from(version + 1))["diagnostics"],
            json!([])
        );
        assert_eq!(
            send_code_action(&mut lsp, &uri, &diagnostic)["result"],
            json!([])
        );
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_annotation_fixes_cover_each_noninteger_literal_category() {
    for (name, role, literal, category) in [
        ("Float", "f64", "42.0", "float"),
        ("Bool", "bool", ".t", "boolean"),
        ("Text", "str", "\"hello\"", "string"),
    ] {
        let dir = TempDir::new("literal-category-annotation");
        dir.write("app.pkg.kio", "pkg app; bridge { app; }");
        let source = format!(
            "mod app; host type {name} role({role}); pub fn run() -> . {{ let _value = {literal}; () }}"
        );
        let uri = path_to_file_uri(&dir.write("app.kio", &source));
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
        );
        let diagnostic = literal_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), category);
        let response = send_code_action(&mut lsp, &uri, &diagnostic);
        let action = named_action(&response, &format!("Annotate this literal with {name}"));
        assert_eq!(action["isPreferred"], false);
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["newText"], format!("[{name}]"));
        let applied = apply_lsp_text_edits(&source, edits);
        assert_eq!(
            applied,
            source.replace(literal, &format!("{literal}[{name}]"))
        );
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
fn lsp_annotation_fix_preserves_import_aliases_and_rechecks_changed_providers() {
    let dir = TempDir::new("literal-type-import");
    dir.write("app.pkg.kio", "pkg app; bridge { app; provider; }");
    let provider = "mod provider; host type Int role(i64); host type Alternate role(i32);";
    let provider_file = dir.write("provider.kio", provider);
    let provider_uri = path_to_file_uri(&provider_file);
    let source = concat!(
        "mod app; import provider { // import policy\n",
        " type Int as Local; type Alternate} pub fn identity(value: Alternate) -> Alternate { value } pub fn run() -> . { let _value = 42; () }",
    );
    let file = dir.write("app.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &provider_uri, provider);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let diagnostic = literal_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), "integer");
    let title = "Annotate this literal with Local";
    let response = send_code_action(&mut lsp, &uri, &diagnostic);
    let action = named_action(&response, title);
    assert_eq!(action["isPreferred"], false);
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(applied, source.replace("42", "42[Local]"));

    let changed = provider.replace("role(i64)", "role(f64)");
    send_full_text_change(&mut lsp, &provider_uri, 2, &changed);
    wait_for_analysis_event(&lsp.analysis_events, &provider_uri, 2);
    let response = send_code_action(&mut lsp, &uri, &diagnostic);
    assert!(
        response["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|action| action["title"] != title)
    );
    send_full_text_change(&mut lsp, &provider_uri, 3, provider);
    wait_for_analysis_event(&lsp.analysis_events, &provider_uri, 3);
    let response = send_code_action(&mut lsp, &uri, &diagnostic);
    named_action(&response, title);
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

#[test]
fn lsp_alias_missing_dot_fix_expands_only_the_selected_use() {
    let dir = TempDir::new("literal-alias-dot-fix");
    dir.write("app.pkg.kio", "pkg app; bridge { app; }");
    let provider = "mod provider; pub lit whole = 4_2;";
    let provider_file = dir.write("provider.kio", provider);
    let provider_uri = path_to_file_uri(&provider_file);
    let source = concat!(
        "mod app; import provider {lit whole}\n",
        "host type Int role(i64); host type Float role(f64);\n",
        "pub fn integer() -> Int { whole }\n",
        "pub fn floating() -> Float { // use policy\n whole[Float] }\n",
    );
    let file = dir.write("app.kio", source);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &provider_uri, provider);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let published = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostic = &published["diagnostics"][0];
    assert_eq!(diagnostic["code"], 14, "{published}");
    assert!(
        diagnostic["relatedInformation"]
            .as_array()
            .unwrap()
            .iter()
            .any(|related| {
                related["location"]["uri"] == provider_uri
                    && related["message"]
                        .as_str()
                        .unwrap()
                        .contains("integer spelling")
            }),
        "{published}"
    );
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = named_action(&response, "Use floating-point spelling at this alias use");
    assert_eq!(action["isPreferred"], false);
    assert_eq!(
        action["edit"]["documentChanges"].as_array().unwrap().len(),
        1
    );
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(edits.len(), 1);
    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(applied, source.replace("whole[Float]", "4_2.0[Float]"));
    assert_eq!(fs::read_to_string(provider_file).unwrap(), provider);
    send_full_text_change(&mut lsp, &uri, 2, &applied);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"],
        json!([])
    );
    assert_eq!(
        send_code_action(&mut lsp, &uri, diagnostic)["result"],
        json!([])
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_unconstrained_integer_does_not_select_an_available_float() {
    let dir = TempDir::new("literal-category-control");
    dir.write("app.pkg.kio", "pkg app; bridge { app; }");
    let source = "mod app; host type Float role(f64); pub fn run() -> . { let _value = 42; () }";
    let uri = path_to_file_uri(&dir.write("app.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let diagnostic = literal_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), "integer");
    assert_eq!(
        send_code_action(&mut lsp, &uri, &diagnostic)["result"],
        json!([])
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_boolean_intrinsic_uses_its_checked_concrete_type_argument() {
    let dir = TempDir::new("boolean-intrinsic-specialization");
    dir.write("app.pkg.kio", "pkg app; bridge { app; }");
    let source = concat!(
        "mod app; import __intrinsics__; host type Bool role(bool); host type Other role(bool);\n",
        "pub fn run(flag: Bool) -> . { __if_then_else__[Bool][.](flag, .() { () }, .() { () }) }",
    );
    let uri = path_to_file_uri(&dir.write("app.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    assert_clean_analysis(&mut lsp, &uri, 1);
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_annotation_fix_uses_an_applied_local_alias() {
    let dir = TempDir::new("applied-alias-literal-fix");
    dir.write("app.pkg.kio", "pkg app; bridge { app; provider; }");
    let provider = "mod provider; host type Int role(i64);";
    let provider_file = dir.write("provider.kio", provider);
    let source = concat!(
        "mod app; import provider as numbers;\n",
        "pub type Identity[A] = A;\n",
        "pub type Selected = Identity[numbers.Int];\n",
        "pub fn run() -> . { let _value = 42; () }\n",
    );
    let uri = path_to_file_uri(&dir.write("app.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &path_to_file_uri(&provider_file), provider);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let diagnostic = literal_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), "integer");
    let response = send_code_action(&mut lsp, &uri, &diagnostic);
    let action = named_action(&response, "Annotate this literal with Selected");
    assert_eq!(action["isPreferred"], false);
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(applied, source.replace("42", "42[Selected]"));
    send_full_text_change(&mut lsp, &uri, 2, &applied);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"],
        json!([])
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_ordinary_import_completion_applies_without_a_default_modifier() {
    let dir = TempDir::new("literal-type-import-completion");
    dir.write("app.pkg.kio", "pkg app; bridge { app; provider; }");
    let provider = concat!(
        "mod provider; host type Int role(i64); host type Float role(f64);\n",
        "pub type Alias = Int; pub fn default() -> . { () }\n",
    );
    let provider_file = dir.write("provider.kio", provider);
    let valid = "mod app; pub fn run() { () }";
    let uri = path_to_file_uri(&dir.write("app.kio", valid));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &path_to_file_uri(&provider_file), provider);
    open_and_wait_for_analysis(&mut lsp, &uri, valid);
    for (ordinal, tail) in ["In", "// import policy\n In"].into_iter().enumerate() {
        let version = 2 + 2 * ordinal as i32;
        let source =
            format!("mod app; import provider {{type {tail}}} pub fn run() -> Int {{ 42 }}");
        send_full_text_change(&mut lsp, &uri, version, &source);
        let published = wait_for_publish_version(&mut lsp, &uri, i64::from(version));
        assert!(!published["diagnostics"].as_array().unwrap().is_empty());
        let offset = source.find("In}").unwrap() + 2;
        let pos = source_position(&source, offset);
        let completion = send_completion(&mut lsp, &uri, pos.0, pos.1);
        let labels = completion_labels(&completion);
        assert!(labels.contains(&"Int"), "{completion}");
        let item = completion["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == "Int")
            .unwrap();
        let applied = apply_lsp_text_edits(&source, &[item["textEdit"].clone()]);
        assert_eq!(applied, source.replace("In}", "Int}"));
        send_full_text_change(&mut lsp, &uri, version + 1, &applied);
        assert_eq!(
            wait_for_publish_version(&mut lsp, &uri, i64::from(version + 1))["diagnostics"],
            json!([])
        );
    }
    assert_eq!(lsp.shutdown(), 0);
}
