use super::*;

#[test]
fn neutral_block_labels_use_only_the_current_selected_header() {
    let source = "module provider; pub elab choose : (. & .) -> . { trailing product; trailing product otherwise; impl implementation }";
    let mut names = NameSet {
        current_module_src: Some("module app; import provider(choose);".to_owned()),
        expression_provider_sources: std::collections::BTreeMap::from([(
            "provider".to_owned(),
            Arc::from(source),
        )]),
        ..Default::default()
    };
    for (input, expected) in [
        ("choose! { () } oth", vec!["otherwise"]),
        ("choose! { () } otherwise { () } ", vec![]),
        ("unknown! { () } oth", vec![]),
    ] {
        let completion = complete(&names, &AstScopeProvider::new(&names), input, input.len());
        assert_eq!(
            completion
                .candidates
                .iter()
                .map(|candidate| candidate.label.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{input}: {completion:?}"
        );
    }
    names.expression_provider_sources.insert(
        "provider".to_owned(),
        Arc::from(source.replace("product otherwise;", "product alternate;")),
    );
    let input = "choose! { () } ";
    let completion = complete(&names, &AstScopeProvider::new(&names), input, input.len());
    assert_eq!(
        completion
            .candidates
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect::<Vec<_>>(),
        vec!["alternate"]
    );
    names.expression_provider_sources.clear();
    assert!(
        complete(&names, &AstScopeProvider::new(&names), input, input.len())
            .candidates
            .is_empty()
    );
}

fn names() -> NameSet {
    NameSet {
        current_module_src: Some(
            "module app; type Item = .; fn identity[A](x: A) -> A { x }".to_owned(),
        ),
        ..Default::default()
    }
}

#[test]
fn parser_context_repl_suppresses_literals_and_binders() {
    let names = names();
    let provider = AstScopeProvider::new(&names);
    for input in ["\"id", ".t", "123", ".(id", ".(x: .) { let id", "// id"] {
        let result = complete(&names, &provider, input, input.len());
        assert!(
            result.candidates.is_empty(),
            "{input}: {:?}",
            result.candidates
        );
    }
}

#[test]
fn parser_context_repl_incomplete_expression_carriers_keep_real_bindings() {
    let names = NameSet {
        current_module_src: Some("module app; fn identity(x: .) -> . { x } fn add(x: ., y: .) { x } op _ + __ { impl add; };".to_owned()),
        ..Default::default()
    };
    let provider = AstScopeProvider::new(&names);
    for input in ["1 + id", "identity(id", "(1, ", "f(a, b", ".(y: .) { y.>id"] {
        let candidates = provider.in_scope(input, input.len());
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.label == "identity"),
            "{input}: {candidates:?}; context {:?}",
            crate::pass::parser::probe_tooling(input, None, Some(input.len() as u32), None)
                .facts
                .cursor
        );
    }
    assert!(provider.in_scope("}}}", 3).is_empty());
}

#[test]
fn parser_context_repl_routes_namespaces_and_preserves_case() {
    let names = names();
    let provider = AstScopeProvider::new(&names);
    for (input, included, excluded) in [
        (".(x: I", "Item", "identity"),
        ("identity(I", "Item", "identity"),
        ("identity(i", "identity", "Item"),
        (".(local: .) { lo", "local", "Item"),
    ] {
        let result = complete(&names, &provider, input, input.len());
        let labels: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect();
        assert!(
            labels.contains(&included),
            "{input}: {labels:?}; context {:?}",
            crate::pass::parser::probe_tooling(input, None, Some(input.len() as u32), None)
                .facts
                .cursor
        );
        assert!(!labels.contains(&excluded), "{input}: {labels:?}");
    }
}

