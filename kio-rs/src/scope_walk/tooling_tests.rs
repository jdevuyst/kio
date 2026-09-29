use super::*;
use crate::ast::KioFileKind;
use crate::pass::parser::{CursorSlot, ToolingProbe, probe_tooling};

#[test]
fn placeholder_missing_brace_recovery_keeps_neighbor_scope_separate() {
    for body in ["()", ".x. { x1 }", ".x. { x1"] {
        let marked = format!(
            "module m; fn broken(prior: .) {{ {body} fn neighbor(current: .) {{ cur|rent }}"
        );
        let (source, offset) = probe(&marked, None);
        let probe = crate::pass::parser::probe_tooling_source(&source, Some(offset));
        assert!(probe.parse_error.is_some());
        let candidates = tooling::namespace_candidates(&probe, offset, Namespace::Value);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.label == "current"),
            "{marked}: {candidates:?}"
        );
        assert!(
            !candidates
                .iter()
                .any(|candidate| matches!(candidate.label.as_str(), "prior" | "x1")),
            "{marked}: {candidates:?}"
        );
    }
    let (source, offset) = probe(
        "module m; fn broken(prior: .) { .x. { x|1 fn neighbor(current: .) { current }",
        None,
    );
    let probe = crate::pass::parser::probe_tooling_source(&source, Some(offset));
    let candidates = tooling::namespace_candidates(&probe, offset, Namespace::Value);
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.label == "prior")
    );
    assert!(candidates.iter().any(|candidate| candidate.label == "x1"));
    assert!(
        !candidates
            .iter()
            .any(|candidate| candidate.label == "current")
    );
}

#[test]
fn placeholder_completion_uses_nearest_family_and_preserves_ordinary_shadowing() {
    for (marked, expected, absent) in [
        (
            "module m; fn run() { .x. { (x3, x|1) } }",
            vec!["x1", "x2", "x3"],
            vec![],
        ),
        (
            "module m; fn run() { .x. { (x1, .y. { y|2 }) } }",
            vec!["y1", "y2"],
            vec!["x1"],
        ),
        (
            "module m; fn run() { .x. { .(x1) { (x2, x|1) } } }",
            vec!["x1", "x2"],
            vec![],
        ),
    ] {
        let (source, offset) = probe(marked, None);
        let module = crate::pass::parser::parse(&source).unwrap();
        let probe = crate::pass::parser::probe_tooling_source(&source, Some(offset));
        for candidates in [
            in_scope_candidates(&module, offset),
            tooling::namespace_candidates(&probe, offset, Namespace::Value),
        ] {
            for name in &expected {
                assert!(
                    candidates.iter().any(|candidate| candidate.label == *name),
                    "{marked}: {candidates:?}"
                );
            }
            for name in &absent {
                assert!(
                    !candidates.iter().any(|candidate| candidate.label == *name),
                    "{marked}: {candidates:?}"
                );
            }
            if marked.contains(".(x1)") {
                let candidate = candidates
                    .iter()
                    .find(|candidate| candidate.label == "x1")
                    .unwrap();
                assert!(
                    matches!(candidate.origin, CandidateOrigin::Local(_)),
                    "inner authored binder wins"
                );
            }
        }
    }
    let (source, offset) = probe("module m; fn run() { .x. { (x1, .y. { y|", None);
    let probe = crate::pass::parser::probe_tooling_source(&source, Some(offset));
    let candidates = tooling::namespace_candidates(&probe, offset, Namespace::Value);
    assert!(
        candidates.iter().any(|candidate| candidate.label == "y1"),
        "{candidates:?}"
    );
    assert!(
        !candidates.iter().any(|candidate| candidate.label == "x1"),
        "{candidates:?}"
    );
}

#[derive(Debug, Default)]
struct Projection {
    slot: Option<CursorSlot>,
    replacement: Option<Span>,
    prefix: Option<Span>,
    keywords: Vec<&'static str>,
    values: Vec<Candidate>,
    types: Vec<Candidate>,
    needs_semantic_eligibility: bool,
}

