use super::{ToolingSyntax, parse, parse_lazy, probe_tooling_source};
use crate::ast::{Expr, Item, Module, OpChainKind, OpPart};
use crate::pretty::pretty_module;

fn source(pattern: &str, other: &str, expression: &str, imported: bool) -> String {
    let operators = if imported {
        format!("import ops(op {pattern}, op {other});")
    } else {
        format!(
            "fn pick(left: ., right: .) -> . {{ left }}\n\
             op {pattern} {{ impl pick; }};\n\
             op {other} {{ impl pick; }};"
        )
    };
    format!(
        "module source;\n{operators}\n\
         fn subject(a: ., b: ., c: .) -> . {{ {expression} }}"
    )
}

fn shape(expr: &Expr) -> String {
    match expr {
        Expr::Path { segments, .. } => segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join("."),
        Expr::OpChain {
            kind: OpChainKind::Normal { pattern, slots },
            ..
        } => {
            let tokens = pattern
                .iter()
                .filter_map(|part| match part {
                    OpPart::Token { content, .. } => Some(content.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let slots = slots.iter().map(shape).collect::<Vec<_>>().join(",");
            format!("{tokens}({slots})")
        }
        other => panic!("unexpected operand shape: {other:?}"),
    }
}

fn subject_shape(module: &Module) -> String {
    let Some(Item::FnDef(function)) = module.items.last() else {
        panic!("expected subject function");
    };
    shape(&function.body)
}

fn assert_modes(source: &str, expected: &str) {
    let eager = parse(source).unwrap_or_else(|error| panic!("{source}: {error:?}"));
    let lazy = parse_lazy(source).unwrap().force_all().unwrap();
    assert_eq!(eager, lazy, "{source}");
    let probe = probe_tooling_source(source, None);
    assert!(
        probe.parse_error.is_none(),
        "{source}: {:?}",
        probe.parse_error
    );
    let Some(ToolingSyntax::Module(probed)) = probe.syntax else {
        panic!("expected complete module");
    };
    assert_eq!(eager, probed, "{source}");
    assert_eq!(subject_shape(&eager), expected, "{source}");
    let formatted = pretty_module(&eager);
    let reparsed = parse(&formatted).unwrap();
    assert_eq!(subject_shape(&reparsed), expected, "{formatted}");
    assert_eq!(pretty_module(&reparsed), formatted);
}

#[test]
fn fixed_operand_leaves_its_owned_closing_word() {
    let mut failures = Vec::new();
    for imported in [false, true] {
        for (pattern, other, expression, expected) in [
            ("_ <| _ |>", "_ |> _", "a <| b |>", "<| |>(a,b)"),
            (
                "_ <| _ |>",
                "_ |> _",
                "a <| b <| c |> |>",
                "<| |>(a,<| |>(b,c))",
            ),
            ("_ ? _ !", "_ ! _", "a ? b !", "? !(a,b)"),
            (
                "_ <| _ |>++",
                "_ |>++ _",
                "a <| b <| c |>++ |>++",
                "<| |>++(a,<| |>++(b,c))",
            ),
            ("_ <| _ |>++", "_ |> _", "a <| b |>++", "<| |>++(a,b)"),
        ] {
            let source = source(pattern, other, expression, imported);
            let result = std::panic::catch_unwind(|| assert_modes(&source, expected));
            if result.is_err() {
                failures.push(source);
            }
        }
    }
    assert!(failures.is_empty(), "failed owned suffixes: {failures:#?}");
}

#[test]
fn atom_operand_rejects_open_chains_and_incomplete_closing_words() {
    for imported in [false, true] {
        for (pattern, other, expression) in [
            ("_ <| _ |>", "_ + _", "a <| b + c |>"),
            ("_ <| _ |>++", "_ |> _", "a <| b |> c"),
            ("_ <| _ |>++", "_ |> _", "a <| b |> --"),
            ("_ <| _ |>++", "_ |> _", "a <| b |>"),
            ("_ <| _ |>", "_ |> _", "a <| b"),
        ] {
            let source = source(pattern, other, expression, imported);
            assert!(parse(&source).is_err(), "{source}");
            assert!(
                parse_lazy(&source).unwrap().force_all().is_err(),
                "{source}"
            );
            assert!(
                probe_tooling_source(&source, None).parse_error.is_some(),
                "{source}"
            );
        }
    }
}

#[test]
fn closed_results_and_explicit_groups_compose_with_unbounded_recursion() {
    for imported in [false, true] {
        for (pattern, other, expression, expected) in [
            ("_ <| _ |>", "_ |> _", "a <| b |>", "<| |>(a,b)"),
            ("_ <| _ |>", "~ __", "(~ a) <| b |>", "<| |>(~(a),b)"),
            ("_ <| _ |>", "_ ~", "(a ~) <| b |>", "<| |>(~(a),b)"),
            ("_ <| _ |>", "_ ~", "a ~ <| b |>", "<| |>(~(a),b)"),
            ("_ <| _ |>", "_ |> _", "a <| b |> |> c", "|>(<| |>(a,b),c)"),
            ("_ ? __", "_ ! _", "a ? b ? c", "?(a,?(b,c))"),
        ] {
            assert_modes(&source(pattern, other, expression, imported), expected);
        }
        assert!(parse(&source("_ ? __", "_ ! _", "a ? b ! c", imported)).is_err());
    }
}

#[test]
fn owned_word_preempts_same_binding_continuation() {
    let declaration = "module source; op _ ? __ ? { impl pick; };";
    let error = parse(declaration).unwrap_err();
    assert!(error.diag().1.contains("ineffective"), "{error:?}");
    for imported in [false, true] {
        assert_modes(
            &source("_ ? ___ ?", "_ ! _", "a ? b ?", imported),
            "? ?(a,b)",
        );
        assert_modes(
            &source("_ ? ___ ?", "_ ! _", "a ? (b ? c ?) ?", imported),
            "? ?(a,? ?(b,c))",
        );
        assert!(parse(&source("_ ? ___ ?", "_ ! _", "a ? b ? c ?", imported)).is_err());
    }
}
