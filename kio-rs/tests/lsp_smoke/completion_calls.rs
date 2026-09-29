use super::*;

#[test]
fn lsp_protocol_completion_argument_head_tracks_current_public_type() {
    let dir = TempDir::new("completion-public-call-head");
    dir.write_pkg_root_package();
    let sources = [
        (
            "fn identity(value: .) -> . { value }",
            "identity",
            false,
            false,
        ),
        (
            "fn identity[A](value: A) -> A { value }",
            "identity",
            true,
            false,
        ),
        (
            "fn identity(value: .) -> . { value }",
            "missing",
            true,
            true,
        ),
        (
            "fn identity(value: .) -> . { value }",
            "identity",
            false,
            false,
        ),
    ];
    let source = |declaration: &str, callee: &str| {
        format!(
            "module pkg/main; type Item = .; {declaration} pub fn run(value: .) -> . {{ {callee}(value) }}"
        )
    };
    let path = dir.write("pkg/main.kio", &source(sources[0].0, sources[0].1));
    let uri = path_to_file_uri(&path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    for (index, (declaration, callee, admits_type, incomplete)) in sources.into_iter().enumerate() {
        let text = source(declaration, callee);
        let version = index as i64 + 1;
        if index == 0 {
            lsp.send_notification("textDocument/didOpen", json!({
                "textDocument": { "uri": uri, "languageId": "kio", "version": version, "text": text },
            }));
        } else {
            lsp.send_notification(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{ "text": text }],
                }),
            );
        }
        let start = text.rfind("(value)").expect("call argument") + 1;
        let (line, character) = source_position(&text, start);
        let deadline = Instant::now() + Duration::from_secs(15);
        let result = loop {
            let result = send_completion(&mut lsp, &uri, line, character);
            if incomplete || result["isIncomplete"] == false {
                break result;
            }
            assert!(
                Instant::now() < deadline,
                "current public type not ready: {result}"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            completion_labels(&result).contains(&"Item"),
            admits_type,
            "version {version}: {result}"
        );
        assert_eq!(
            result["isIncomplete"], incomplete,
            "version {version}: {result}"
        );
        let value = result["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|item| item["label"] == "value")
            .expect("value argument candidate");
        assert_eq!(
            value["textEdit"]["range"],
            json!({
                "start": { "line": line, "character": character },
                "end": { "line": line, "character": character + 5 },
            })
        );
    }
    assert_eq!(lsp.shutdown(), 0);
}
