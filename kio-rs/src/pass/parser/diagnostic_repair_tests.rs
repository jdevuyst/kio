use super::{
    parse, parse_dependency_file, parse_lazy, parse_lock_file, parse_package_file,
    parse_signature_file,
};
use crate::ast::{Item, Type};
use crate::error::{Applicability, Error, Fix};
use crate::span::Span;

fn apply_fix(source: &str, fix: &Fix) -> String {
    let mut edits = fix.edits.iter().collect::<Vec<_>>();
    edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
    for pair in edits.windows(2) {
        assert!(pair[0].span.end <= pair[1].span.start, "overlapping edits");
    }
    let mut repaired = source.to_owned();
    for edit in edits.into_iter().rev() {
        assert!(edit.file.is_none());
        assert!(edit.replacement_parts.is_empty());
        assert!(edit.required_whitespace.is_empty());
        repaired.replace_range(
            edit.span.start as usize..edit.span.end as usize,
            &edit.replacement,
        );
    }
    repaired
}

fn type_shape(ty: &Type) -> String {
    match ty {
        Type::Path { segments, args, .. } => {
            let name = segments
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if args.is_empty() {
                name
            } else {
                format!(
                    "{name}({})",
                    args.iter().map(type_shape).collect::<Vec<_>>().join(",")
                )
            }
        }
        Type::Unit { .. } => ".".to_owned(),
        Type::Bottom { .. } => "!".to_owned(),
        Type::Function { param, ret, .. } => {
            format!("Fn({},{})", type_shape(param), type_shape(ret))
        }
        Type::Product { left, right, .. } => {
            format!("Prod({},{})", type_shape(left), type_shape(right))
        }
        Type::Sum { left, right, .. } => format!("Sum({},{})", type_shape(left), type_shape(right)),
        Type::Forall { param, body, .. } => format!("All({},{})", param.name, type_shape(body)),
        other => panic!("unexpected type in the repair oracle: {other:?}"),
    }
}

fn alias_shape(source: &str) -> String {
    let module =
        parse(source).unwrap_or_else(|error| panic!("invalid repair {source:?}: {error:?}"));
    let Item::TypeAlias(alias) = &module.items[0] else {
        panic!("expected the repaired alias")
    };
    type_shape(alias.type_body())
}

fn assert_eager_lazy_error(source: &str) -> Error {
    let eager = parse(source).expect_err("the invalid spelling must remain rejected");
    let lazy = parse_lazy(source).expect_err("the header type must reject during lazy parsing");
    assert_eq!(eager.diagnostic(), lazy.diagnostic());
    assert_eq!(eager.exit_code(), crate::exit_code::ExitCode::Parse);
    eager
}

#[test]
fn identifier_word_repairs_are_role_correct_and_token_local() {
    for (source, role, invalid, valid) in [
        ("module main; fn a1b() -> . { () }", "value", "a1b", "a1_b"),
        (
            "module main; fn foo123bar() -> . { () }",
            "value",
            "foo123bar",
            "foo123_bar",
        ),
        (
            "module main; fn foo_123() -> . { () }",
            "value",
            "foo_123",
            "foo123",
        ),
        (
            "module main; fn foo__bar() -> . { () }",
            "value",
            "foo__bar",
            "foo_bar",
        ),
        (
            "module main; fn foo_Bar() -> . { () }",
            "value",
            "foo_Bar",
            "foo_bar",
        ),
        (
            "module main; type Foo_Bar = .;",
            "type",
            "Foo_Bar",
            "Foo_bar",
        ),
        ("module main; labels { _foo: . };", "label", "_foo", "foo"),
        ("module foo_123;", "module", "foo_123", "foo123"),
    ] {
        let error = parse(source).expect_err("invalid governed name");
        let diagnostic = error.diagnostic();
        assert!(
            diagnostic
                .message
                .starts_with(&format!("{role} name `{invalid}`"))
        );
        let fix = diagnostic
            .fixes()
            .iter()
            .find(|fix| fix.edits[0].replacement == valid)
            .expect("the compiler owns the role-correct proposal");
        assert_eq!(fix.title, format!("Replace this token with `{valid}`"));
        assert_eq!(fix.applicability, Applicability::MaybeIncorrect);
        assert_eq!(fix.edits.len(), 1);
        let start = source.find(invalid).unwrap() as u32;
        assert_eq!(
            fix.edits[0].span,
            Span::new(start, start + invalid.len() as u32)
        );
        parse(&apply_fix(source, fix)).expect("a spelling repair parses without changing its role");
    }
}