#[test]
fn parser_context_repl_namespace_follows_the_typed_name_head() {
    let names = names();
    let input = "identity(IT";
    let result = complete(&names, &AstScopeProvider::new(&names), input, input.len());
    let labels: Vec<_> = result
        .candidates
        .iter()
        .map(|candidate| candidate.label.as_str())
        .collect();
    assert!(labels.contains(&"Item"), "{labels:?}");
    assert!(!labels.contains(&"identity"), "{labels:?}");
    assert!(language_match("_", "_Item").is_some());
    assert!(language_match("_", "_identity").is_some());
}

#[test]
fn parser_context_repl_underscore_prefix_retains_the_complete_atom() {
    let names = NameSet {
        current_module_src: Some("module app; type Item = .; type _Item = .; fn identity[A](x: A) -> A { x } fn _identity(x: .) -> . { x }".to_owned()),
        ..Default::default()
    };
    let provider = AstScopeProvider::new(&names);
    let mut failures = Vec::new();
    for (input, included, excluded, start) in [
        (".(x: _", vec!["_Item"], vec!["_identity"], 5),
        ("identity(_", vec!["_Item", "_identity"], vec![], 9),
    ] {
        let result = complete(&names, &provider, input, input.len());
        let labels: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect();
        if included.iter().any(|name| !labels.contains(name))
            || excluded.iter().any(|name| labels.contains(name))
            || result.replace != (start..input.len())
        {
            failures.push(format!(
                "{input}: {labels:?}; replace={:?}; context={:?}",
                result.replace,
                crate::pass::parser::probe_tooling(input, None, Some(input.len() as u32), None)
                    .facts
                    .cursor
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn parser_context_repl_known_value_argument_excludes_type_names() {
    call_types::CALL_HEAD_WORK.with(|work| work.set((0, 0)));
    let root = std::path::PathBuf::from("/kio-repl-tests/completion-call-types");
    let files = std::collections::BTreeMap::from([
        (
            root.join("app.pkg.kio"),
            "package app; bridge { app/**; }".to_owned(),
        ),
        (
            root.join("app/main.kio"),
            "module app/main; import app/provider(remote); import app/provider as api; type Item = .; newtype Box : . { constructor make; projector read; }; type Alias = Box; fn identity(x: .) -> . { x } fn choose[A](x: A) -> A { x } fn staged(value: .)[A](x: A) -> A { x }".to_owned(),
        ),
        (
            root.join("app/provider.kio"),
            "module app/provider; pub fn remote(x: .) -> . { x } pub newtype Box : . { pub constructor make; projector hidden; }; pub type Alias = Box; pub newtype Poly[A] : A { pub constructor make; projector hidden; }; newtype Secret : . { pub constructor make; projector hidden; };".to_owned(),
        ),
        (
            root.join("app/poly.kio"),
            "module app/poly; type Item = .; fn identity[A](x: A) -> A { x }".to_owned(),
        ),
    ]);
    let mut session = Session::new_in_memory(root, files);
    let loaded = crate::repl_core::commands::Command::Load("app/main".to_owned())
        .run(&mut session, crate::repl_core::highlight::Palette::plain());
    assert!(
        loaded.output.contains("loaded app/main"),
        "{}",
        loaded.output
    );
    let names = NameSet::from_session(&session);
    let provider = AstScopeProvider::new(&names);
    let mut failures = Vec::new();
    for (input, type_allowed) in [
        ("identity(I", false),
        ("choose(I", true),
        ("remote(I", false),
        ("api.remote(I", false),
        ("api.Box.make(I", false),
        ("api.Alias.make(I", false),
        ("api.Poly.make(I", true),
        ("api.Secret.make(I", true),
        ("api.Box.hidden(I", true),
        (".(api: .) { api.Box.make(I", true),
        ("Box.make(I", false),
        ("Alias.make(I", false),
        ("staged(I", false),
        ("staged((), I", true),
        ("missing(I", true),
        (".(identity: . -> .) { identity(I", true),
        (".[Box](x: Box) { Box.make(I", true),
    ] {
        let result = complete(&names, &provider, input, input.len());
        let labels: Vec<_> = result
            .candidates
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        if labels.contains(&"Item") != type_allowed {
            failures.push(format!("{input}: {labels:?}"));
        }
    }
    let (builds, lookups) = call_types::CALL_HEAD_WORK.with(|work| work.get());
    assert_eq!(builds, 1);
    assert_eq!(
        lookups, 13,
        "only first, unshadowed call heads consult the cache"
    );
    let mut stale = names.clone();
    stale.current_module_src.as_mut().unwrap().push(' ');
    let input = "identity(I";
    assert!(
        complete(&stale, &AstScopeProvider::new(&stale), input, input.len())
            .candidates
            .iter()
            .any(|item| item.label == "Item")
    );
    let loaded = crate::repl_core::commands::Command::Load("app/poly".to_owned())
        .run(&mut session, crate::repl_core::highlight::Palette::plain());
    assert!(
        loaded.output.contains("loaded app/poly"),
        "{}",
        loaded.output
    );
    let refreshed = NameSet::from_session(&session);
    assert!(
        complete(
            &refreshed,
            &AstScopeProvider::new(&refreshed),
            input,
            input.len()
        )
        .candidates
        .iter()
        .any(|item| item.label == "Item")
    );
    assert_eq!(call_types::CALL_HEAD_WORK.with(|work| work.get().0), 2);
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn parser_context_repl_replaces_whole_atom_without_inventing_bindings() {
    let names = names();
    let provider = AstScopeProvider::new(&names);
    let result = complete(&names, &provider, "identity", 2);
    assert_eq!(result.replace, 0..8);
    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.label == "identity")
    );
    let empty = NameSet::default();
    let result = complete(&empty, &AstScopeProvider::new(&empty), "i", 1);
    assert!(result.candidates.is_empty(), "{:?}", result.candidates);
}

#[test]
fn parser_context_repl_routes_expression_operators() {
    let names = NameSet {
        current_module_src: Some("module app; fn identity(x: .) { x } op _ + __ { impl identity; }; op ~ _ { impl identity; }; varop [* *] { foldl identity identity; };".to_owned()),
        ..Default::default()
    };
    let provider = AstScopeProvider::new(&names);
    for (input, expected, excluded) in [
        (":normalize ", "~", "+"),
        (":normalize ", "[*", "+"),
        ("x +", "+", "identity"),
        ("~ ", "identity", "+"),
    ] {
        let result = complete(&names, &provider, input, input.len());
        let labels: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect();
        assert!(labels.contains(&expected), "{input:?}: {labels:?}");
        assert!(!labels.contains(&excluded), "{input:?}: {labels:?}");
    }
}

#[test]
fn parser_context_repl_newtype_and_identity_alias_members() {
    let names = NameSet {
        current_module_src: Some("module app; newtype Box[A] : A { constructor make; projector read; }; type Alias[A] = Box(A); type Partial = Box;".to_owned()),
        ..Default::default()
    };
    let provider = AstScopeProvider::new(&names);
    for input in ["Box.", "Alias."] {
        let result = complete(&names, &provider, input, input.len());
        let mut labels: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect();
        labels.sort_unstable();
        assert_eq!(labels, ["make", "read"], "{input:?}");
    }
    assert!(
        complete(&names, &provider, "Partial.", 8)
            .candidates
            .is_empty()
    );
}

#[test]
fn parser_context_repl_replaces_started_multi_run_operator_once() {
    let names = NameSet {
        current_module_src: Some(
            "module app; fn choose(x: ., y: .) { x } op _ && ++ _ { impl choose; };".to_owned(),
        ),
        ..Default::default()
    };
    let input = "x && +";
    let result = complete(&names, &AstScopeProvider::new(&names), input, input.len());
    assert_eq!(result.replace, 2..6);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect::<Vec<_>>(),
        ["&& ++"]
    );
}
