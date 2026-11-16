#![cfg(all(feature = "surface", feature = "cli"))]

use std::path::PathBuf;

use kio_lang::cmd::fmt::format_source;
use kio_lang::pass::{op_fold, parser};
use kio_lang::pretty::pretty_item_source;

const DECLARATIONS: &str = r#"
mod operators;
fn add(a: A, b: A) -> A { a }
fn index(a: A, b: A) -> A { a }
fn wrap(a: A) -> A { a }
fn step(a: A, b: A) -> A { a }
fn empty() -> A { value }
op _ + _ { impl add }
op _ $ ___ { impl add }
op ___ < _ > { impl index }
op | _ | { impl wrap }
op ! _ { impl wrap }
op _ ? { impl wrap }
op_var [] { foldr step empty }
op_var [* *] { foldr step empty }
op_var [** **] { foldr step empty }
"#;

fn source(expression: &str) -> String {
    format!("{DECLARATIONS}\nfn probe() {{ {expression} }}")
}

fn folded_body(source: &str) -> String {
    let module =
        parser::parse(source).unwrap_or_else(|error| panic!("parse failed: {error:?}\n{source}"));
    let mut modules = op_fold::fold_package(vec![(PathBuf::from("operators.kio"), module)])
        .unwrap_or_else(|error| panic!("operator lowering failed: {error:?}\n{source}"));
    let (_, module) = modules.pop().unwrap();
    pretty_item_source(module.items.last().expect("probe function"))
}

/// The fixture declares legal gaps; the parser does not decide which mutations
/// count. In particular, none of these gaps splits an operator word or `name!`.
fn variants(template: &str) -> Vec<String> {
    let pieces: Vec<_> = template.split('§').collect();
    assert!(pieces.len() > 1, "fixture must mark legal whitespace gaps");
    let mut variants = Vec::new();
    for separator in [" ", "\t", "\n", " \n\t "] {
        variants.push(pieces.join(separator));
        for gap in 0..pieces.len() - 1 {
            let mut text = pieces[0].to_owned();
            for (index, piece) in pieces[1..].iter().enumerate() {
                text.push_str(if index == gap { separator } else { " " });
                text.push_str(piece);
            }
            variants.push(text);
        }
    }
    variants
}

#[test]
fn separating_whitespace_preserves_declared_grouping() {
    for (template, expected) in [
        ("x§+§y§<§key§>", "(x + y) < key >"),
        ("x§?§?", "(x ?) ?"),
        ("|§x§|§(§y§)", "(| x |)(y)"),
        ("x§+§y.>f", "x + (y.>f)"),
        ("[§*§[§*§*§]§*§]", "[* [* *] *]"),
        ("[§*§(§|§x§|§)§*§]", "[* (| x |) *]"),
        ("[§*§-1§*§]", "[* -1 *]"),
        ("[§x§,§y§]", "[x, y]"),
    ] {
        let expected = folded_body(&source(expected));
        for expression in variants(template) {
            let input = source(&expression);
            assert_eq!(folded_body(&input), expected, "input: {expression}");
            let formatted = format_source("operators.kio", &input).unwrap();
            assert_eq!(folded_body(&formatted), expected, "formatted: {expression}");
            assert_eq!(
                format_source("operators.kio", &formatted).unwrap(),
                formatted,
                "formatter convergence: {expression}",
            );
        }
    }
}

#[test]
fn explicit_grouping_oracle_distinguishes_the_competing_tree() {
    assert_ne!(
        folded_body(&source("(x + y) < key >")),
        folded_body(&source("x + (y < key >)")),
    );
    assert_ne!(
        folded_body(&source("x + (y.>f)")),
        folded_body(&source("(x + y).>f")),
    );
}

#[test]
fn formatter_keeps_parentheses_that_bound_a_leading_chain_claim() {
    for expression in ["x + (y < key >)", "x $ (y < key >)", "(x $ y) < key >"] {
        let input = source(expression);
        let expected = folded_body(&input);
        let formatted = format_source("operators.kio", &input).unwrap();
        assert_eq!(
            folded_body(&formatted),
            expected,
            "{expression}\n{formatted}"
        );
        assert_eq!(
            format_source("operators.kio", &formatted).unwrap(),
            formatted
        );
    }
    assert!(parser::parse(&source("x $ y < key >")).is_err());
    assert_ne!(
        folded_body(&source("x $ (y < key >)")),
        folded_body(&source("(x $ y) < key >")),
    );
}

#[test]
fn separating_whitespace_does_not_make_invalid_nesting_valid() {
    for template in ["!§!§x", "x§+§!§y", "[§*§|§x§|§*§]"] {
        for expression in variants(template) {
            assert!(
                parser::parse(&source(&expression)).is_err(),
                "whitespace must not supply grouping: {expression}",
            );
        }
    }
}

#[test]
fn compact_frames_share_the_spaced_meaning_and_canonical_form() {
    for (compact, spaced) in [
        ("[**]", "[* *]"),
        ("[****]", "[** **]"),
        ("[*[**]*]", "[* [* *] *]"),
        ("[*x,y*]", "[* x, y *]"),
        ("[*(|x|)*]", "[* (| x |) *]"),
        ("[*(-1)*]", "[* -1 *]"),
        ("[]", "[]"),
    ] {
        assert_eq!(folded_body(&source(compact)), folded_body(&source(spaced)));
        assert_eq!(
            format_source("operators.kio", &source(compact)).unwrap(),
            format_source("operators.kio", &source(spaced)).unwrap(),
            "canonical convergence for {compact}",
        );
    }
}
