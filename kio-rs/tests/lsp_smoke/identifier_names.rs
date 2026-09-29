use super::*;

fn assert_local_repair(
    role: &str,
    source: &str,
    expected: &str,
    replacement: &str,
    relative: &str,
    support: &[(&str, &str)],
) {
    let dir = TempDir::new("identifier-word-repair");
    if support.is_empty() {
        dir.write_pkg_root_package();
    } else {
        for (path, content) in support {
            dir.write(path, content);
        }
    }
    let path = dir.write(relative, source);
    let uri = path_to_file_uri(&path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let publish = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostic = publish["diagnostics"].as_array().unwrap().first().unwrap();
    assert_eq!(diagnostic["code"], 11, "{publish:?}");
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{role} name `")),
        "{publish:?}"
    );
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let title = format!("Replace this token with `{replacement}`");
    let action = response["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["title"] == title)
        .unwrap_or_else(|| panic!("missing {role} repair {title}: {response:?}"));
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["isPreferred"], false);
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(
        edits.len(),
        1,
        "unresolved spelling repairs are token-local"
    );
    let repaired = apply_lsp_text_edits(source, edits);
    assert_eq!(repaired, expected);
    send_full_text_change(&mut lsp, &uri, 2, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        cleared["diagnostics"].as_array().is_some_and(Vec::is_empty),
        "{cleared:?}\n{repaired}"
    );
    let stale = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        stale["result"].as_array().is_some_and(Vec::is_empty),
        "{stale:?}"
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn every_identifier_word_violation_repairs_the_current_version() {
    for (invalid, valid) in [
        ("Foo", "foo"),
        ("fooBar", "foobar"),
        ("a1b", "a1_b"),
        ("foo123bar", "foo123_bar"),
        ("foo_123", "foo123"),
        ("foo__bar", "foo_bar"),
        ("foo_Bar", "foo_bar"),
        ("__foo", "_foo"),
        ("__foo", "foo"),
    ] {
        let source = format!("module pkg/main;\npub fn {invalid}() -> . {{ () }}\n");
        let expected = format!("module pkg/main;\npub fn {valid}() -> . {{ () }}\n");
        assert_local_repair("value", &source, &expected, valid, "pkg/main.kio", &[]);
    }
    assert_local_repair(
        "type",
        "module pkg/main;\npub type Foo_Bar = .;\n",
        "module pkg/main;\npub type Foo_bar = .;\n",
        "Foo_bar",
        "pkg/main.kio",
        &[],
    );
    assert_local_repair(
        "label",
        "module pkg/main;\npub labels { _foo: . };\n",
        "module pkg/main;\npub labels { foo: . };\n",
        "foo",
        "pkg/main.kio",
        &[],
    );
}

#[test]
fn name_repairs_cover_module_and_package_headers() {
    assert_local_repair(
        "module",
        "module pkg/foo_123;\n",
        "module pkg/foo123;\n",
        "foo123",
        "pkg/foo123.kio",
        &[],
    );
    assert_local_repair(
        "package",
        "package foo_123;\n",
        "package foo123;\n",
        "foo123",
        "foo123.pkg.kio",
        &[("foo123.kio", "module foo123;\n")],
    );
}

#[test]
fn dependency_local_name_repairs_keep_the_exact_filename_identity() {
    assert_local_repair(
        "dependency",
        "dependency foo_123;\nsource { path \"provider/provider.pkg.kio\"; }\n",
        "dependency foo123;\nsource { path \"provider/provider.pkg.kio\"; }\n",
        "foo123",
        "foo123.dep.kio",
        &[
            ("pkg.pkg.kio", "package pkg;\n"),
            ("pkg.kio", "module pkg;\n"),
            ("provider/provider.pkg.kio", "package provider;\n"),
            ("provider/provider.kio", "module provider;\n"),
            ("foo123/main.kio", "module foo123/main;\n"),
        ],
    );
}

#[test]
fn a_token_local_name_repair_does_not_rename_other_namespaces_or_text() {
    let source = concat!(
        "module pkg/main;\n",
        "host type Str role(str);\n",
        "pub type Foo = .;\n",
        "// Foo remains a type and literal text.\n",
        "pub fn Foo(value: Foo) -> Foo { value }\n",
        "pub fn text() -> Str { \"Foo\" }\n",
    );
    let expected = source.replacen("fn Foo(", "fn foo(", 1);
    assert_local_repair("value", source, &expected, "foo", "pkg/main.kio", &[]);
}

#[test]
fn invalid_name_repairs_withhold_collisions_and_letterless_guesses() {
    for source in [
        "module pkg/main;\npub fn foo123() -> . { () }\npub fn foo_123() -> . { () }\n",
        "module pkg/main;\npub fn _123() -> . { () }\n",
    ] {
        let dir = TempDir::new("identifier-repair-withheld");
        dir.write_pkg_root_package();
        let uri = path_to_file_uri(&dir.write("pkg/main.kio", source));
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        lsp.send_notification("textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}));
        let publish = wait_for_publish_version(&mut lsp, &uri, 1);
        let diagnostic = publish["diagnostics"].as_array().unwrap().first().unwrap();
        assert_eq!(diagnostic["code"], 11);
        let actions = send_code_action(&mut lsp, &uri, diagnostic);
        assert!(
            actions["result"].as_array().is_some_and(Vec::is_empty),
            "{actions:?}"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn marked_type_and_numbered_words_are_valid_without_a_naming_action() {
    let source = "module pkg/main;\npub type _Foo = .;\npub fn a1_b2() -> . { () }\n";
    let invalid_source = source.replace("_Foo", "_FooBar");
    let dir = TempDir::new("identifier-valid-roles");
    dir.write_pkg_root_package();
    let uri = path_to_file_uri(&dir.write("pkg/main.kio", &invalid_source));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": invalid_source}}),
    );
    let initial = wait_for_publish_version(&mut lsp, &uri, 1);
    assert_eq!(initial["diagnostics"][0]["code"], 11, "{initial:?}");
    send_full_text_change(&mut lsp, &uri, 2, source);
    let publish = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        publish["diagnostics"].as_array().is_some_and(Vec::is_empty),
        "{publish:?}"
    );
    assert_eq!(lsp.shutdown(), 0);
}