#[test]
fn all_header_file_kinds_use_the_governed_name_repairs() {
    for (kind, source, role) in [
        ("package", "package foo_123;", "package"),
        ("signature", "signature foo_123 v(1);", "package"),
        (
            "dependency",
            "dependency foo_123; source { path \"lib/lib.pkg.kio\"; }",
            "dependency",
        ),
        (
            "lock",
            "lock foo_123; resolved { git \"https://example.invalid/lib\"; ref \"main\"; commit \"revision\"; sig \"digest\"; }",
            "dependency",
        ),
    ] {
        let parse_header = |text: &str| match kind {
            "package" => parse_package_file(text, Some("foo123")).map(|_| ()),
            "signature" => parse_signature_file(text, Some("foo123")).map(|_| ()),
            "dependency" => parse_dependency_file(text, Some("foo123")).map(|_| ()),
            "lock" => parse_lock_file(text, Some("foo123")).map(|_| ()),
            _ => unreachable!(),
        };
        let error = parse_header(source).expect_err("invalid header name");
        let diagnostic = error.diagnostic();
        assert!(
            diagnostic
                .message
                .starts_with(&format!("{role} name `foo_123`")),
            "{kind}: {diagnostic:?}"
        );
        let fix = diagnostic.fixes().first().expect("header name correction");
        assert_eq!(fix.edits[0].replacement, "foo123");
        parse_header(&apply_fix(source, fix))
            .expect("repaired header agrees with its filename stem");
    }
}

#[test]
fn value_name_in_type_reference_reports_syntax_not_binding() {
    for source in [
        "module main; type Result = value;",
        "module main; fn value() -> . { () } type Result = value;",
        "module main; type Result = provider.value;",
        "module main; fn f(arg: value) -> . { () }",
        "module main; fn f() -> value { () }",
        "module main; fn f() -> . { let .(item: value) = (); () }",
        "module main; type Result = Box(value);",
    ] {
        let error = parse(source).expect_err("the value spelling is invalid in a type reference");
        let lazy_error = if source.contains("let .(item:") {
            parse_lazy(source)
                .expect("the body-local type reference must remain deferred")
                .force_all()
                .expect_err("body forcing must report the type-reference error")
        } else {
            parse_lazy(source)
                .and_then(|module| module.force_all())
                .expect_err("lazy parsing must retain the header type-reference error")
        };
        assert_eq!(error.diagnostic(), lazy_error.diagnostic());
        assert_eq!(error.exit_code(), lazy_error.exit_code());
        let diagnostic = error.diagnostic();
        assert_eq!(error.exit_code(), crate::exit_code::ExitCode::Parse);
        assert_eq!(
            diagnostic.message,
            "type name `value` must have an uppercase first letter"
        );
        let start = source.rfind("value").unwrap() as u32;
        assert_eq!(diagnostic.span, Span::new(start, start + 5));
        assert_eq!(diagnostic.notes().len(), 1);
        assert!(diagnostic.notes()[0].contains(crate::naming::TYPE_NAME_PATTERN));
        assert!(diagnostic.secondary().is_empty());
        assert_eq!(diagnostic.fixes().len(), 1);
        assert_eq!(
            diagnostic.fixes()[0].title,
            "Replace this token with `Value`"
        );
        assert_eq!(diagnostic.fixes()[0].edits[0].span, diagnostic.span);
        assert!(parse(&apply_fix(source, &diagnostic.fixes()[0])).is_ok());
        assert_eq!(diagnostic.unresolved_name(), None);
    }
}

