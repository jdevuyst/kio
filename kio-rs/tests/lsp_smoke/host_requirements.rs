use super::*;

#[test]
fn host_requirement_locations_follow_provider_only_overlays() {
    for (declaration, import, body, replacement, requirement) in [
        (
            "pub host fn take(value: W) -> .;",
            "import pkg/provider as p;",
            "p.take(())",
            "p.take(p.make(()))",
            "W",
        ),
        (
            "pub host fn take(value: W) -> .;",
            "import pkg/provider as p;",
            "let f = p.take; f(())",
            "let f = p.take; f(p.make(()))",
            "W",
        ),
        (
            "pub host fn take(value: W) -> .;",
            "import pkg/provider(take, make);",
            "let f = take; f(())",
            "let f = take; f(make(()))",
            "W",
        ),
        (
            "pub host fn tick() -> .;",
            "import pkg/provider as p;",
            "p.tick(p.tick)",
            "p.tick()",
            "fn tick() -> .",
        ),
        (
            "pub fn tick() -> . { () }",
            "import pkg/provider as p;",
            "p.tick(p.tick)",
            "p.tick()",
            "fn tick() -> .",
        ),
    ] {
        let dir = TempDir::new("host-requirement-source");
        dir.write_pkg_root_package();
        dir.write("pkg/origin.kio", "module pkg/origin;\npub host type W;\n");
        dir.write("pkg/decoy.kio", "module pkg/decoy;\npub type W = .;\n");
        let provider = format!(
            "module pkg/provider;\nimport pkg/origin as o;\npub type W = o.W;\npub host fn make(value: .) -> W;\n{declaration}\n"
        );
        let provider_path = dir.write("pkg/provider.kio", &provider);
        let source = format!(
            "module pkg/main;\n{import}\n// Caller-only padding places its call well beyond the provider header and annotations.\nfn caller() -> . {{ {body} }}\n"
        );
        let main_path = dir.write("pkg/main.kio", &source);
        let uri = path_to_file_uri(&main_path);
        let provider_uri = path_to_file_uri(&provider_path);
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        for (file_uri, text) in [(&provider_uri, provider.as_str()), (&uri, source.as_str())] {
            lsp.send_notification(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": file_uri, "languageId": "kio", "version": 1, "text": text,
                }}),
            );
        }
        let requirement_range = |text: &str| {
            let start = if requirement == "W" {
                text.find("value: W").unwrap() + "value: ".len()
            } else {
                text.find(requirement).unwrap()
            };
            source_range(text, start, requirement.len())
        };
        let assert_signature = |publish: &Value, provider: &str| {
            assert_eq!(publish["version"], 1, "{publish:?}");
            let errors = publish["diagnostics"].as_array().unwrap();
            assert_eq!(errors.len(), 1, "{publish:?}");
            assert_eq!(errors[0]["code"], 14, "{publish:?}");
            let (argument, width) = if requirement == "W" {
                (source.rfind("()").unwrap(), 2)
            } else {
                (source.rfind("p.tick").unwrap(), "p.tick".len())
            };
            assert_eq!(
                json_range(&errors[0]["range"]),
                source_range(&source, argument, width)
            );
            let related = errors[0]["relatedInformation"].as_array().unwrap();
            assert_eq!(related.len(), 1, "{publish:?}");
            assert_eq!(related[0]["location"]["uri"], provider_uri, "{publish:?}");
            assert_eq!(
                json_range(&related[0]["location"]["range"]),
                requirement_range(provider),
                "{publish:?}"
            );
        };
        let initial = wait_for_publish_version(&mut lsp, &uri, 1);
        assert_signature(&initial, &provider);
        let shifted = provider.replace(
            declaration,
            &format!("// 😀 shifted signature\n\n{declaration}"),
        );
        send_full_text_change(&mut lsp, &provider_uri, 2, &shifted);
        let refreshed = wait_for_publish_version(&mut lsp, &uri, 1);
        assert_signature(&refreshed, &shifted);
        assert_eq!(fs::read_to_string(&provider_path).unwrap(), provider);

        let grown = format!("{shifted}\npub host fn unrelated(value: .) -> .;\n");
        send_full_text_change(&mut lsp, &provider_uri, 3, &grown);
        let after_growth = wait_for_publish_version(&mut lsp, &uri, 1);
        assert_signature(&after_growth, &grown);

        let corrected = source.replace(body, replacement);
        send_full_text_change(&mut lsp, &uri, 2, &corrected);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert!(
            cleared["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .all(|diagnostic| diagnostic["severity"] != 1),
            "{cleared:?}"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}
