use super::*;

#[test]
fn current_provider_headers_authenticate_candidates_and_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provider.kio");
    std::fs::write(&path, "module provider;").unwrap();
    let path = std::fs::canonicalize(path).unwrap();
    let uri = crate::lsp::diagnostics::path_to_uri(&path, directory.path()).unwrap();
    let mut state = crate::lsp::state::ServerState::new();
    let module = crate::pass::parser::parse_module_file_lazy(
        "module app; import provider as p; import provider(value); fn value() -> . { () }",
    )
    .unwrap()
    .module;
    let mut analysis = LspAnalysis {
        position_index: crate::pass::typecheck_full::PositionIndex::new(),
        file_to_module: BTreeMap::from([(path.clone(), "provider".to_owned())]),
        sources: std::collections::HashMap::from([(
            path.clone(),
            "module provider; pub fn stale() {}".to_owned(),
        )]),
        label_reuse_indexes: Default::default(),
        generated_label_nominals: Default::default(),
        root_package_lowered: crate::pass::resolve::Package::from_parts(BTreeMap::new(), None),
        warnings: Vec::new(),
    };
    for (version, name) in [(1, "first"), (2, "second")] {
        state.open(uri.clone(), format!("module provider;\n/// {name} documentation.\npub fn {name}() -> . {{ incomplete body }}\npub fn value(arg: .) -> . {{ arg }}\nfn hidden() {{}}"), version);
        let sources = CompletionSources {
            local: Some(&analysis),
            catalog: Some(&analysis),
            declarations: Some(&analysis.file_to_module),
            state: Some(&state),
        };
        let qualified = names(&module, "p.", None, &sources);
        assert_eq!(
            labels(&qualified),
            [format!("p.{name}"), "p.value".to_owned()]
        );
        let item = qualified
            .items
            .iter()
            .find(|item| item.label == format!("p.{name}"))
            .unwrap();
        assert!(format!("{:?}", item.documentation).contains(&format!("{name} documentation")));
        assert!(!qualified.is_incomplete);
        let local = names(&module, "", None, &sources);
        let selected = local
            .items
            .iter()
            .find(|item| item.label == "value")
            .unwrap();
        assert!(selected.detail.as_ref().unwrap().contains("value()"));
        assert!(!selected.detail.as_ref().unwrap().contains("arg"));
    }
    state.open(uri.clone(), "module wrong; pub fn first() {}".into(), 3);
    let result = names(
        &module,
        "p.",
        None,
        &CompletionSources {
            local: Some(&analysis),
            catalog: Some(&analysis),
            declarations: Some(&analysis.file_to_module),
            state: Some(&state),
        },
    );
    assert!(result.items.is_empty());
    assert!(result.is_incomplete);
    state.open(uri, "module provider; pub fn first() {}".into(), 4);
    analysis.file_to_module.insert(
        directory.path().join("other/provider.kio"),
        "provider".into(),
    );
    let result = names(
        &module,
        "p.",
        None,
        &CompletionSources {
            local: Some(&analysis),
            catalog: Some(&analysis),
            declarations: Some(&analysis.file_to_module),
            state: Some(&state),
        },
    );
    assert!(result.items.is_empty());
    assert!(result.is_incomplete);
}

#[test]
fn bounded_doc_and_ordinary_completion_requests() {
    let mut source = String::from("module app;\n");
    for index in 0..200 {
        source.push_str(&format!(
            "/// Description of declaration {index}.\nfn value{index}() -> . {{ () }}\n"
        ));
    }
    let doc = format!("{source}/// [`value1|`]\nfn documented() {{}}\n");
    let ordinary = format!("{source}fn ordinary() {{ val| }}");
    for (label, marked_source) in [("doc", doc), ("ordinary", ordinary)] {
        let start = std::time::Instant::now();
        for _ in 0..20 {
            let result = marked(&marked_source).1;
            assert!(result.items.iter().any(|item| item.label == "value1"));
        }
        eprintln!(
            "{label}: 20 completion requests over 200 documented declarations: {:?}",
            start.elapsed()
        );
    }
}

#[test]
fn exact_utf16_operand_edits_preserve_program_and_framing() {
    let (source, result) =
        marked("module app;\r\n\t/// é😀 [`@signature va|lue`]  \r\nfn value() -> . { () }\r\n");
    let item = result
        .items
        .iter()
        .find(|item| item.label == "value")
        .unwrap();
    let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
        panic!("plain edit")
    };
    assert_eq!(edit.range.start, Position::new(1, 22));
    let changed = apply(&source, edit);
    assert_eq!(changed, source);
    let (source, result) = marked(
        "module app;\r\n\t/// é😀 [`@signature va|lue_old`]  \r\nfn value() -> . { () }\r\n",
    );
    let Some(CompletionTextEdit::Edit(edit)) = &result.items[0].text_edit else {
        panic!("plain edit")
    };
    let changed = apply(&source, edit);
    assert!(changed.contains("[`@signature value`]  \r\n"));
    assert_eq!(tokens(&source), tokens(&changed));
}