fn project(probe: &ToolingProbe<'_>) -> Projection {
    let Some(cursor) = probe.facts.cursor.as_ref() else {
        return Projection::default();
    };
    if probe.facts.suppression.is_some() {
        return Projection::default();
    }
    let mut result = Projection {
        slot: Some(cursor.slot),
        replacement: Some(cursor.atom.replacement),
        prefix: Some(cursor.atom.prefix),
        keywords: cursor.keywords.clone(),
        ..Projection::default()
    };
    result.keywords = tooling_keywords(probe);
    for candidate in tooling_candidates(probe) {
        let type_shaped = matches!(
            candidate.kind,
            CandidateKind::Type | CandidateKind::TypeParameter
        );
        match cursor.slot {
            CursorSlot::Type => result.types.push(candidate),
            CursorSlot::Argument if type_shaped => result.types.push(candidate),
            CursorSlot::Argument if candidate.kind == CandidateKind::Module => {
                result.types.push(candidate.clone());
                result.values.push(candidate);
            }
            _ => result.values.push(candidate),
        }
    }
    result.needs_semantic_eligibility = matches!(
        cursor.slot,
        CursorSlot::Argument | CursorSlot::RecursiveCallee
    );
    result
}

fn probe(marked: &str, kind: Option<KioFileKind>) -> (String, u32) {
    assert_eq!(marked.matches('|').count(), 1);
    let offset = marked.find('|').unwrap() as u32;
    let source = marked.replace('|', "");
    assert!(offset <= source.len() as u32, "{kind:?}");
    (source, offset)
}

fn at(marked: &str) -> (String, u32) {
    probe(marked, Some(KioFileKind::Module))
}

fn names(entries: &[Candidate]) -> Vec<&str> {
    entries.iter().map(|entry| entry.label.as_str()).collect()
}

