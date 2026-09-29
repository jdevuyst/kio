use super::*;
use crate::ast::{Expr, ImportItem, ImportKind, Item, OpChainKind, OperatorGrammar};

fn body(source: &str) -> Expr {
    let module = parse(&format!("module source; fn run() {{ {source} }}")).unwrap();
    let Item::FnDef(def) = module.items.into_iter().next().unwrap() else {
        panic!()
    };
    def.body
}

#[test]
fn delimiter_shape_is_independent_of_operator_scope() {
    for (open, close) in [
        ("[*", "*]"),
        ("[[", "]]"),
        ("[%", "%]"),
        ("+[<", "<]+"),
        ("/[", "]/"),
        ("..[", "].."),
        ("[.+.", ".+.]"),
        ("[[[[", "]]]]"),
    ] {
        let declaration = format!("module source; varop {open} {close} {{ foldr step seed; }};");
        let parsed = parse(&declaration).unwrap_or_else(|error| panic!("{declaration}: {error:?}"));
        let Item::VariadicOperator(operator, _) = &parsed.items[0] else {
            panic!()
        };
        let imported = parse(&format!(
            "module consumer; import source(varop {open} {close});"
        ))
        .unwrap();
        let ImportKind::Selective { items, .. } = &imported.imports[0].kind else {
            panic!()
        };
        let ImportItem::OperatorPattern { grammar, .. } = &items[0] else {
            panic!()
        };
        assert_eq!(
            grammar,
            &OperatorGrammar::variadic(&operator.open, &operator.spec)
        );
        assert!(
            matches!(body(&format!("{open} x, y {close}")), Expr::OpChain { kind: OpChainKind::Variadic { elements, .. }, .. } if elements.len() == 2)
        );
    }
}

#[test]
fn commas_separate_ordinary_expressions_without_phantom_elements() {
    for (source, count) in [
        ("[* *]", 0),
        ("[* ,,, *]", 0),
        ("[* , x,,, *]", 1),
        ("[* , x,, y,,, *]", 2),
    ] {
        assert!(
            matches!(body(source), Expr::OpChain { kind: OpChainKind::Variadic { elements, .. }, .. } if elements.len() == count),
            "{source}"
        );
    }
    for source in [
        "[* x y *]",
        "[* x; y *]",
        "[* x => y *]",
        "[* x ]",
        "[* x",
        "[[]]",
    ] {
        assert!(
            parse(&format!("module source; fn run() {{ {source} }}")).is_err(),
            "{source}"
        );
    }
    let source = "module source; import ops(op _ => _); fn run() { [* x => y, [% z %] *] }";
    let module = parse(source).unwrap();
    let Item::FnDef(def) = &module.items[0] else {
        panic!()
    };
    let Expr::OpChain {
        kind: OpChainKind::Variadic { elements, .. },
        ..
    } = &def.body
    else {
        panic!()
    };
    assert!(matches!(
        elements[0],
        Expr::OpChain {
            kind: OpChainKind::Normal { .. },
            ..
        }
    ));
    assert!(matches!(
        elements[1],
        Expr::OpChain {
            kind: OpChainKind::Variadic { .. },
            ..
        }
    ));
}

#[test]
fn contextual_control_names_remain_values_before_varop_close() {
    for source in ["[* if *]", "[* do *]", "[* [% if %], do *]"] {
        body(source);
    }
}