#[test]
fn type_reference_wording_preserves_declaration_and_spelling_checks() {
    for source in [
        "module main; type value = .;",
        "module main; fn f[value]() -> . { () }",
        "module main; type Result = FooBar;",
    ] {
        let error = parse(source).expect_err("invalid declaration or type spelling");
        assert!(error.diagnostic().message.starts_with("type name `"));
        assert!(!error.diagnostic().fixes().is_empty());
    }
    parse("module main; type Value = .; type Result = Value;")
        .expect("ordinary type names remain accepted");
}

#[test]
fn module_header_extra_names_explain_the_path_without_reserving_keywords() {
    for name in [
        "package", "module", "fn", "type", "newtype", "labels", "elab", "op", "rec", "ordinary",
    ] {
        let source = format!("module {name} main;");
        let error = assert_eager_lazy_error(&source);
        let diagnostic = error.diagnostic();
        assert_eq!(
            diagnostic.message,
            "`module` must be followed by one module path, not adjacent names"
        );
        assert_eq!(diagnostic.span, Span::new(7, (7 + name.len() + 5) as u32));
        let help = diagnostic.help().expect("module-path guidance");
        for fact in [
            "module <path>;",
            "`/`",
            "package root",
            "`.kio`",
            "package-name prefix",
        ] {
            assert!(help.contains(fact), "{source:?}: {help:?} lacks {fact:?}");
        }
        assert!(
            diagnostic.fixes().is_empty(),
            "no filename-derived path may be guessed"
        );
        if name == "package" {
            assert!(
                diagnostic
                    .notes()
                    .iter()
                    .any(|note| note.contains("*.pkg.kio"))
            );
        }
    }
}

#[test]
fn package_header_in_a_module_explains_the_file_kind() {
    let error = assert_eager_lazy_error("package app;");
    assert_eq!(error.diagnostic().span, Span::new(0, 7));
    assert_eq!(
        error.diagnostic().message,
        "a package declaration is not a module header"
    );
    let help = error.diagnostic().help().expect("file-kind guidance");
    assert!(help.contains("*.pkg.kio"));
    assert!(help.contains("module <path>;"));
    assert!(error.diagnostic().fixes().is_empty());
    parse_package_file("package app;", Some("app")).expect("real package headers remain legal");
}

#[test]
fn legal_contextual_header_names_and_missing_semicolons_keep_their_grammar() {
    for name in [
        "package",
        "module",
        "fn",
        "type",
        "newtype",
        "labels",
        "elab",
        "op",
        "rec",
        "host",
        "use",
        "bridge",
        "equiv",
        "dependency",
        "signature",
        "lock",
    ] {
        for path in [name.to_owned(), format!("{name}/module/package")] {
            let source = format!("module {path}; fn package() -> . {{ let module = (); module }}");
            let eager = parse(&source).expect("contextual names remain identifiers");
            let lazy = parse_lazy(&source).expect("lazy header accepts contextual names");
            let forced = lazy
                .force_all()
                .expect("contextual function and let names stay legal");
            assert_eq!(eager.path, forced.path);
        }
    }
    for source in [
        "module main",
        "module main fn run() -> . { () }",
        "module package fn run() -> . { () }",
    ] {
        let error = assert_eager_lazy_error(source);
        assert_eq!(
            error.diagnostic().message,
            "expected `;` after the module header"
        );
    }
}

#[test]
fn foreign_file_headers_identify_their_owner_without_reserving_names() {
    for (header, keyword, extension) in [
        ("package app;", "package", "pkg"),
        ("dependency app;", "dependency", "dep"),
        ("lock app;", "lock", "lock"),
        ("signature app v(1);", "signature", "sig"),
    ] {
        for prefix in ["", "module main; "] {
            let source = format!("{prefix}{header}");
            let error = assert_eager_lazy_error(&source);
            let diagnostic = error.diagnostic();
            assert_eq!(
                diagnostic.span,
                Span::new(prefix.len() as u32, (prefix.len() + keyword.len()) as u32)
            );
            assert!(diagnostic.message.contains(keyword), "{diagnostic:?}");
            assert!(
                diagnostic
                    .help()
                    .unwrap()
                    .contains(&format!("*.{extension}.kio"))
            );
            assert!(diagnostic.fixes().is_empty());
        }
        let valid = format!("module {keyword}; fn {keyword}() -> . {{ () }}");
        parse(&valid).expect("the same spelling remains a legal module/function name");
        parse_lazy(&valid).unwrap().force_all().unwrap();
    }
    for source in ["dependency app", "lock app", "signature app v(1)"] {
        let error = assert_eager_lazy_error(source);
        assert_eq!(error.diagnostic().message, "expected `module`");
    }
}