fn tokens(source: &str) -> Vec<(String, String)> {
    crate::pass::lexer::lex(source)
        .unwrap()
        .iter()
        .map(|token| {
            (
                format!("{:?}", token.kind),
                source[token.span.start as usize..token.span.end as usize].to_owned(),
            )
        })
        .collect()
}

fn apply(source: &str, edit: &TextEdit) -> String {
    let index = LineIndex::new(source);
    let lo = index.position_to_offset(LspPosition {
        line: edit.range.start.line,
        character: edit.range.start.character,
    }) as usize;
    let hi = index.position_to_offset(LspPosition {
        line: edit.range.end.line,
        character: edit.range.end.character,
    }) as usize;
    format!("{}{}{}", &source[..lo], edit.new_text, &source[hi..])
}

#[test]
fn embedded_plain_edits_reframe_multiline_and_reject_unsafe_shapes() {
    let source =
        "module app;\r\n\t/// ```kio {@}\r\n\t/// old\r\n\t/// ```\r\nfn value() -> . { () }\r\n";
    let lexed = crate::pass::lexer::lex_prefix(source);
    let docs = DocSource::from_lexed(source, &lexed);
    let doc = &docs[0];
    let cursor = doc.text.find("old").unwrap();
    let Some(DocCursor::Fence { body, .. }) = completion_cursor(doc, cursor) else {
        panic!("fence")
    };
    let synthetic = format!("fn wrapper() {{\n{}\n}}", &doc.text[body.clone()]);
    let start = "fn wrapper() {\n".len();
    let index = LineIndex::new(&synthetic);
    let item = CompletionItem {
        label: "value".into(),
        text_edit: Some(CompletionTextEdit::Edit(text_edit(
            &index,
            crate::span::Span::new(start as u32, (start + 3) as u32),
            "let local = ();\nvalue()".into(),
        ))),
        ..Default::default()
    };
    let mapped =
        map_embedded_item(item.clone(), &synthetic, source, doc, start, body.clone()).unwrap();
    let Some(CompletionTextEdit::Edit(edit)) = mapped.text_edit else {
        panic!("mapped edit")
    };
    let changed = apply(source, &edit);
    assert!(changed.contains("\t/// let local = ();\r\n\t/// value()"));
    assert_eq!(tokens(source), tokens(&changed));
    let new_lexed = crate::pass::lexer::lex_prefix(&changed);
    let new_docs = DocSource::from_lexed(&changed, &new_lexed);
    assert_eq!(
        new_docs[0].text,
        doc.text.replace("old", "let local = ();\nvalue()")
    );
    for range in [0..start + 1, start..synthetic.len()] {
        let mut unsafe_item = item.clone();
        unsafe_item.text_edit = Some(CompletionTextEdit::Edit(text_edit(
            &index,
            crate::span::Span::new(range.start as u32, range.end as u32),
            "bad".into(),
        )));
        assert!(
            map_embedded_item(unsafe_item, &synthetic, source, doc, start, body.clone()).is_none()
        );
    }
    let mut additional = item.clone();
    additional.additional_text_edits = Some(vec![text_edit(
        &index,
        crate::span::Span::new(0, 0),
        "escape".into(),
    )]);
    assert!(map_embedded_item(additional, &synthetic, source, doc, start, body.clone()).is_none());
    let mut snippet = item;
    snippet.insert_text_format = Some(lsp_types::InsertTextFormat::SNIPPET);
    snippet.insert_text = Some("${1:arbitrary\ntext}$0".into());
    let mapped = map_embedded_item(snippet, &synthetic, source, doc, start, body).unwrap();
    assert_eq!(mapped.insert_text_format, None);
    let Some(CompletionTextEdit::Edit(edit)) = mapped.text_edit else {
        panic!("plain fallback")
    };
    assert_eq!(edit.new_text, "value");
    assert_eq!(tokens(source), tokens(&apply(source, &edit)));
}

#[test]
fn doc_scope_keeps_operator_label_member_and_type_identities() {
    let prefix = concat!(
        "module app; import __intrinsics__;\n",
        "fn value() -> . { () }\n",
        "fn pair(a: ., b: .) -> . { a }\n",
        "op _ + _ { impl pair }\n",
        "newtype Box : . { constructor make; projector read; };\n",
        "labels { field: . };\n",
        "type {alias} = {field};\n",
    );
    for (term, expected) in [
        ("+|", vec!["+"]),
        ("op _ +|", vec!["op _ + _"]),
        ("{a|lias}", vec!["{alias}"]),
        ("Fi|", vec!["Field"]),
        ("Box.|", vec!["Box.make", "Box.read"]),
        ("@type ma|", vec!["make"]),
        ("@type F|", vec![]),
        ("@type {a|lias}", vec![]),
    ] {
        let (_, list) = marked(&format!("{prefix}/// [`{term}`]\nfn doc() {{}}\n"));
        assert_eq!(labels(&list), expected, "{term}");
    }
    let (_, list) = marked(&format!("{prefix}/// [`__le|`]\nfn doc() {{}}\n"));
    assert!(labels(&list).contains(&"__left__"));
}

