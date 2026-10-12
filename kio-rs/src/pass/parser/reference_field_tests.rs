use super::{parse, parse_lazy, probe_tooling};
use crate::ast::{ElaboratorSchedule, Item, KioFileKind};
use crate::pretty::pretty_module;

fn canonical(source: &str) -> String {
    let eager = parse(source).unwrap_or_else(|error| panic!("{source}: {error:?}"));
    assert_eq!(parse_lazy(source).unwrap().force_all().unwrap(), eager);
    let tooling = probe_tooling(source, Some(KioFileKind::Module), None, None);
    assert!(tooling.parse_error.is_none(), "{source}");
    let formatted = pretty_module(&eager);
    assert_eq!(pretty_module(&parse(&formatted).unwrap()), formatted);
    formatted
}

#[test]
fn grouped_reference_contexts_share_canonical_paths() {
    for (grouped, bare) in [
        (
            "op _ + _ { impl ((helpers.add)) }",
            "op _ + _ { impl helpers.add }",
        ),
        (
            "elab inspect : . -> . { impl ((helpers.inspect)) }",
            "elab inspect : . -> . { impl helpers.inspect }",
        ),
        (
            "elab inspect : . -> . { impl(fills) ((helpers.inspect)) }",
            "elab inspect : . -> . { impl(fills) helpers.inspect }",
        ),
        (
            "elab inspect : . -> . { captures ((source), ((Box))); impl helper }",
            "elab inspect : . -> . { captures (source, Box); impl helper }",
        ),
        (
            "rec(((loop))) fn f(x: .) -> . { x }",
            "rec(loop) fn f(x: .) -> . { x }",
        ),
        (
            "fn f(x: .) -> . { x.>((helpers.keep)) }",
            "fn f(x: .) -> . { x.>helpers.keep }",
        ),
        (
            "fn f(x: .) -> . { x.>>((Box.make))(()) }",
            "fn f(x: .) -> . { x.>>Box.make(()) }",
        ),
    ] {
        assert_eq!(
            canonical(&format!("module main; {grouped}")),
            canonical(&format!("module main; {bare}"))
        );
    }
    for mode in ["foldl", "foldr", "foldl1", "foldr1"] {
        assert_eq!(
            canonical(&format!(
                "module main; varop [[ ]] {{ {mode} ((helpers.step)) ((Box.seed)); finalize ((helpers.finish)) }}"
            )),
            canonical(&format!(
                "module main; varop [[ ]] {{ {mode} helpers.step Box.seed; finalize helpers.finish }}"
            )),
        );
    }
}

#[test]
fn grouped_fills_reference_and_schedule_are_distinct() {
    for (field, expected) in [
        ("impl fills", ElaboratorSchedule::Late),
        ("impl (fills)", ElaboratorSchedule::Late),
        ("impl ((fills))", ElaboratorSchedule::Late),
        ("impl(fills) fills", ElaboratorSchedule::Fills),
        ("impl(fills) (fills)", ElaboratorSchedule::Fills),
        ("impl(fills) ((fills))", ElaboratorSchedule::Fills),
        (
            "impl (// marker\nfills) // target\n(fills)",
            ElaboratorSchedule::Fills,
        ),
    ] {
        let source = format!("module main; elab inspect : . -> . {{ {field} }}");
        let module = parse(&source).unwrap();
        let Item::Elaborator(elaborator, _) = &module.items[0] else {
            panic!("elaborator")
        };
        assert_eq!(elaborator.schedule, expected, "{field}");
        canonical(&source);
    }
}

#[test]
fn optional_finalizer_unit_is_absence_but_still_present_for_duplicates() {
    let bare = canonical("module main; varop [[ ]] { foldl step base }");
    assert_eq!(bare, "module main;\n\nvarop [[ ]] { foldl step base }\n");
    for unit in ["()", "(())", "((()))"] {
        for body in [
            format!("foldl step base; finalize {unit}"),
            format!("finalize {unit}; foldl step base"),
        ] {
            assert_eq!(
                canonical(&format!("module main; varop [[ ]] {{ {body} }}")),
                "module main;\n\nvarop [[ ]] { foldl step base; finalize () }\n"
            );
        }
        for second in ["()", "finish"] {
            let source = format!(
                "module main; varop [[ ]] {{ foldl step base; finalize {unit}; finalize {second} }}"
            );
            assert!(
                parse(&source)
                    .unwrap_err()
                    .diag()
                    .1
                    .contains("duplicate `finalize`")
            );
        }
    }
}

