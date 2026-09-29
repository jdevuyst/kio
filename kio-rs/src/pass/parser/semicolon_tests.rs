use super::{
    parse, parse_build_block_body, parse_dependency_file, parse_lazy, parse_lock_file,
    parse_package_file, parse_signature_file,
};

fn module(source: &str) -> crate::ast::Module {
    let eager = parse(source).unwrap_or_else(|error| panic!("{source}\n{error:?}"));
    let forced = parse_lazy(source).unwrap().force_all().unwrap();
    assert_eq!(
        crate::pretty::pretty_module(&eager),
        crate::pretty::pretty_module(&forced)
    );
    eager
}

#[test]
fn outer_braced_entries_accept_one_redundant_suffix() {
    for item in [
        "fn f() { () }",
        "newtype Box : . { constructor make; projector get }",
        "rec newtype List : . { constructor make; projector get }",
        "rec { type A = B; newtype B : . { constructor make; projector get } }",
        "rec(loop) fn f() { () }",
        "rec(loop) { fn f() { () }; fn g() { () } }",
        "equiv same { (); () }",
        "op _ + _ { impl add }",
        "varop [* *] { foldr1 step seed }",
        "elab apply : . -> . { impl run }",
    ] {
        for suffix in ["", ";"] {
            let source = format!("module sample; {item}{suffix}");
            let parsed = module(&source);
            assert_eq!(
                parsed.items[0].span().end as usize,
                "module sample; ".len() + item.len(),
                "{source}"
            );
            let printed = crate::pretty::pretty_module(&parsed);
            assert!(!printed.trim_end().ends_with(';'), "{printed}");
            module(&printed);
        }
        assert!(
            parse(&format!("module sample; {item};;")).is_err(),
            "{item}"
        );
    }
}

#[test]
fn strict_nested_edges_preserve_repeat_and_empty_policy() {
    for body in [
        "op _ + _ { ; impl add; }",
        "varop [* *] { ; foldr1 step seed; finalize finish; }",
        "elab apply : . -> . { ; captures (helper); impl run; }",
        "rec(loop) { ; fn f() { () }; fn g() { () }; }",
        "rec { ; type A = B; newtype B : . { constructor make; projector get }; }",
    ] {
        module(&format!("module sample; {body}"));
    }
    for body in [
        "op _ + _ { ;; impl add }",
        "op _ + _ { impl add;; }",
        "elab apply : . -> . { captures (helper) impl run }",
        "rec(loop) { fn f() { () } fn g() { () } }",
        "rec { type A = B;; type B = . }",
    ] {
        assert!(parse(&format!("module sample; {body}")).is_err(), "{body}");
    }
    assert!(parse_package_file("package sample; bridge {}", None).is_ok());
    for source in [
        "package sample; bridge {;}",
        "package sample; bridge {;; sample}",
        "package sample; bridge {sample;;}",
    ] {
        assert!(parse_package_file(source, None).is_err(), "{source}");
    }
    assert!(parse_package_file("package sample; bridge {; sample;}", None).is_ok());
}

#[test]
fn run_tolerant_fields_allow_edges_and_require_peer_separators() {
    module("module sample; newtype Box : . { ;; constructor make;; projector get;; }");
    assert!(
        parse_build_block_body(
            ";; cache ();; docs {;; md \"docs\";;}; target js {;; out \"out\";;};;"
        )
        .is_ok()
    );
    assert!(parse_build_block_body("cache () docs {md \"docs\"}").is_err());
    assert!(parse_build_block_body("target js {} target rust {}").is_err());
    assert!(
        parse_dependency_file(
            "dependency lib; source {;; git \"url\";; ref \"main\";;};",
            None
        )
        .is_ok()
    );
    assert!(
        parse_dependency_file("dependency lib; source {git \"url\" ref \"main\"}", None).is_err()
    );
    assert!(
        parse_lock_file(
            "lock lib; resolved {;; git \"url\"; ref \"main\"; commit \"abc\"; sig \"digest\";;};",
            None
        )
        .is_ok()
    );
}

#[test]
fn nonbraced_outer_entries_keep_terminators_and_commas() {
    for body in [
        "type A = .",
        "host type A",
        "host type A { owned }",
        "host fn f() -> .",
        "labels {flag: .}",
        "literal one = 1",
    ] {
        assert!(parse(&format!("module sample; {body}")).is_err(), "{body}");
    }
    assert!(
        parse("module sample; varop [* *] {foldr1 step seed} fn f() { [* (); () *] }").is_err()
    );
    assert!(parse("module sample; fn f() { let x = (); }").is_err());
}