#[test]
fn package_body_context_does_not_invent_an_optional_bridge() {
    for source in [
        "package app; module main;",
        "package app; build { target js { out \"out/js\"; } } module main;",
        "package app; bridge { main; } module main;",
    ] {
        let error = parse_package_file(source, Some("app")).unwrap_err();
        assert_eq!(
            error.diagnostic().message,
            "a package file contains a `package <name>;` header, an optional `build` block, and an optional `bridge` block"
        );
        let start = source.find("module main").unwrap();
        assert_eq!(
            error.diagnostic().span,
            Span::new(start as u32, (start + 6) as u32)
        );
    }
    parse_package_file("package app;", Some("app")).unwrap();
    parse_package_file("package app; bridge { main; }", Some("app")).unwrap();
}

fn assert_forced_parser_message(source: &str, message: &str) {
    let eager = parse(source).expect_err("the source must remain rejected");
    let lazy = parse_lazy(source)
        .and_then(|module| module.force_all())
        .unwrap_err();
    assert_eq!(eager.diagnostic(), lazy.diagnostic(), "{source}");
    assert_eq!(eager.diagnostic().message, message, "{source}");
    assert_eq!(eager.exit_code(), crate::exit_code::ExitCode::Parse);
    assert!(eager.diagnostic().fixes().is_empty());
}

#[test]
fn declaration_and_statement_terminators_name_their_actual_construct() {
    for (tail, context) in [
        ("import __intrinsics__", "import"),
        ("import __comptime__", "import"),
        ("import lib(thing)", "import"),
        ("import lib as l", "import"),
        ("host type Value", "host type declaration"),
        ("host fn value() -> .", "host function declaration"),
        ("type Value = .", "type alias"),
        ("literal value = 1", "literal declaration"),
        ("labels { value: . }", "labels declaration"),
        ("fn value() -> . { let local = () }", "let binding"),
        (
            "fn value() -> . { let .({field as local}) = row }",
            "row-let binding",
        ),
        (
            "fn value() -> . { let .(left, right) = pair }",
            "destructuring let binding",
        ),
        (
            "fn value() -> . { let .(<T> local) = packed }",
            "existential-opening let binding",
        ),
    ] {
        assert_forced_parser_message(
            &format!("module main; {tail}"),
            &format!("expected `;` after the {context}"),
        );
    }
    assert_forced_parser_message(
        "module main; fn value() -> . { do! bind { let local = () } }",
        "expected `;` after neutral let",
    );
    for tail in [
        "newtype Value : . { constructor mk; projector get; }",
        "elab value : . -> . { impl value; }",
        "op + __ { impl value; }",
        "varop [% %] { foldr push empty; }",
    ] {
        for suffix in ["", ";"] {
            let source = format!("module main; {tail}{suffix}");
            let eager = parse(&source).unwrap();
            let lazy = parse_lazy(&source).unwrap().force_all().unwrap();
            assert_eq!(eager, lazy, "{source}");
        }
    }
}

#[test]
fn file_header_and_outer_directive_terminators_keep_file_kind_context() {
    let cases: Vec<(Error, &str)> = vec![
        (parse_package_file("package app", Some("app")).unwrap_err(), "package header"),
        (parse_signature_file("signature app v(1)", Some("app")).unwrap_err(), "signature header"),
        (parse_dependency_file("dependency dep", Some("dep")).unwrap_err(), "dependency header"),
        (parse_lock_file("lock dep", Some("dep")).unwrap_err(), "lock header"),
        (parse_dependency_file("dependency dep; source { path \"lib/lib.pkg.kio\"; } rehost lib/host to app/host", Some("dep")).unwrap_err(), "rehost directive"),
        (parse_dependency_file("dependency dep; source { path \"lib/lib.pkg.kio\"; } retype lib/types to app/types", Some("dep")).unwrap_err(), "retype directive"),
    ];
    for (error, context) in cases {
        assert_eq!(
            error.diagnostic().message,
            format!("expected `;` after the {context}")
        );
        assert_eq!(error.exit_code(), crate::exit_code::ExitCode::Parse);
    }
}

