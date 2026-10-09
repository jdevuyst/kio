use super::*;

struct ClosedCase {
    kind: &'static str,
    prefix: String,
    choices: Vec<(String, String)>,
    incomplete: bool,
}

fn closed_cases() -> Vec<ClosedCase> {
    let mut cases = Vec::new();
    let mut add = |kind, prefix: &str, choices: &[(&str, &str)]| {
        cases.push(ClosedCase {
            kind,
            prefix: prefix.to_owned(),
            choices: choices
                .iter()
                .map(|(name, tail)| ((*name).to_owned(), (*tail).to_owned()))
                .collect(),
            incomplete: false,
        });
    };
    // Grammar sections: module declarations, host declarations, and surface items.
    let declarations = [
        ("elab", " make: . { impl run; };"),
        ("equiv", " same() { (); () }"),
        ("fn", " run() { () }"),
        ("host", " type A;"),
        ("import", " __intrinsics__;"),
        ("labels", " { field: . };"),
        ("literal", " one = 1;"),
        ("newtype", " A : . { constructor make; projector read; };"),
        ("op", " ~ _ { impl run; };"),
        ("pub", " fn run() { () }"),
        ("pure", " fn run() { () }"),
        ("rec", "(loop) fn run() { () }"),
        ("type", " A = .;"),
        ("varop", " [* *] { foldr step base; };"),
    ];
    add("", "", &[("module", " app;")]);
    add("", "module app; ", &declarations);
    add(
        "",
        "module app; pub ",
        &declarations
            .iter()
            .copied()
            .filter(|(name, _)| !["pub", "equiv", "import"].contains(name))
            .collect::<Vec<_>>(),
    );
    add(
        "",
        "module app; pure ",
        &[("fn", " run() { () }"), ("pub", " fn run() { () }")],
    );
    add("", "module app; pub pure ", &[("fn", " run() { () }")]);
    add("", "module app; pure pub ", &[("fn", " run() { () }")]);
    add(
        "",
        "module app; host ",
        &[("fn", " run() -> .;"), ("pub", " type A;"), ("type", " A;")],
    );
    add(
        "",
        "module app; host pub ",
        &[("fn", " run() -> .;"), ("type", " A;")],
    );
    add(
        "",
        "module app; pub host ",
        &[("fn", " run() -> .;"), ("type", " A;")],
    );
    add(
        "",
        "module app; type Prior = .; ",
        &declarations
            .iter()
            .copied()
            .filter(|(name, _)| *name != "import")
            .collect::<Vec<_>>(),
    );
    for visibility in ["", "pub "] {
        add(
            "",
            &format!("module app; {visibility}rec "),
            &[
                ("newtype", " A : A { constructor make; projector read; };"),
                ("labels", " A = { next: A };"),
            ],
        );
    }
    add(
        "",
        "module app; rec { ",
        &[
            (
                "labels",
                " A = { next: B }; newtype B : A { constructor make; projector read; }; }",
            ),
            (
                "newtype",
                " A : B { constructor make; projector read; }; type B = A; }",
            ),
            (
                "pub",
                " newtype A : B { constructor make; projector read; }; type B = A; }",
            ),
            (
                "type",
                " A = B; newtype B : A { constructor make; projector read; }; }",
            ),
        ],
    );
    add(
        "",
        "module app; rec { pub ",
        &[
            (
                "labels",
                " A = { next: B }; newtype B : A { constructor make; projector read; }; }",
            ),
            (
                "newtype",
                " A : B { constructor make; projector read; }; type B = A; }",
            ),
            (
                "type",
                " A = B; newtype B : A { constructor make; projector read; }; }",
            ),
        ],
    );
    add(
        "",
        "module app; rec(loop) { ",
        &[("fn", " run() { () } }"), ("pub", " fn run() { () } }")],
    );
    add(
        "",
        "module app; rec(loop) { pub ",
        &[("fn", " run() { () } }")],
    );
    add("", "module app; import dep/api ", &[("as", " api;")]);
    add("", "module app; host type A ", &[("role", "(str);")]);
    add("", "module app; host type A[T] ", &[]);
    add(
        "",
        "module app; host type A role(",
        &[
            ("bool", ");"),
            ("f32", ");"),
            ("f64", ");"),
            ("i8", ");"),
            ("i16", ");"),
            ("i32", ");"),
            ("i64", ");"),
            ("i128", ");"),
            ("str", ");"),
            ("u8", ");"),
            ("u16", ");"),
            ("u32", ");"),
            ("u64", ");"),
            ("u128", ");"),
        ],
    );
    add(
        "",
        "module app; host type A role(str) { ",
        &[("owned", " };")],
    );
    add("", "module app; host type A role(str) { owned ", &[]);
    add(
        "",
        "module app; newtype A : . { ",
        &[
            ("constructor", " make; projector read; };"),
            ("projector", " read; constructor make; };"),
            ("pub", " constructor make; projector read; };"),
        ],
    );
    add(
        "",
        "module app; newtype A : . { pub ",
        &[
            ("constructor", " make; projector read; };"),
            ("projector", " read; constructor make; };"),
        ],
    );
    add(
        "",
        "module app; newtype A : . { constructor make; ",
        &[("projector", " read; };"), ("pub", " projector read; };")],
    );
    add(
        "",
        "module app; newtype A : . { projector read; ",
        &[
            ("constructor", " make; };"),
            ("pub", " constructor make; };"),
        ],
    );
    add(
        "",
        "module app; newtype A : . { projector read; pub ",
        &[("constructor", " make; };")],
    );
    add(
        "",
        "module app; newtype A : . { constructor make; projector read; ",
        &[],
    );
    add(
        "",
        "module app; elab make: . -> . { ",
        &[
            ("captures", " (run); impl run; };"),
            ("impl", " run; };"),
            ("trailing", " product; impl run; };"),
        ],
    );
    add(
        "",
        "module app; elab make: . -> . { captures (run); ",
        &[("impl", " run; };"), ("trailing", " product; impl run; };")],
    );
    add(
        "",
        "module app; elab make: . { impl(",
        &[("fills", ") run; };")],
    );
    add(
        "",
        "module app; elab make: . -> . { impl run; ",
        &[("captures", " (run); };"), ("trailing", " product; };")],
    );
    add(
        "",
        "module app; elab make: [A] A -> . { trailing ",
        &[
            ("product", "; impl run; };"),
            ("thunk", "; impl run; };"),
            ("sequence", "; impl run; };"),
        ],
    );
    add("", "module app; elab make: . -> . { trailing product ", &[]);
    add("", "module app; op ~ _ { ", &[("impl", " run; };")]);
    add("", "module app; op ~ _ { impl run; ", &[]);
    add(
        "",
        "module app; varop [* *] { ",
        &[
            ("foldl", " step base; };"),
            ("foldl1", " step seed; };"),
            ("foldr", " step base; };"),
            ("foldr1", " step seed; };"),
            ("finalize", " finish; foldr step base; };"),
        ],
    );
    add(
        "",
        "module app; varop [* *] { finalize finish; ",
        &[
            ("foldl", " step base; };"),
            ("foldl1", " step seed; };"),
            ("foldr", " step base; };"),
            ("foldr1", " step seed; };"),
        ],
    );
    for mode in ["foldl", "foldl1", "foldr", "foldr1"] {
        add(
            "",
            &format!("module app; varop [* *] {{ {mode} step base; "),
            &[("finalize", " finish; };")],
        );
        add(
            "",
            &format!("module app; varop [* *] {{ {mode} step base; finalize finish; "),
            &[],
        );
    }
    add(
        "",
        "module app; elab choose : (. & .) -> . { trailing product; trailing product otherwise; impl implementation } fn run() { choose! { () } ",
        &[("otherwise", " { () } }")],
    );
    add(
        "",
        "module app; labels { field: . }; fn run(value: Field) { let .({field ",
        &[("as", " local}) = value; local }")],
    );
    add("", "module app; fn run() { recur(", &[]);
    add(
        "",
        "module app; rec(loop) fn run(x: .) { recur(",
        &[("cont", ") run(x) }")],
    );
    add(
        "",
        "module app; rec(loop) fn run[A](x: A) { recur(",
        &[("cont", ") run(x) }"), ("poly", ") run(x) }")],
    );
    add(
        "",
        "module app; rec(loop) fn run[A](x: A) { recur(cont, ",
        &[("poly", ") run(x) }")],
    );
    add(
        "",
        "module app; rec(loop) fn run[A](x: A) { recur(poly, ",
        &[("cont", ") run(x) }")],
    );
    add(
        "",
        "module app; rec(loop) fn run[A](x: A) { recur(poly, cont, ",
        &[],
    );
    add(
        "",
        "module app; rec(loop) fn run[A](x: A) { .(y: A) { recur(",
        &[("escape", ") run(x) } }"), ("poly", ") run(x) } }")],
    );

    // Package and backend key contracts. Values are ordinary valid field values;
    // each target cohort has its own independently spelled expected vocabulary.
    add(".pkg", "", &[("package", " app;")]);
    add(
        ".pkg",
        "package app; ",
        &[("bridge", " { app; }"), ("build", " { cache (); }")],
    );
    add(".pkg", "package app; build {} ", &[("bridge", " { app; }")]);
    add(
        ".pkg",
        "package app; bridge { app; } ",
        &[("build", " { cache (); }")],
    );
    add(
        ".pkg",
        "package app; build { ",
        &[
            ("cache", " (); }"),
            ("docs", " { md \"docs\"; } }"),
            ("target", " rust {} }"),
        ],
    );
    add(
        ".pkg",
        "package app; build { cache (); ",
        &[("docs", " { md \"docs\"; } }"), ("target", " rust {} }")],
    );
    add(
        ".pkg",
        "package app; build { docs { md \"docs\"; }; ",
        &[("cache", " (); }"), ("target", " rust {} }")],
    );
    add(
        ".pkg",
        "package app; build { target rust {}; ",
        &[
            ("cache", " (); }"),
            ("docs", " { md \"docs\"; } }"),
            ("target", " js {} }"),
        ],
    );
    add(
        ".pkg",
        "package app; build { target ",
        &[
            ("go", " {} }"),
            ("haskell", " {} }"),
            ("java", " {} }"),
            ("js", " {} }"),
            ("kio-prime", " {} }"),
            ("python", " {} }"),
            ("rust", " {} }"),
            ("swift", " {} }"),
            ("ts", " {} }"),
        ],
    );
    for target in [
        "go",
        "haskell",
        "java",
        "js",
        "kio-prime",
        "python",
        "rust",
        "swift",
        "ts",
    ] {
        let mut keys = vec![("out", " \"out\"; } }")];
        if target != "kio-prime" {
            let namespace = if matches!(target, "swift" | "haskell") {
                " \"App\"; } }"
            } else {
                " \"app\"; } }"
            };
            keys.push(("namespace", namespace));
        }
        if target == "rust" {
            keys.push(("thread_safety", " \"send\"; } }"));
        }
        add(
            ".pkg",
            &format!("package app; build {{ target {target} {{ "),
            &keys,
        );
        keys.retain(|(name, _)| *name != "out");
        add(
            ".pkg",
            &format!("package app; build {{ target {target} {{ out \"out\"; "),
            &keys,
        );
    }
    add(
        ".pkg",
        "package app; build { docs { ",
        &[
            ("md", " \"docs\"; } }"),
            ("support", " \"assets\"; md \"docs\"; } }"),
            ("md_out", " \"out\"; md \"docs\"; } }"),
            ("html", " \"html\"; md \"docs\"; } }"),
        ],
    );
    add(
        ".pkg",
        "package app; build { docs { md \"docs\"; ",
        &[
            ("support", " \"assets\"; } }"),
            ("md_out", " \"out\"; } }"),
            ("html", " \"html\"; } }"),
        ],
    );
    add(
        ".pkg",
        "package app; build { docs { md \"docs\"; md_out \"out\"; html \"html\"; support \"assets\"; ",
        &[("support", " \"images\"; } }")],
    );

    // Dependency source alternatives and once-only lock fields.
    add(
        ".dep",
        "",
        &[("dependency", " app; source { path \"dep.pkg.kio\"; }")],
    );
    add(
        ".dep",
        "dependency app; ",
        &[
            ("source", " { path \"dep.pkg.kio\"; }"),
            (
                "rehost",
                " dep/api to local/api; source { path \"dep.pkg.kio\"; }",
            ),
            (
                "retype",
                " dep/api.A to local/api.A; source { path \"dep.pkg.kio\"; }",
            ),
        ],
    );
    add(
        ".dep",
        "dependency app; source { ",
        &[
            ("path", " \"dep.pkg.kio\"; }"),
            ("git", " \"https://example.invalid/dep\"; ref \"main\"; }"),
            ("ref", " \"main\"; git \"https://example.invalid/dep\"; }"),
        ],
    );
    add(
        ".dep",
        "dependency app; source { path \"dep.pkg.kio\"; ",
        &[
            ("git", " \"https://example.invalid/dep\"; ref \"main\"; }"),
            ("ref", " \"main\"; git \"https://example.invalid/dep\"; }"),
        ],
    );
    add(
        ".dep",
        "dependency app; source { git \"https://example.invalid/dep\"; ",
        &[
            ("path", " \"dep.pkg.kio\"; ref \"main\"; }"),
            ("ref", " \"main\"; }"),
        ],
    );
    add(
        ".dep",
        "dependency app; source { ref \"main\"; ",
        &[
            ("git", " \"https://example.invalid/dep\"; }"),
            (
                "path",
                " \"dep.pkg.kio\"; git \"https://example.invalid/dep\"; }",
            ),
        ],
    );
    add(
        ".dep",
        "dependency app; source { git \"https://example.invalid/dep\"; ref \"main\"; ",
        &[("path", " \"dep.pkg.kio\"; }")],
    );
    add(
        ".dep",
        "dependency app; source { git \"https://example.invalid/dep\"; path \"dep.pkg.kio\"; ",
        &[("ref", " \"main\"; }")],
    );
    add(
        ".dep",
        "dependency app; source { ref \"main\"; path \"dep.pkg.kio\"; ",
        &[("git", " \"https://example.invalid/dep\"; }")],
    );
    add(
        ".dep",
        "dependency app; source { git \"https://example.invalid/dep\"; ref \"main\"; path \"dep.pkg.kio\"; ",
        &[],
    );
    add(
        ".dep",
        "dependency app; source { path \"dep.pkg.kio\"; } ",
        &[
            ("rehost", " dep/api to local/api;"),
            ("retype", " dep/api.A to local/api.A;"),
        ],
    );
    add(
        ".dep",
        "dependency app; source { path \"dep.pkg.kio\"; } rehost dep/api ",
        &[("to", " local/api;")],
    );
    add(
        ".dep",
        "dependency app; source { path \"dep.pkg.kio\"; } retype dep/api.A ",
        &[("to", " local/api.A;")],
    );
    let lock_tail =
        " { git \"https://example.invalid/dep\"; ref \"main\"; commit \"abc\"; sig \"digest\"; }";
    add(
        ".lock",
        "",
        &[("lock", &format!(" app; resolved{lock_tail}"))],
    );
    add(".lock", "lock app; ", &[("resolved", lock_tail)]);
    let lock_keys = ["git", "ref", "commit", "sig", "path"];
    for mask in 0..(1 << lock_keys.len()) {
        let mut prefix = "lock app; resolved { ".to_owned();
        for (index, key) in lock_keys.iter().enumerate() {
            if mask & (1 << index) != 0 {
                prefix.push_str(&format!("{key} \"value\"; "));
            }
        }
        let remaining_required: Vec<_> = lock_keys[..4]
            .iter()
            .enumerate()
            .filter_map(|(index, key)| (mask & (1 << index) == 0).then_some(*key))
            .collect();
        let mut tails: Vec<_> = remaining_required
            .iter()
            .map(|selected| {
                let mut tail = " \"value\"; ".to_owned();
                for key in &remaining_required {
                    if key != selected {
                        tail.push_str(&format!("{key} \"value\"; "));
                    }
                }
                tail.push('}');
                (*selected, tail)
            })
            .collect();
        if mask & (1 << 4) == 0 {
            let mut tail = " \"dep.pkg.kio\"; ".to_owned();
            for key in &remaining_required {
                tail.push_str(&format!("{key} \"value\"; "));
            }
            tail.push('}');
            tails.push(("path", tail));
        }
        add(
            ".lock",
            &prefix,
            &tails
                .iter()
                .map(|(key, tail)| (*key, tail.as_str()))
                .collect::<Vec<_>>(),
        );
    }

    // Signature changelog occupancy and its restricted declaration grammar.
    add(".sig", "", &[("signature", " app v(1);")]);
    add(".sig", "signature app ", &[("v", "(1);")]);
    add(
        ".sig",
        "signature app v(1); ",
        &[(
            "v",
            "(1) { nonbreaking { add { module api { pub fn run() -> .; } } } }",
        )],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { ",
        &[
            (
                "with",
                " { module api { pub type A = .; } }; nonbreaking { add { api.A; } } }",
            ),
            (
                "breaking",
                " { add { module api { pub fn run() -> .; } } } }",
            ),
            (
                "nonbreaking",
                " { add { module api { pub fn run() -> .; } } } }",
            ),
        ],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { with { ",
        &[(
            "module",
            " api { pub type A = .; } }; nonbreaking { add { api.A; } } }",
        )],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { with { module api { pub type A = .; } }; ",
        &[
            ("breaking", " { add { api.A; } } }"),
            ("nonbreaking", " { add { api.A; } } }"),
        ],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { breaking { add { module api { pub type A = .; } } }; ",
        &[
            ("with", " { module api { pub type A = .; } } }"),
            (
                "nonbreaking",
                " { add { module api { pub type B = .; } } } }",
            ),
        ],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { nonbreaking { add { module api { pub type A = .; } } }; ",
        &[
            ("with", " { module api { pub type A = .; } } }"),
            ("breaking", " { add { module api { pub type B = .; } } } }"),
        ],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { breaking { ",
        &[
            ("add", " { module api { pub fn run() -> .; } } } }"),
            ("modify", " { module api { pub fn run() -> .; } } } }"),
            ("remove", " { module api { run; } } } }"),
        ],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { breaking { add { module api { pub type A = .; } }; ",
        &[
            ("modify", " { module api { pub type B = .; } } } }"),
            ("remove", " { module api { run; } } } }"),
        ],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { breaking { modify { module api { pub type A = .; } }; ",
        &[
            ("add", " { module api { pub type B = .; } } } }"),
            ("remove", " { module api { run; } } } }"),
        ],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { breaking { remove { module api { run; } }; ",
        &[
            ("add", " { module api { pub type A = .; } } } }"),
            ("modify", " { module api { pub type B = .; } } } }"),
        ],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { breaking { add { ",
        &[("module", " api { pub fn run() -> .; } } } }")],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { breaking { modify { ",
        &[("module", " api { pub fn run() -> .; } } } }")],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { breaking { remove { ",
        &[("module", " api { run; } } } }")],
    );
    add(
        ".sig",
        "signature app v(1); v(1) { breaking { add { module api { ",
        &[
            ("host", " type A; } } } }"),
            ("import", " __intrinsics__; pub fn run() -> .; } } } }"),
            (
                "newtype",
                " A : . { constructor make; projector read; }; } } } }",
            ),
            ("pub", " fn run() -> .; } } } }"),
            ("pure", " pub fn run() -> .; } } } }"),
            ("type", " A = .; } } } }"),
        ],
    );
    // A signature operation's module path comes from the open FQN catalog. Its
    // `module` keyword and replacement range remain exact; only these heads
    // advertise an incomplete result.
    for prefix in [
        "signature app v(1); v(1) { breaking { add { ",
        "signature app v(1); v(1) { breaking { modify { ",
        "signature app v(1); v(1) { breaking { remove { ",
    ] {
        cases
            .iter_mut()
            .find(|case| case.kind == ".sig" && case.prefix == prefix)
            .expect("signature operation completion case")
            .incomplete = true;
    }
    cases
}

fn insertion_parses(kind: &str, source: &str) -> Result<(), String> {
    use kio_lang::pass::parser;
    let result = match kind {
        "" => parser::parse(source).map(|_| ()),
        ".pkg" => parser::parse_package_file(source, Some("app")).map(|_| ()),
        ".dep" => parser::parse_dependency_file(source, Some("app")).map(|_| ()),
        ".lock" => parser::parse_lock_file(source, Some("app")).map(|_| ()),
        ".sig" => parser::parse_signature_file(source, Some("app")).map(|_| ()),
        _ => unreachable!("test file kind"),
    };
    result.map_err(|error| format!("{error:?}"))
}

#[test]
fn lsp_protocol_closed_choices_insert_and_retract_across_real_edits() {
    let dir = TempDir::new("completion-closed-edits");
    dir.write("outer.pkg.kio", "package outer;\n");
    let mut uris = std::collections::BTreeMap::new();
    for kind in ["", ".pkg", ".dep", ".lock", ".sig"] {
        let uri = path_to_file_uri(&dir.write(&format!("workspace/app{kind}.kio"), ""));
        uris.insert(kind, uri);
    }
    let workspace = dir.path().join("workspace");
    let (mut lsp, stderr_rx) =
        LspProcess::spawn_with_env_and_captured_stderr(&[("KIO_DEBUG_TIMING", "lsp")]);
    lsp.initialize(&path_to_file_uri(&workspace));
    for kind in ["", ".pkg", ".dep", ".lock", ".sig"] {
        let uri = &uris[kind];
        lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": ""}}),
        );
    }
    let cases = closed_cases();
    let mut failures = Vec::new();
    // Reversing the same states restores choices retracted by earlier edits.
    for (index, case) in cases.iter().chain(cases.iter().rev()).enumerate() {
        let uri = &uris[case.kind];
        lsp.send_notification("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": index + 2}, "contentChanges": [{"text": case.prefix}]}));
        let (line, character) = source_position(&case.prefix, case.prefix.len());
        let result = send_completion(&mut lsp, uri, line, character);
        let mut actual = completion_labels(&result);
        actual.sort_unstable();
        let mut expected: Vec<_> = case.choices.iter().map(|(name, _)| name.as_str()).collect();
        expected.sort_unstable();
        if actual != expected || result["isIncomplete"] != case.incomplete {
            failures.push(format!(
                "{} {:?}: expected {expected:?}, actual {actual:?}, incomplete {}",
                case.kind, case.prefix, result["isIncomplete"]
            ));
        }
        if index >= cases.len() {
            continue;
        }
        for (label, tail) in &case.choices {
            let witness = format!("{}{label}{tail}", case.prefix);
            if let Err(error) = insertion_parses(case.kind, &witness) {
                failures.push(format!("expected grammar witness {witness:?}: {error}"));
            }
            let item = result["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["label"] == *label);
            let Some(item) = item else {
                continue;
            };
            let edit = &item["textEdit"];
            if !case.prefix.contains("recur(") {
                assert_eq!(edit["newText"], *label);
                assert_eq!(edit["range"]["start"]["line"], 0);
                assert_eq!(edit["range"]["end"]["line"], 0);
                let start = edit["range"]["start"]["character"].as_u64().unwrap() as usize;
                let end = edit["range"]["end"]["character"].as_u64().unwrap() as usize;
                let eof = case.prefix.encode_utf16().count();
                assert_eq!(
                    (start, end),
                    (eof, eof),
                    "{} {:?}: {label}",
                    case.kind,
                    case.prefix
                );
            }
            let mut inserted = apply_lsp_text_edits(&case.prefix, std::slice::from_ref(edit));
            inserted.push_str(tail);
            if let Err(error) = insertion_parses(case.kind, &inserted) {
                failures.push(format!("insertion {inserted:?}: {error}"));
            }
        }
    }
    assert_eq!(lsp.shutdown(), 0);
    super::completion_context::assert_scheduled_root(stderr_rx, &workspace);
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn lsp_recursive_annotations_replace_canonically_on_current_overlays() {
    let dir = TempDir::new("completion-recursive-annotations");
    dir.write_pkg_root_package();
    dir.write(
        "pkg.kio",
        "module pkg; host fn drive[S][R](step: ((S -> R) & S) -> (S | R), state: S) -> R; host fn retain(callback: . -> .) -> .;\n",
    );
    let path = dir.write("pkg/main.kio", "");
    let uri = path_to_file_uri(&path);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": ""}}),
    );
    let cases = [
        ("|", vec!["escape", "poly"], Some(("escape", "escape")), "x"),
        (
            "cont, |",
            vec!["escape", "poly"],
            Some(("escape", "escape")),
            "x",
        ),
        (
            "poly, cont, es|capex",
            vec!["escape"],
            Some(("escape", "poly, escape")),
            "A & A, (x, x)",
        ),
        (
            "escape, |",
            vec!["poly"],
            Some(("poly", "poly, escape")),
            "A & A, (x, x)",
        ),
        (
            "cont, // 😀 keep\n es|cape",
            vec!["escape", "poly"],
            Some(("escape", " // 😀 keep\n escape")),
            "x",
        ),
        (
            "cont,\n |",
            vec!["escape", "poly"],
            Some(("escape", "\n escape")),
            "x",
        ),
        (
            "|,\n escape",
            vec!["poly"],
            Some(("poly", "poly, escape\n ")),
            "A & A, (x, x)",
        ),
        (
            "|, escape",
            vec!["poly"],
            Some(("poly", "poly, escape")),
            "A & A, (x, x)",
        ),
        ("poly, escape, |", vec![], None, "A & A, (x, x)"),
        ("es|capex, unknown", vec![], None, "x"),
        ("es|capex, // keep\n unknown", vec![], None, "x"),
    ];
    let mut version = 1;
    for (annotations, expected, selected, args) in cases.iter().chain(cases.iter().rev()) {
        let marked = format!(
            "module pkg/main; import pkg as host; rec(host.drive) {{ fn again[A](x: A) -> . {{ recur again(x) }}; pub fn capture[A](x: A) -> . {{ host.retain(.() -> . {{ recur({annotations}) again({args}) }}) }} }}"
        );
        let cursor = marked.find('|').unwrap();
        let source = marked.replacen('|', "", 1);
        version += 1;
        send_full_text_change(&mut lsp, &uri, version, &source);
        let (line, character) = source_position(&source, cursor);
        let response = send_completion(&mut lsp, &uri, line, character);
        let mut actual = completion_labels(&response);
        actual.sort_unstable();
        assert_eq!(&actual, expected, "{source}: {response}");
        if let Some((label, canonical)) = selected {
            let item = response["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["label"] == *label)
                .unwrap();
            assert_eq!(
                item["textEdit"]["range"]["start"]["line"],
                item["textEdit"]["range"]["end"]["line"]
            );
            let mut edits = vec![item["textEdit"].clone()];
            if let Some(additional) = item["additionalTextEdits"].as_array() {
                edits.extend(additional.iter().cloned());
            }
            let applied = apply_lsp_text_edits(&source, &edits);
            assert_eq!(
                applied,
                marked.replace(annotations, canonical),
                "{response}"
            );
            // Clean analysis publishes only when retracting an earlier diagnostic.
            let sentinel = format!("{applied}\nfn broken() -> Missing {{ () }}");
            version += 1;
            send_full_text_change(&mut lsp, &uri, version, &sentinel);
            let rejected = wait_for_publish_version(&mut lsp, &uri, i64::from(version));
            assert!(!rejected["diagnostics"].as_array().unwrap().is_empty());
            version += 1;
            send_full_text_change(&mut lsp, &uri, version, &applied);
            let diagnostics = wait_for_publish_version(&mut lsp, &uri, i64::from(version));
            assert_eq!(
                diagnostics["diagnostics"],
                json!([]),
                "{applied}: {diagnostics}"
            );
        }
    }
    lsp.send_notification(
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri}}),
    );
    let source = "module pkg/main; fn ordinary() { recur(";
    lsp.send_notification("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "kio", "version": version + 1, "text": source}}));
    wait_for_publish_version(&mut lsp, &uri, i64::from(version + 1));
    let response = send_completion(&mut lsp, &uri, 0, source.len() as u64);
    assert!(completion_labels(&response).is_empty(), "{response}");
    assert_eq!(lsp.shutdown(), 0);
}