#[test]
fn signature_nested_sections_and_mixed_entries_own_separators() {
    let source = "signature sample v(1); v(1) {; with {; module names {; import other as o; type A = .; newtype Box : A {constructor make; projector get}; };}; nonbreaking {; add {; names.A; module api {; pub fn get() -> .;};};};};";
    let parsed = parse_signature_file(source, None).unwrap();
    let printed = crate::sig::emit_signature_file(&parsed);
    let reparsed =
        parse_signature_file(&printed, None).unwrap_or_else(|error| panic!("{printed}\n{error:?}"));
    assert_eq!(printed, crate::sig::emit_signature_file(&reparsed));
    for invalid in [
        "signature sample v(1); v(1) {with {module names {type A = .}} nonbreaking {add {names.A}}}",
        "signature sample v(1); v(1) {nonbreaking {add {module one {} module two {}}}}",
        "signature sample v(1); v(1) {nonbreaking {add {names.A;;}}}",
    ] {
        assert!(parse_signature_file(invalid, None).is_err(), "{invalid}");
    }
}

#[test]
fn removed_separator_comments_survive_each_file_kind() {
    let sources = [
        (
            0,
            "module sample; fn f() { () } // note1\n; // note2\nfn g() { () } // note3\n; // note4\n",
        ),
        (
            1,
            "package sample; build {; target js {; out \"out\" // note1\n; // note2\n}; // note3\n} // note4\n;",
        ),
        (
            2,
            "dependency lib; source {; git \"url\"; // note1\nref \"main\" // note2\n; // note3\n} // note4\n;",
        ),
        (
            3,
            "lock lib; resolved {; sig \"digest\"; // note1\ngit \"url\"; ref \"main\"; commit \"abc\" // note2\n; // note3\n} // note4\n;",
        ),
        (
            4,
            "signature sample v(1); v(1) {; nonbreaking {; add {; module api {; type A = . // note1\n; // note2\n}; // note3\n};};} // note4\n;",
        ),
    ];
    fn format(kind: usize, source: &str) -> String {
        match kind {
            0 => crate::pretty::pretty_module(&module(source)),
            1 => crate::pretty::pretty_package_file(&parse_package_file(source, None).unwrap()),
            2 => {
                crate::pretty::pretty_dependency_file(&parse_dependency_file(source, None).unwrap())
            }
            3 => crate::pretty::pretty_lock_file(&parse_lock_file(source, None).unwrap()),
            4 => crate::sig::emit_signature_file(&parse_signature_file(source, None).unwrap()),
            _ => unreachable!(),
        }
    }
    for (kind, source) in sources {
        let printed = format(kind, source);
        for index in 1..=4 {
            assert_eq!(
                printed.matches(&format!("note{index}")).count(),
                1,
                "kind {kind}: {printed}"
            );
        }
        assert_eq!(printed, format(kind, &printed), "kind {kind}");
    }
}

#[test]
fn compact_declaration_outer_comments_have_one_owner() {
    for item in [
        "op _ + _ { impl add }",
        "varop [* *] { foldr1 step seed }",
        "elab apply : . -> . { impl run }",
    ] {
        for following in ["", "fn next() { () }"] {
            let source = format!("module sample; {item} // outside_marker\n; {following}");
            let printed = crate::pretty::pretty_module(&module(&source));
            assert_eq!(printed.matches("outside_marker").count(), 1, "{printed}");
            assert_eq!(printed, crate::pretty::pretty_module(&module(&printed)));
        }
    }
}

#[test]
fn signature_import_peer_diagnostic_inserts_owned_separator() {
    let source = "signature sample v(1); v(1) { nonbreaking { add { module api { import one as o import two as t; type A = . } } } }";
    let error = parse_signature_file(source, None).expect_err("missing import separator");
    let [fix] = error.diagnostic().fixes() else {
        panic!("one exact separator insertion: {error:?}")
    };
    let [edit] = fix.edits.as_slice() else {
        panic!("one edit")
    };
    assert_eq!(edit.replacement, ";");
    assert_eq!(edit.span.start, edit.span.end);
    assert!(source[..edit.span.start as usize].ends_with("as o"));
    let mut repaired = source.to_owned();
    repaired.insert_str(edit.span.start as usize, &edit.replacement);
    parse_signature_file(&repaired, None).expect("insertion repairs the recognized peer boundary");
}