#[test]
fn block_field_missing_peer_separators_offer_exact_reparsable_insertions() {
    use crate::ast::KioFileKind;

    let check = |kind, source: &str| -> Result<(), Error> {
        match kind {
            KioFileKind::Package => parse_package_file(source, None).map(|_| ()),
            KioFileKind::Dependency => parse_dependency_file(source, None).map(|_| ()),
            KioFileKind::Lock => parse_lock_file(source, None).map(|_| ()),
            _ => unreachable!("the table contains configuration file kinds"),
        }
    };
    for (kind, source, completed) in [
        (
            KioFileKind::Package,
            "package app; build { cache \"out/cache\" target js { out \"out/js\" } }",
            "cache \"out/cache\"",
        ),
        (
            KioFileKind::Package,
            "package app; build { docs { md \"out/docs\" support \"assets\" } }",
            "md \"out/docs\"",
        ),
        (
            KioFileKind::Package,
            "package app; build { target js { out \"out/js\" namespace \"app\" } }",
            "out \"out/js\"",
        ),
        (
            KioFileKind::Package,
            "package app; bridge { main other }",
            "main",
        ),
        (
            KioFileKind::Dependency,
            "dependency dep; source { git \"url\" ref \"main\" }",
            "git \"url\"",
        ),
        (
            KioFileKind::Lock,
            "lock dep; resolved { git \"url\" ref \"main\"; commit \"abc\"; sig \"digest\" }",
            "git \"url\"",
        ),
    ] {
        let error = check(kind, source).unwrap_err();
        assert_eq!(error.exit_code(), crate::exit_code::ExitCode::Parse);
        assert_eq!(
            error.diagnostic().message,
            "expected `;` between block entries"
        );
        let [fix] = error.diagnostic().fixes() else {
            panic!("missing unique separator fix: {error:?}")
        };
        assert_eq!(fix.applicability, Applicability::MachineApplicable);
        let [edit] = fix.edits.as_slice() else {
            panic!("separator fix must contain one insertion")
        };
        let offset = (source.find(completed).unwrap() + completed.len()) as u32;
        assert_eq!(edit.span, Span::new(offset, offset));
        assert_eq!(edit.replacement, ";");
        let repaired = apply_fix(source, fix);
        check(kind, &repaired).unwrap_or_else(|error| panic!("{repaired}: {error:?}"));
    }
}

#[test]
fn malformed_declaration_headers_explain_the_owned_form() {
    for (tail, message) in [
        (
            "type Value Other = .;",
            "expected `=` before the type alias body",
        ),
        (
            "newtype Value Other : . { constructor mk; projector get; };",
            "expected `:` before the newtype payload type",
        ),
        (
            "labels Value Other = { field: . };",
            "expected `=` before the named labels declaration body",
        ),
        (
            "fn value() other;",
            "expected `{` to open the function body, after an optional `->` return type",
        ),
    ] {
        assert_forced_parser_message(&format!("module main; {tail}"), message);
    }
    let error = assert_eager_lazy_error("module main; host fn value() -> . { () }");
    assert_eq!(
        error.diagnostic().message,
        "a `host fn` declares a function supplied by the host, not a Kio function body"
    );
    let help = error.diagnostic().help().unwrap();
    assert!(help.contains("`;`"));
    assert!(help.contains("ordinary `fn`"));
    parse("module main; host fn value() -> .;").unwrap();
    parse("module main; fn value() -> . { () }").unwrap();
}

