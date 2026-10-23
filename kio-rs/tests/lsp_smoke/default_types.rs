use super::*;

fn default_diagnostic(published: &Value, category: &str) -> Value {
    published["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|diagnostic| {
            let message = diagnostic["message"].as_str().unwrap().to_lowercase();
            message.contains("default") && message.contains(category)
        })
        .unwrap_or_else(|| panic!("missing {category} default diagnostic: {published}"))
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

#[test]
fn lsp_missing_dot_fixes_preserve_literal_spelling_and_refuse_stale_edits() {
    let dir = TempDir::new("literal-dot-fixes");
    dir.write("app.pkg.kio", "package app; bridge { app; }");
    let source_for = |literal: &str| {
        format!("module app; host type Float role(f64); pub fn run() -> Float {{ {literal} }}")
    };
    let initial = source_for("-4_2E-0(Float)");
    let file = dir.write("app.kio", &initial);
    let uri = path_to_file_uri(&file);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": initial}}),
    );
    for (ordinal, (literal, fixed, code)) in [
        ("-4_2E-0(Float)", "-4_2.0E-0(Float)", 11),
        ("42e-3", "42.0e-3", 11),
        ("42", "42.0", 14),
        ("42(Float)", "42.0(Float)", 14),
        ("4_2E+3", "4_2.0E+3", 11),
        ("4_2E+3(Float)", "4_2.0E+3(Float)", 11),
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
fn lsp_integer_exponent_fix_needs_no_expected_type() {
    let dir = TempDir::new("integer-exponent-fix");
    dir.write("app.pkg.kio", "package app; bridge { app; }");
    let source = "module app; default host type Float role(f64); pub fn run() -> . { let _value = 1e12; () }";
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
    let action = named_action(&response, "Use floating-point spelling");
    assert_eq!(action["isPreferred"], false);
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["newText"], ".0");
    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(applied, source.replace("1e12", "1.0e12"));
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
fn lsp_missing_default_choices_are_explicit_module_local_and_source_current() {
    let dir = TempDir::new("default-type-choices");
    dir.write("app.pkg.kio", "package app; bridge { app; }");
    let source = concat!(
        "module app;\n",
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
    let diagnostic = default_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), "integer");
    let response = send_code_action(&mut lsp, &uri, &diagnostic);
    for name in ["Int", "Narrow"] {
        let action = named_action(
            &response,
            &format!("Use {name} as the default integer type in this module"),
        );
        assert_eq!(action["isPreferred"], false, "{action}");
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["newText"], "default ");
    }
    let current = format!("// retained policy comment\n{source}");
    send_full_text_change(&mut lsp, &uri, 2, &current);
    let current_diagnostic =
        default_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 2), "integer");
    assert_eq!(
        send_code_action(&mut lsp, &uri, &diagnostic)["result"],
        json!([])
    );
    let response = send_code_action(&mut lsp, &uri, &current_diagnostic);
    let action = named_action(
        &response,
        "Use Int as the default integer type in this module",
    );
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(2));
    let applied = apply_lsp_text_edits(&current, edits);
    assert_eq!(
        applied,
        current.replace("pub host type Int", "default pub host type Int")
    );
    send_full_text_change(&mut lsp, &uri, 3, &applied);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 3)["diagnostics"],
        json!([])
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_shadowed_literal_type_omits_invalid_annotation_and_retains_default_selection() {
    let dir = TempDir::new("literal-only-type-fix");
    dir.write("app.pkg.kio", "package app; bridge { app; }");
    let source_for = |literal: &str| {
        format!(
            "module app; host type Int role(i64); pub fn run[Int](value: Int) -> . {{ let literal = {literal}; let _pair = (value, literal); () }}"
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
    for (ordinal, literal) in ["42", "42( // type policy\n _)", "((42))"]
        .into_iter()
        .enumerate()
    {
        let version = 1 + 2 * ordinal as i32;
        let source = source_for(literal);
        if ordinal != 0 {
            send_full_text_change(&mut lsp, &uri, version, &source);
        }
        let diagnostic = default_diagnostic(
            &wait_for_publish_version(&mut lsp, &uri, i64::from(version)),
            "integer",
        );
        let response = send_code_action(&mut lsp, &uri, &diagnostic);
        assert!(
            response["result"].as_array().unwrap().iter().all(|action| {
                !action["title"]
                    .as_str()
                    .unwrap()
                    .starts_with("Annotate this literal with ")
            }),
            "{response}"
        );
        let action = named_action(
            &response,
            "Use Int as the default integer type in this module",
        );
        assert_eq!(action["isPreferred"], false);
        let (edits, target_version) = action_edits_for_uri(action, &uri);
        assert_eq!(target_version, Some(i64::from(version)));
        assert_eq!(edits.len(), 1);
        let applied = apply_lsp_text_edits(&source, edits);
        assert_eq!(
            applied,
            source.replace("host type Int", "default host type Int")
        );
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
fn lsp_literal_annotation_fix_uses_an_existing_qualified_import_past_a_type_binder() {
    let dir = TempDir::new("qualified-literal-only-type-fix");
    dir.write(
        "app.pkg.kio",
        "package app; bridge { app; provider; other; }",
    );
    let provider = "module provider; host type Int role(i64);";
    let provider_uri = path_to_file_uri(&dir.write("provider.kio", provider));
    dir.write("other.kio", "module other; host type Int role(i32);");
    let source_for = |literal: &str| {
        format!(
            "module app; import provider(Int); import other as provider; import provider as numbers; pub fn identity(value: Int) -> Int {{ value }} pub fn other_identity(value: provider.Int) -> provider.Int {{ value }} pub fn run[Int](value: Int) -> . {{ let literal = {literal}; let _pair = (value, literal); () }}"
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
        ("42", "42(numbers.Int)"),
        (
            "42( // type policy\n _)",
            "42( // type policy\n numbers.Int)",
        ),
        ("((42))", "((42(numbers.Int)))"),
    ]
    .into_iter()
    .enumerate()
    {
        let version = 1 + 2 * ordinal as i32;
        let source = source_for(literal);
        if ordinal != 0 {
            send_full_text_change(&mut lsp, &uri, version, &source);
        }
        let diagnostic = default_diagnostic(
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
fn lsp_default_selection_fixes_cover_each_noninteger_literal_category() {
    for (name, role, literal, category) in [
        ("Float", "f64", "42.0", "float"),
        ("Bool", "bool", ".t", "boolean"),
        ("Text", "str", "\"hello\"", "string"),
    ] {
        let dir = TempDir::new("default-literal-category");
        dir.write("app.pkg.kio", "package app; bridge { app; }");
        let source = format!(
            "module app; host type {name} role({role}); pub fn run() -> . {{ let _value = {literal}; () }}"
        );
        let uri = path_to_file_uri(&dir.write("app.kio", &source));
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
        );
        let diagnostic = default_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), category);
        let response = send_code_action(&mut lsp, &uri, &diagnostic);
        let action = named_action(
            &response,
            &format!("Use {name} as the default {category} type in this module"),
        );
        assert_eq!(action["isPreferred"], false);
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["newText"], "default ");
        let applied = apply_lsp_text_edits(&source, edits);
        assert_eq!(applied, source.replace("host type", "default host type"));
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
fn lsp_default_import_fix_preserves_aliases_and_rechecks_changed_providers() {
    let dir = TempDir::new("default-type-import");
    dir.write("app.pkg.kio", "package app; bridge { app; provider; }");
    let provider =
        "module provider; default host type Int role(i64); host type Alternate role(i32);";
    let provider_file = dir.write("provider.kio", provider);
    let provider_uri = path_to_file_uri(&provider_file);
    let source = concat!(
        "module app; import provider( // import policy\n",
        " Int as Local, Alternate); pub fn identity(value: Alternate) -> Alternate { value } pub fn run() -> . { let _value = 42; () }",
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
    let diagnostic = default_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), "integer");
    let title = "Use Local as the default integer type in this module";
    let response = send_code_action(&mut lsp, &uri, &diagnostic);
    let action = named_action(&response, title);
    assert_eq!(action["isPreferred"], false);
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(
        applied,
        source.replace("Int as Local", "default Int as Local")
    );

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
    dir.write("app.pkg.kio", "package app; bridge { app; }");
    let provider = "module provider; pub literal whole = 4_2;";
    let provider_file = dir.write("provider.kio", provider);
    let provider_uri = path_to_file_uri(&provider_file);
    let source = concat!(
        "module app; import provider(whole);\n",
        "host type Int role(i64); host type Float role(f64);\n",
        "pub fn integer() -> Int { whole }\n",
        "pub fn floating() -> Float { // use policy\n whole(Float) }\n",
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
    assert_eq!(applied, source.replace("whole(Float)", "4_2.0(Float)"));
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
fn lsp_float_default_does_not_offer_a_missing_dot_fix_for_an_intended_integer() {
    let dir = TempDir::new("default-category-control");
    dir.write("app.pkg.kio", "package app; bridge { app; }");
    let source =
        "module app; default host type Float role(f64); pub fn run() -> . { let _value = 42; () }";
    let uri = path_to_file_uri(&dir.write("app.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let diagnostic = default_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), "integer");
    assert_eq!(
        send_code_action(&mut lsp, &uri, &diagnostic)["result"],
        json!([])
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_boolean_default_fix_repairs_the_intrinsic_without_a_boolean_literal() {
    let dir = TempDir::new("boolean-default-fix");
    dir.write("app.pkg.kio", "package app; bridge { app; }");
    let source = concat!(
        "module app; import __intrinsics__; host type Bool role(bool);\n",
        "pub fn run(flag: Bool) -> . { __if_then_else__(., flag, .() { () }, .() { () }) }",
    );
    let uri = path_to_file_uri(&dir.write("app.kio", source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let diagnostic = default_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), "boolean");
    let response = send_code_action(&mut lsp, &uri, &diagnostic);
    let actions = response["result"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "{response}");
    let action = &actions[0];
    assert!(
        action["title"]
            .as_str()
            .unwrap()
            .starts_with("Use Bool as the default ")
    );
    assert_eq!(action["isPreferred"], false);
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(
        applied,
        source.replace("host type Bool", "default host type Bool")
    );
    send_full_text_change(&mut lsp, &uri, 2, &applied);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"],
        json!([])
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_missing_default_fix_selects_an_applied_local_alias() {
    let dir = TempDir::new("applied-alias-default-fix");
    dir.write("app.pkg.kio", "package app; bridge { app; provider; }");
    let provider = "module provider; host type Int role(i64);";
    let provider_file = dir.write("provider.kio", provider);
    let source = concat!(
        "module app; import provider as numbers;\n",
        "pub type Identity[A] = A;\n",
        "pub type Selected = Identity(numbers.Int);\n",
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
    let diagnostic = default_diagnostic(&wait_for_publish_version(&mut lsp, &uri, 1), "integer");
    let response = send_code_action(&mut lsp, &uri, &diagnostic);
    let action = named_action(
        &response,
        "Use Selected as the default integer type in this module",
    );
    assert_eq!(action["isPreferred"], false);
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(
        applied,
        source.replace("pub type Selected", "default pub type Selected")
    );
    send_full_text_change(&mut lsp, &uri, 2, &applied);
    assert_eq!(
        wait_for_publish_version(&mut lsp, &uri, 2)["diagnostics"],
        json!([])
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_default_import_completion_applies_without_erasing_the_modifier() {
    let dir = TempDir::new("default-import-completion");
    dir.write("app.pkg.kio", "package app; bridge { app; provider; }");
    let provider = concat!(
        "module provider; host type Int role(i64); host type Float role(f64);\n",
        "pub type Alias = Int; pub fn default() -> . { () }\n",
    );
    let provider_file = dir.write("provider.kio", provider);
    let valid = "module app; pub fn run() { () }";
    let uri = path_to_file_uri(&dir.write("app.kio", valid));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    open_and_wait_for_analysis(&mut lsp, &path_to_file_uri(&provider_file), provider);
    open_and_wait_for_analysis(&mut lsp, &uri, valid);
    for (ordinal, tail) in ["default In", "default // import policy\n In"]
        .into_iter()
        .enumerate()
    {
        let version = 2 + 2 * ordinal as i32;
        let source = format!(
            "module app; import provider({tail}); pub fn run() -> . {{ let _value = 42; () }}"
        );
        send_full_text_change(&mut lsp, &uri, version, &source);
        let published = wait_for_publish_version(&mut lsp, &uri, i64::from(version));
        assert!(!published["diagnostics"].as_array().unwrap().is_empty());
        let offset = source.find("In);").unwrap() + 2;
        let pos = source_position(&source, offset);
        let completion = send_completion(&mut lsp, &uri, pos.0, pos.1);
        let labels = completion_labels(&completion);
        assert!(labels.contains(&"Int"), "{completion}");
        assert!(!labels.contains(&"default"), "{completion}");
        let item = completion["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == "Int")
            .unwrap();
        let applied = apply_lsp_text_edits(&source, &[item["textEdit"].clone()]);
        assert_eq!(applied, source.replace("In);", "Int);"));
        send_full_text_change(&mut lsp, &uri, version + 1, &applied);
        assert_eq!(
            wait_for_publish_version(&mut lsp, &uri, i64::from(version + 1))["diagnostics"],
            json!([])
        );
    }
    assert_eq!(lsp.shutdown(), 0);
}