#[test]
fn grouped_references_preserve_name_only_boundaries() {
    for target in [
        "()",
        "(())",
        "(helper())",
        "(.(x) { x })",
        "(helper, other)",
        "(Helper)",
    ] {
        for field in [
            format!("op _ + _ {{ impl {target} }}"),
            format!("elab inspect : . -> . {{ impl {target} }}"),
            format!("varop [[ ]] {{ foldl {target} base }}"),
            format!("varop [[ ]] {{ foldl step {target} }}"),
        ] {
            assert!(parse(&format!("module main; {field}")).is_err(), "{field}");
        }
    }
    for body in [
        "captures (); impl helper",
        "captures ((source())); impl helper",
    ] {
        assert!(parse(&format!("module main; elab inspect : . -> . {{ {body} }}")).is_err());
    }
    for source in [
        "module main; newtype Box : . { constructor (make); projector get }",
        "module main; newtype Box : . { constructor make; projector (get) }",
        "module main; fn f(x: .) { x.>(helper)! }",
        "module main; fn f(x: .) { x.>(helper()) }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
    for declaration in [
        "op _ + _ { impl ((provider/path.add)) }",
        "elab inspect : . -> . { impl ((provider/path.add)) }",
        "varop [[ ]] { foldl ((provider/path.add)) base }",
    ] {
        let error = parse(&format!("module main; {declaration}")).unwrap_err();
        assert!(
            error
                .diag()
                .1
                .contains("slash-qualified item paths are not lexical callable targets")
        );
    }
}

#[test]
fn grouped_reference_comments_survive_removed_tokens_once() {
    for declaration in [
        "op _ + _ { impl (// opener\n(// inner\nadd // leaf\n) // closer\n) }",
        "elab inspect : . -> . { captures ((// capture\nBox)); impl ((// implementation\nhelper)) }",
        "varop [[ ]] { foldl ((// step\nstep)) ((// base\nbase)); finalize ((// absent\n())) }",
        "rec((// capability\nloop // closing\n)) fn f(x: .) -> . { x }",
        "fn f(x: .) -> . { x.>((// callee\nhelper // closing\n)) }",
    ] {
        let source = format!("module main; {declaration}");
        let formatted = canonical(&source);
        for line in source.lines() {
            if let Some((_, comment)) = line.split_once("//") {
                assert_eq!(
                    formatted.matches(&format!("//{comment}\n")).count(),
                    1,
                    "{formatted}"
                );
            }
        }
    }
}

#[test]
fn unfinished_grouped_references_remain_tooling_input() {
    for tail in [
        "op _ + _ { impl ((",
        "elab inspect : . -> . { impl (fills) (",
        "elab inspect : . -> . { captures ((",
        "varop [[ ]] { foldl ((step)) ((",
        "rec((",
        "fn f(x: .) { x.>((",
    ] {
        let source = format!("module main; {tail}");
        let probe = probe_tooling(
            &source,
            Some(KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert!(probe.parse_error.is_some());
        assert!(probe.facts.cursor.is_some(), "{source}");
    }
}

#[test]
fn recovered_reference_closers_never_authorize_normal_input() {
    for declaration in [
        "op _ + _ { impl(fi }",
        "elab make: . { impl(fi }",
        "elab make: . { impl f; captures ((Type) }",
        "varop [[ ]] { foldl (step base }",
        "varop [[ ]] { foldl step base; finalize ( }",
        "varop [[ ]] { foldl step base; finalize (( }",
        "fn f(x: .) { x.>(fi }",
    ] {
        let source = format!("module main; {declaration}");
        assert!(parse(&source).is_err(), "accepted {source}");
        assert!(
            parse_lazy(&source)
                .and_then(|module| module.force_all())
                .is_err(),
            "lazy parsing accepted {source}"
        );
    }
    let source = "module main; elab make: . { impl(fi";
    let probe = probe_tooling(
        source,
        Some(KioFileKind::Module),
        Some(source.len() as u32),
        None,
    );
    assert!(probe.parse_error.is_none());
    assert!(probe.facts.recovered_group);
    assert!(probe.facts.cursor.is_some());
}