#[test]
fn unit_type_repairs_preserve_complete_spans_and_token_boundaries() {
    for (source, repaired, shape) in [
        ("module m; type U=();", "module m; type U= . ;", "."),
        ("module m; type U = ();", "module m; type U =  . ;", "."),
        ("module m; type U=(,,);", "module m; type U= . ;", "."),
        ("module m; type U=( \n );", "module m; type U= . ;", "."),
        (
            "module m; type T=F(());",
            "module m; type T=F( . );",
            "F(.)",
        ),
        (
            "module m; type T=()->A;",
            "module m; type T= . ->A;",
            "Fn(.,A)",
        ),
        (
            "module m; type T=A & ();",
            "module m; type T=A &  . ;",
            "Prod(A,.)",
        ),
    ] {
        let error = assert_eager_lazy_error(source);
        let diagnostic = error.diagnostic();
        assert_eq!(
            diagnostic.message,
            "`()` is unit value syntax; write `.` for the unit type"
        );
        assert_eq!(
            diagnostic.help(),
            Some("replace this unit value spelling with the unit type `.`")
        );
        let [fix] = diagnostic.fixes() else {
            panic!("missing unique Unit fix: {error:?}")
        };
        assert_eq!(fix.title, "Use `.` for the unit type");
        assert_eq!(fix.applicability, Applicability::MachineApplicable);
        assert_eq!(fix.edits.len(), 1);
        assert_eq!(fix.edits[0].span, diagnostic.span);
        assert_eq!(fix.edits[0].replacement, " . ");
        assert_eq!(apply_fix(source, fix), repaired);
        assert_eq!(alias_shape(repaired), shape);
    }

    let source = "module m; fn run(x:())->. { x }";
    let error = assert_eager_lazy_error(source);
    let [fix] = error.diagnostic().fixes() else {
        panic!("missing colon-site Unit fix")
    };
    let repaired = apply_fix(source, fix);
    assert_eq!(repaired, "module m; fn run(x: . )->. { x }");
    let module = parse(&repaired).expect("compact colon repair parses");
    let Item::FnDef(function) = &module.items[0] else {
        panic!("function")
    };
    assert_eq!(
        type_shape(
            function
                .sig
                .value_params()
                .next()
                .unwrap()
                .ty
                .as_ref()
                .unwrap()
        ),
        "."
    );
}

#[test]
fn commented_unit_type_repairs_preserve_every_comment_and_existing_space() {
    for (group, replacement) in [
        ("(// keep unit\n)", " . // keep unit\n"),
        ("(, // keep unit\n,)", " .  // keep unit\n"),
        ("(// first\n, // second\n)", " . // first\n // second\n"),
    ] {
        let source = format!("module m; type U={group};");
        let error = assert_eager_lazy_error(&source);
        let [fix] = error.diagnostic().fixes() else {
            panic!("missing comment-safe Unit fix")
        };
        assert_eq!(fix.applicability, Applicability::MachineApplicable);
        for edit in &fix.edits {
            let original = &source[edit.span.start as usize..edit.span.end as usize];
            assert!(
                matches!(original, "(" | ")" | ","),
                "trivia erased: {original:?}"
            );
        }
        let repaired = apply_fix(&source, fix);
        assert_eq!(repaired, format!("module m; type U={replacement};"));
        assert_eq!(alias_shape(&repaired), ".");
    }
}