#[test]
fn doc_fences_use_module_harness_and_current_local_scope() {
    for opening in ["```kio {@}", "<!--kio {@}"] {
        let closing = if opening.starts_with("<!--") {
            "-->"
        } else {
            "```"
        };
        let (_, list) = marked(&format!(
            "module app;\n/// {opening}\n/// let local = (); lo|\n/// {closing}\nfn documented(parameter: .) -> . {{ parameter }}\nfn other() {{}}"
        ));
        assert!(labels(&list).contains(&"local"));
        assert!(!labels(&list).contains(&"parameter"));
        assert!(!labels(&list).contains(&"kiodoc_completion"));
    }
    for attrs in [
        "{}",
        "{@named}",
        "{@ unknown}",
        "{@ variant=module}",
        "{@ check_exit_code=oops}",
    ] {
        let (_, list) = marked(&format!(
            "module app;\n/// ```kio {attrs}\n/// va|\n/// ```\nfn value() {{}}"
        ));
        assert!(list.items.is_empty(), "{attrs}");
    }
}

#[test]
fn nonmodule_docs_and_unauthenticated_trivia_never_invent_names() {
    for (suffix, header) in [
        (".pkg", "package app;"),
        (".dep", "dependency app;"),
        (".lock", "lock app;"),
        (".sig", "signature app;"),
    ] {
        let source = format!("{header}\n/// [`na");
        let uri = crate::lsp::util::test_file_uri(format!("/app{suffix}.kio"));
        let position = LineIndex::new(&source).to_position(source.len() as u32);
        let result = handle_completion(
            &uri,
            &Position::new(position.line, position.character),
            Some(&source),
            None,
        )
        .unwrap();
        assert!(result.items.is_empty());
    }
    for source in [
        "module app; fn bad() { \"unterminated\n/// [`va|",
        "module app;\n//// [`va|",
        "module app;\n///[`va|",
    ] {
        assert!(marked(source).1.items.is_empty(), "{source}");
    }
}

fn marked(text: &str) -> (String, CompletionList) {
    let offset = text.find('|').unwrap();
    let source = text.replacen('|', "", 1);
    let position = LineIndex::new(&source).to_position(offset as u32);
    let uri = crate::lsp::util::test_file_uri("/app.kio");
    let result = handle_completion(
        &uri,
        &Position::new(position.line, position.character),
        Some(&source),
        None,
    )
    .unwrap();
    (source, result)
}

fn labels(list: &CompletionList) -> Vec<&str> {
    list.items.iter().map(|item| item.label.as_str()).collect()
}

#[test]
fn keywords_and_module_names_recover_without_function_locals() {
    let (_, qualified) =
        marked("module app; import provider as path;\n/// [`pa|th.member`]\nfn value() {}\n");
    assert_eq!(labels(&qualified), ["path"]);
    for (tail, expected) in [
        ("[`@|", vec!["signature", "source", "type"]),
        ("[`@ty|pe`]", vec!["type"]),
        ("[`va|lue`]", vec!["value"]),
        ("[`@type |`]", vec!["documented", "value"]),
    ] {
        let text = format!(
            "module app;\ntype Shape = .;\n/// {tail}\nfn documented(parameter: .) -> . {{ parameter }}\nfn value() -> . {{ () }}\n"
        );
        let (_, list) = marked(&text);
        assert_eq!(labels(&list), expected, "{text}");
        assert!(!list.is_incomplete);
    }
}

#[test]
fn ordinary_prose_and_markdown_overrides_stay_suppressed() {
    for text in [
        "module app;\n/// val|ue\nfn value() {}",
        "module app;\n// [`va|lue`]\nfn value() {}",
        "module app;\n/// `[`va|lue`]`\nfn value() {}",
        "module app;\n/// [`va|lue`](https://example.test)\nfn value() {}",
        "module app;\n/// [`va|lue`]\n/// [value]: https://example.test\nfn value() {}",
        "module app;\n/// [key]: https://example.test/[`va|lue`]\nfn value() {}",
        "module app;\n/// ```kio {ignore}\n/// va|\n/// ```\nfn value() {}",
        "module app;\n/// ```kio\n/// va|\n/// ```\nfn value() {}",
    ] {
        assert!(marked(text).1.items.is_empty(), "{text}");
    }
}