#[test]
fn recursive_type_group_retains_headers_after_invalid_payload() {
    let source = "module source; rec { type Alias[A] = ; newtype Later[B]: . { constructor mk; projector get; }; }";
    let offset = source.find("= ;").unwrap() as u32 + 2;
    let probe = probe_tooling(
        source,
        Some(crate::ast::KioFileKind::Module),
        Some(offset),
        None,
    );
    assert!(probe.parse_error.is_some());
    let names = probe
        .facts
        .scope_prefix
        .iter()
        .find_map(|prefix| match &prefix.syntax {
            super::ScopeSyntax::RecursiveTypes(names) => Some(names),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        names
            .iter()
            .map(|(name, _)| name.name.as_str())
            .collect::<Vec<_>>(),
        ["Alias", "Later"]
    );
    assert_eq!(
        names
            .iter()
            .map(|(_, params)| params.len())
            .collect::<Vec<_>>(),
        [1, 1]
    );
}

#[test]
fn tooling_source_selects_ordinary_roots_without_wrapping() {
    for source in [
        "fn run() { () }",
        "varop [* *] { foldl step seed; };",
        "import source(foo);",
    ] {
        assert!(
            matches!(
                probe_tooling_source(source, None).syntax,
                Some(ToolingSyntax::Declarations { .. })
            ),
            "{source}"
        );
    }
    for source in ["module()", "signature()", "fn()", "op(x)"] {
        assert!(
            matches!(
                probe_tooling_source(source, None).syntax,
                Some(ToolingSyntax::Expression(_))
            ),
            "{source}"
        );
    }
}

#[test]
fn declaration_and_import_share_strict_mirrored_heads() {
    for head in [
        "varop [ ]",
        "varop [* *>",
        "varop [* _ *]",
        "varop [**]",
        "varop [* , *]",
        "varop [* ; *]",
        "varop [* ... *]",
        "varop .[ ].",
        "varop [. .]",
        "varop [] []",
        "varop [ ! ! ]",
        "op variadic [ (_ ,) ... ]",
    ] {
        assert!(
            parse(&format!("module source; {head} {{ foldr step seed; }};")).is_err(),
            "{head}"
        );
        assert!(
            parse(&format!("module source; import provider({head});")).is_err(),
            "{head}"
        );
    }
    parse("module source; varop [* // head\n *] { foldl step seed; finalize done; }; fn varop(x: .) { x }").unwrap();
    parse("module source; varop [*// head\n*] { foldl seed seed; };").unwrap();
    parse("module source; import ops(varop [[/// head\n]]);").unwrap();
}

#[test]
fn brackets_are_excluded_from_fixed_operators_and_placeholder_stems() {
    for token in ["[", "]", "[*", "*]", "[]", "][", "[!]"] {
        for pattern in [
            format!("_ {token} _"),
            format!("_ ({token}) _"),
            format!("{token} _"),
        ] {
            assert!(
                parse(&format!("module source; op {pattern} {{ impl step; }};")).is_err(),
                "{pattern}"
            );
            assert!(
                parse(&format!("module source; import provider(op {pattern});")).is_err(),
                "{pattern}"
            );
        }
        assert!(
            parse(&format!("module source; fn run() {{ .{token}. {{ x1 }} }}")).is_err(),
            "{token}"
        );
    }
}

#[test]
fn ordinary_type_lambda_placeholder_and_value_elements_roundtrip() {
    for expr in [
        "[* f([*F] .), .[*F](x: F(.)) -> F(.) { x }, .x. { x1 }, (a, b), {field = x}, [% y %] *]",
        "[* , a, b, *]",
        "[* *]",
        "[* .x. { x1 } *]",
    ] {
        let source = format!("module source; fn run() {{ {expr} }}");
        let module = parse(&source).unwrap_or_else(|error| panic!("{source}: {error:?}"));
        let formatted = crate::pretty::pretty_module(&module);
        let reparsed = parse(&formatted).unwrap_or_else(|error| panic!("{formatted}: {error:?}"));
        assert_eq!(formatted, crate::pretty::pretty_module(&reparsed));
    }
}

#[test]
fn formatter_uses_tuple_layout_and_retains_comments() {
    let source = "module source; varop [* *] { foldr step seed; }; fn run() { [* // first\n , first, // last\n second, // tail\n *] }";
    let formatted = crate::pretty::pretty_module(&parse(source).unwrap());
    for comment in ["// first", "// last", "// tail"] {
        assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
    }
    assert_eq!(
        formatted,
        crate::pretty::pretty_module(&parse(&formatted).unwrap())
    );
    assert!(
        crate::pretty::pretty_module(&parse("module source; fn run() { [* ,,, *] }").unwrap())
            .contains("[* *]")
    );
    assert!(
        crate::pretty::pretty_module(&parse("module source; fn run() { [* , x,, *] }").unwrap())
            .contains("[* x *]")
    );
}

#[test]
fn formatter_retains_comments_in_empty_varop_region() {
    let source = "module source; fn run() { [* // empty\n ,,, *] }";
    let formatted = crate::pretty::pretty_module(&parse(source).unwrap());
    assert_eq!(formatted.matches("// empty").count(), 1, "{formatted}");
    assert_eq!(
        formatted,
        crate::pretty::pretty_module(&parse(&formatted).unwrap())
    );
}

#[test]
fn incomplete_cursor_facts_follow_argument_tuple_import_and_target_owners() {
    for (source, slot) in [
        ("identity(I", super::CursorSlot::Argument),
        ("(1, ", super::CursorSlot::Value),
    ] {
        let probe = probe_tooling(source, None, Some(source.len() as u32), None);
        assert_eq!(
            probe.facts.cursor.as_ref().map(|cursor| cursor.slot),
            Some(slot),
            "{source}: {:?}",
            probe.facts
        );
    }
    for (source, kind) in [
        (
            "module app/main; import provider/path(",
            super::ImportSelectionKind::Any,
        ),
        (
            "module app/main; import provider/path(first, sec",
            super::ImportSelectionKind::Name,
        ),
        (
            "module app/main; import provider/path(first, op _ +",
            super::ImportSelectionKind::FixedOperator,
        ),
        (
            "module app/main; import provider/path(first, varop [*",
            super::ImportSelectionKind::VariadicOperator,
        ),
    ] {
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        let cursor = probe
            .facts
            .cursor
            .as_ref()
            .unwrap_or_else(|| panic!("{source}: {:?}", probe.facts));
        assert_eq!(
            cursor.slot,
            super::CursorSlot::ImportSelection,
            "{source}: {:?}",
            probe.facts
        );
        let import = cursor.import.as_ref().unwrap();
        assert_eq!(import.kind, kind);
        assert_eq!(import.provider.segments.join("/"), "provider/path");
        assert_eq!(
            probe.facts.module_path.as_ref().unwrap().segments.join("/"),
            "app/main"
        );
    }
    let source = "package app; build { target kio-pr";
    let probe = probe_tooling(
        source,
        Some(crate::ast::KioFileKind::Package),
        Some(source.len() as u32),
        None,
    );
    let cursor = probe.facts.cursor.unwrap();
    assert_eq!(cursor.slot, super::CursorSlot::TargetId);
    assert_eq!(
        &source[cursor.atom.replacement.start as usize..cursor.atom.replacement.end as usize],
        "kio-pr"
    );
    let source = "package app; build { target rust { out \"out\"; name";
    let probe = probe_tooling(
        source,
        Some(crate::ast::KioFileKind::Package),
        Some(source.len() as u32),
        None,
    );
    let cursor = probe.facts.cursor.unwrap();
    assert_eq!(cursor.slot, super::CursorSlot::TargetField);
    let target = cursor.target.unwrap();
    assert_eq!(target.id, "rust");
    assert_eq!(target.fields, ["out"]);
}

#[test]
fn operator_cursor_owns_whole_written_leading_run() {
    let source = "module source; import ops(op _ && ++ _); fn run() { x && + }";
    let cursor = source.find("+ }").unwrap() as u32 + 1;
    let probe = probe_tooling(
        source,
        Some(crate::ast::KioFileKind::Module),
        Some(cursor),
        None,
    );
    let context = probe.facts.cursor.unwrap();
    assert_eq!(context.slot, super::CursorSlot::OperatorContinuation);
    assert_eq!(
        context.operator_prefix,
        Some(vec!["&&".to_owned(), "+".to_owned()])
    );
    assert_eq!(
        &source[context.atom.replacement.start as usize..context.atom.replacement.end as usize],
        "&& +"
    );
}

#[test]
fn import_comma_gaps_and_tag_atoms_keep_selection_context() {
    for source in [
        "module app; import ",
        "module app; import pro",
        "module app; import dir/pro",
    ] {
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert_eq!(
            probe.facts.cursor.as_ref().map(|context| context.slot),
            Some(super::CursorSlot::ImportProvider),
            "{source}: {:?}",
            probe.facts
        );
    }
    for source in [
        "module app/main; import syntax(\n  , ",
        "module app/main; import syntax(\n , // Leading trivia\n , ",
        "module app/main; import syntax(op",
        "module app/main; import syntax(op (_ ? ___), op",
    ] {
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        let cursor = probe
            .facts
            .cursor
            .as_ref()
            .unwrap_or_else(|| panic!("{source}: {:?}", probe.facts));
        assert_eq!(cursor.slot, super::CursorSlot::ImportSelection);
        assert_eq!(
            cursor.import.as_ref().unwrap().kind,
            super::ImportSelectionKind::Any
        );
    }
    let source = "module app/main; import syntax(op _ + _);";
    let cursor = source.find("op _").unwrap() as u32 + 2;
    let probe = probe_tooling(
        source,
        Some(crate::ast::KioFileKind::Module),
        Some(cursor),
        None,
    );
    let context = probe.facts.cursor.unwrap();
    assert_eq!(
        context.import.unwrap().kind,
        super::ImportSelectionKind::Any
    );
    assert_eq!(
        &source[context.atom.replacement.start as usize..context.atom.replacement.end as usize],
        "op _ + _"
    );
}

#[test]
fn recursive_completion_retains_group_polymorphism_and_lambda_boundaries() {
    for (source, expected, lambda) in [
        ("module app; fn run() { rec(", None, false),
        ("module app; rec(loop) fn run() { rec(", Some(false), false),
        (
            "module app; rec(loop) fn run[A]() { rec(",
            Some(true),
            false,
        ),
        (
            "module app; rec(loop) fn run[A]() { .() { rec(",
            Some(true),
            true,
        ),
        (
            "module app; rec(loop) fn run[A]() { .x. { rec(",
            Some(true),
            true,
        ),
    ] {
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert_eq!(
            probe.facts.cursor.as_ref().unwrap().slot,
            super::CursorSlot::RecursiveAnnotation
        );
        let polymorphic = probe
            .facts
            .scope_prefix
            .iter()
            .find_map(|prefix| match &prefix.syntax {
                super::ScopeSyntax::RecursiveMembers { polymorphic, .. } => Some(*polymorphic),
                _ => None,
            });
        assert_eq!(polymorphic, expected, "{source}");
        assert_eq!(
            probe.facts.regions.iter().any(|region| region.lambda),
            lambda,
            "{source}"
        );
    }
}

#[cfg(feature = "prime")]
#[test]
fn prime_rejects_varop_declarations_and_regions() {
    for source in [
        "module source; varop [* *] { foldr step seed; };",
        "module source; fn run() { [* *] }",
    ] {
        assert!(crate::prime::lower::lower_module(parse(source).unwrap()).is_err());
    }
}