#[test]
fn product_type_repairs_preserve_components_nested_commas_and_operator_tokens() {
    for (group, replacement, shape) in [
        ("(A,B)", "(A & B)", "Prod(A,B)"),
        ("(A,)", "(A & )", "A"),
        ("(,,A,,)", "( &  & A &  & )", "A"),
        (
            "(,,A,,B,,C,,)",
            "( &  & A &  & B &  & C &  & )",
            "Prod(A,Prod(B,C))",
        ),
        ("(A,!)", "(A & !)", "Prod(A,!)"),
        ("(!,A)", "(! & A)", "Prod(!,A)"),
        ("(.,!)", "(. & !)", "Prod(.,!)"),
        ("(A -> B,C)", "((A -> B) & C)", "Prod(Fn(A,B),C)"),
        ("(A,B | C)", "(A & (B | C))", "Prod(A,Sum(B,C))"),
        ("(A & B,C)", "((A & B) & C)", "Prod(Prod(A,B),C)"),
        ("(F(A,B),C)", "(F(A,B) & C)", "Prod(F(A,B),C)"),
        ("([A,B] A,C)", "(([A,B] A) & C)", "Prod(All(A,All(B,A)),C)"),
        ("((A)->B,C)", "(((A)->B) & C)", "Prod(Fn(A,B),C)"),
        ("(((A & B)),C)", "((((A & B))) & C)", "Prod(Prod(A,B),C)"),
        (
            "(A, // keep separator\nF(B,C))",
            "(A &  // keep separator\nF(B,C))",
            "Prod(A,F(B,C))",
        ),
        (
            "(A -> B // keep component\n,C)",
            "((A -> B // keep component\n) & C)",
            "Prod(Fn(A,B),C)",
        ),
    ] {
        let source = format!("module m; type T={group};");
        let error = assert_eager_lazy_error(&source);
        let diagnostic = error.diagnostic();
        assert_eq!(
            diagnostic.message,
            "commas form tuple values, not product types; write `A & B`"
        );
        assert_eq!(
            &source[diagnostic.span.start as usize..diagnostic.span.end as usize],
            ","
        );
        assert_eq!(
            diagnostic.help(),
            Some("replace tuple separators with `&`; keep each component's grouping")
        );
        let [fix] = diagnostic.fixes() else {
            panic!("missing product fix: {error:?}")
        };
        assert_eq!(fix.title, "Use `&` for the product type");
        assert_eq!(fix.applicability, Applicability::MachineApplicable);
        let repaired = apply_fix(&source, fix);
        assert_eq!(repaired, format!("module m; type T={replacement};"));
        assert_eq!(
            alias_shape(&repaired),
            shape,
            "wrong component grouping for {group}"
        );
    }
}

#[test]
fn collapsed_chain_components_keep_their_written_grouping() {
    for (group, replacement, shape) in [
        ("(| A,B)", "((| A) & B)", "Prod(A,B)"),
        ("(& A,B)", "((& A) & B)", "Prod(A,B)"),
        ("(A,B |)", "(A & (B |))", "Prod(A,B)"),
        ("(A,B &)", "(A & (B &))", "Prod(A,B)"),
        ("(A,|)", "(A & (|))", "Prod(A,!)"),
        ("(&,A)", "((&) & A)", "Prod(.,A)"),
        ("(|,A)", "((|) & A)", "Prod(!,A)"),
        ("(A,| |)", "(A & (| |))", "Prod(A,!)"),
        ("(A,& .)", "(A & (& .))", "Prod(A,.)"),
    ] {
        let source = format!("module m; type T={group};");
        let error = assert_eager_lazy_error(&source);
        let [fix] = error.diagnostic().fixes() else {
            panic!("missing complete chain fix for {group}: {error:?}")
        };
        let repaired = apply_fix(&source, fix);
        assert_eq!(repaired, format!("module m; type T={replacement};"));
        assert_eq!(alias_shape(&repaired), shape);
    }
}

#[test]
fn malformed_type_components_never_offer_a_partial_product_fix() {
    for source in [
        "module m; type U=(",
        "module m; type U=(,",
        "module m; type T=(A,B ->);",
        "module m; type T=(A,B C);",
        "module m; type T=(A,B | C & D);",
        "module m; type T=(A,B;",
        "module m; type T=(A,(B,C));",
    ] {
        let error = parse(source).expect_err("invalid component");
        assert!(
            error.diagnostic().fixes().is_empty(),
            "unsafe partial fix for {source:?}: {error:?}"
        );
    }
}

#[test]
fn correct_unit_values_tuple_values_and_type_groups_remain_accepted() {
    for source in [
        "module m; fn run() -> . { () }",
        "module m; fn run() -> . & . { ((), ()) }",
        "module m; type T = .;",
        "module m; type T = (&);",
        "module m; type T = (|);",
        "module m; type T = (A & B);",
        "module m; type T = (A | B);",
        "module m; type T = F(A,B);",
        "module m; type T = [A,B] A;",
    ] {
        parse(source).unwrap_or_else(|error| panic!("legal syntax changed: {source:?}: {error:?}"));
        parse_lazy(source)
            .expect("valid lazy source")
            .force_all()
            .expect("valid body");
    }
}