#[test]
fn neutral_block_completion_keeps_the_lexical_recursive_owner() {
    let mut failures = Vec::new();
    for (prefix, eligible) in [
        ("", true),
        ("sequence! bind { ", true),
        ("sequence! bind { let local = parent; ", true),
        ("sequence! bind { let local <- ", true),
        ("sequence! bind { let local <- parent; ", true),
        ("sequence! bind { parent; ", true),
        ("sequence! bind { parent; let local = parent; ", true),
        (
            "sequence! bind { let .(first, second) <- (parent, parent); ",
            true,
        ),
        ("scope! { let local = parent; ", true),
        ("scope! { parent; ", true),
        (
            "let prior = sequence! bind { let local <- parent; local }; ",
            true,
        ),
        (
            "sequence! bind { let local = sequence! bind { let nested <- parent; nested }; ",
            true,
        ),
        (
            "sequence! bind { let local <- sequence! bind { let nested <- parent; nested }; ",
            true,
        ),
        ("sequence! bind { .() { ", false),
    ] {
        for (tail, slot) in [
            ("", CursorSlot::Value),
            ("identity(", CursorSlot::Argument),
            ("rec(", CursorSlot::RecursiveAnnotation),
            ("rec ", CursorSlot::RecursiveCallee),
        ] {
            let source = format!(
                "module app; fn identity(value: .) {{ value }} rec(loop) fn run[A](parent: .) {{ {prefix}{tail}"
            );
            let probe = probe_tooling(
                &source,
                Some(KioFileKind::Module),
                Some(source.len() as u32),
                None,
            );
            let result = project(&probe);
            let matches_eligibility = match slot {
                CursorSlot::RecursiveAnnotation => ["cont", "poly"]
                    .iter()
                    .all(|word| result.keywords.contains(word) == eligible),
                CursorSlot::RecursiveCallee => names(&result.values).contains(&"run") == eligible,
                _ => result.keywords.contains(&"rec") == eligible,
            };
            if result.slot != Some(slot) || !matches_eligibility {
                failures.push(format!(
                    "{source}: expected {slot:?}, eligible={eligible}, got {result:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn neutral_block_completion_preserves_locals_and_preceding_positions() {
    for (marked, expected) in [
        (
            "module app; rec(loop) fn run(parent: .) { sequence! bind { let local <- pa|rent; local } }",
            vec!["parent"],
        ),
        (
            "module app; rec(loop) fn run(parent: .) { sequence! bind { let local <- parent; lo|cal } }",
            vec!["local", "parent"],
        ),
        (
            "module app; rec(loop) fn run(parent: .) { sequence! bind { pa|rent; parent } }",
            vec!["parent"],
        ),
        (
            "module app; rec(loop) fn run(parent: .) { let prior = sequence! bind { let local <- parent; local }; pa|rent }",
            vec!["prior", "parent"],
        ),
    ] {
        let (source, offset) = at(marked);
        let probe = probe_tooling(&source, Some(KioFileKind::Module), Some(offset), None);
        let result = project(&probe);
        assert_eq!(names(&result.values), expected, "{marked}");
        assert!(result.keywords.contains(&"rec"), "{marked}");
    }
}

#[test]
fn neutral_block_completion_distinguishes_insertions_from_closed_and_malformed_bodies() {
    for (body, recursive, eligible) in [
        ("sequence! bind { parent;| }", true, true),
        ("scope! { parent;| }", true, true),
        ("sequence! bind { parent; } |", true, false),
        ("sequence! bind { parent | }", true, false),
        ("sequence! bind { let local <- ; | }", true, false),
        (
            "sequence! bind { let local <- parent; sequence! bind { | } }",
            true,
            true,
        ),
        (
            "sequence!(|bind) { let local <- parent; local }",
            true,
            true,
        ),
        ("sequence! bind { let local = |parent; local }", true, true),
        ("sequence! bind { let local <- parent; | }", false, false),
        (
            "sequence! bind { let local <- |parent; local }",
            false,
            false,
        ),
    ] {
        let owner = if recursive { "rec(loop) fn" } else { "fn" };
        let marked = format!("module app; {owner} run(parent: .) {{ {body} }}");
        let (source, offset) = at(&marked);
        let probe = probe_tooling(&source, Some(KioFileKind::Module), Some(offset), None);
        let result = project(&probe);
        assert_eq!(
            result.keywords.contains(&"rec"),
            eligible,
            "{marked}: {result:?}"
        );
        if body.contains(";|") {
            assert_eq!(result.slot, Some(CursorSlot::Value), "{marked}");
            assert_eq!(
                result.replacement,
                Some(Span::new(offset, offset)),
                "{marked}"
            );
            assert!(names(&result.values).contains(&"parent"), "{marked}");
        }
        if body == "sequence! bind { parent | }" {
            assert!(result.values.is_empty(), "{marked}: {result:?}");
            assert!(!result.keywords.contains(&"let"), "{marked}: {result:?}");
        }
    }
    for (body, parses) in [
        ("sequence! bind { parent; parent }", true),
        ("sequence! bind { parent parent }", false),
    ] {
        let source = format!("module app; fn run(parent: .) {{ {body} }}");
        assert_eq!(
            crate::pass::parser::parse(&source).is_ok(),
            parses,
            "{body}"
        );
    }
}

#[test]
fn adapter_success_uses_exact_root_scope_and_local_identity() {
    let (source, offset) = at(
        "module app; import provider(imported, Imported); fn earlier() { () } fn run(parameter: .) { let local = parameter; lo|cal } fn later() { () }",
    );
    let probe = probe_tooling(&source, Some(KioFileKind::Module), Some(offset), None);
    assert!(probe.syntax.is_some() && probe.parse_error.is_none());
    let result = project(&probe);
    assert_eq!(result.slot, Some(CursorSlot::Value));
    assert_eq!(
        names(&result.values),
        ["local", "parameter", "earlier", "imported"]
    );
    let CandidateOrigin::Local(span) = result.values[0].origin else {
        panic!("{:?}", result.values);
    };
    assert_eq!(&source[span.start as usize..span.end as usize], "local");
    let atom = result.replacement.unwrap();
    let prefix = result.prefix.unwrap();
    assert_eq!(&source[atom.start as usize..atom.end as usize], "local");
    assert_eq!(&source[prefix.start as usize..prefix.end as usize], "lo");
    let applied = format!(
        "{}parameter{}",
        &source[..atom.start as usize],
        &source[atom.end as usize..]
    );
    crate::pass::parser::parse(&applied).expect("exact completion edit parses");
}

#[test]
fn adapter_failed_body_projects_patterns_and_shadow_order_once() {
    let (source, offset) = at(
        "module app; import provider(imported, Imported); fn earlier() { () } fn run((left: ., right: .)) { let left = right; let .(a: ., b: .) = (left, right); |",
    );
    let probe = probe_tooling(&source, Some(KioFileKind::Module), Some(offset), None);
    assert!(probe.syntax.is_none() && probe.parse_error.is_some());
    let result = project(&probe);
    assert_eq!(
        names(&result.values),
        ["b", "a", "left", "right", "earlier", "imported"]
    );
    assert!(
        result
            .values
            .iter()
            .all(|entry| !entry.label.starts_with("__"))
    );
    let left = result
        .values
        .iter()
        .find(|entry| entry.label == "left")
        .unwrap();
    assert_eq!(
        left.origin,
        CandidateOrigin::Local(Span::new(
            source.find("let left").unwrap() as u32 + 4,
            source.find("let left").unwrap() as u32 + 8,
        ))
    );
}

#[test]
fn adapter_failed_body_projects_row_let_and_excludes_unfinished_rhs_binder() {
    let (source, offset) = at(
        "module app; fn run(record: .) { let .({field as renamed}) = record; let unfinished = |",
    );
    let probe = probe_tooling(&source, Some(KioFileKind::Module), Some(offset), None);
    assert!(probe.syntax.is_none());
    assert_eq!(names(&project(&probe).values), ["renamed", "record"]);
}

#[test]
fn adapter_grammar_slots_do_not_admit_lexical_values_or_types() {
    for (marked, kind) in [
        (
            "module app; type Earlier = .; fn earlier() { () } |",
            KioFileKind::Module,
        ),
        ("package app; build { |", KioFileKind::Package),
        ("module app; newtype Box : . { |", KioFileKind::Module),
    ] {
        let (source, offset) = probe(marked, Some(kind));
        let probe = probe_tooling(&source, Some(kind), Some(offset), None);
        let result = project(&probe);
        assert!(
            matches!(
                result.slot,
                Some(CursorSlot::Grammar | CursorSlot::BuildField | CursorSlot::NewtypeMember)
            ),
            "{result:?}"
        );
        assert!(!result.keywords.is_empty(), "{result:?}");
        assert!(result.values.is_empty() && result.types.is_empty());
    }
}

#[test]
fn adapter_type_slot_preserves_namespace_and_written_type_parameter() {
    let (source, offset) =
        at("module app; type Earlier = .; fn before() { () } fn run[T](value: T|) -> T { value }");
    let probe = probe_tooling(&source, Some(KioFileKind::Module), Some(offset), None);
    let result = project(&probe);
    assert_eq!(result.slot, Some(CursorSlot::Type));
    assert_eq!(names(&result.types), ["T", "Earlier"]);
    assert!(result.values.is_empty());
}

#[test]
fn adapter_argument_keeps_namespaces_separate_until_semantic_eligibility() {
    let (source, offset) = at("module app; type Earlier = .; fn run[T](value: T) { f(   |");
    let probe = probe_tooling(&source, Some(KioFileKind::Module), Some(offset), None);
    let result = project(&probe);
    assert_eq!(result.slot, Some(CursorSlot::Argument));
    assert!(result.needs_semantic_eligibility);
    assert_eq!(names(&result.values), ["value"]);
    assert_eq!(names(&result.types), ["T", "Earlier"]);
}

#[test]
fn adapter_parser_suppression_prevents_edits_and_candidates() {
    for marked in [
        "module app; fn run(para|meter: .) { parameter }",
        "module app; fn run() { \"te|xt\" }",
        "module app; fn run() { // comm|ent\n () }",
    ] {
        let (source, offset) = at(marked);
        let probe = probe_tooling(&source, Some(KioFileKind::Module), Some(offset), None);
        assert!(probe.facts.suppression.is_some());
        let result = project(&probe);
        assert!(result.slot.is_none() && result.replacement.is_none());
        assert!(result.values.is_empty() && result.types.is_empty() && result.keywords.is_empty());
    }
}

#[test]
fn adapter_success_delimiter_whitespace_retains_completed_locals() {
    let (source, offset) =
        at("module app; fn run(parameter: .) { let local = parameter;  |  local }");
    let probe = probe_tooling(&source, Some(KioFileKind::Module), Some(offset), None);
    assert!(probe.syntax.is_some() && probe.parse_error.is_none());
    assert_eq!(names(&project(&probe).values), ["local", "parameter"]);
}
