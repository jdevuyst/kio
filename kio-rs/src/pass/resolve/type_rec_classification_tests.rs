use super::*;
use crate::ast::{Meta, Prime, TypeRecGroup};

fn independent_group(count: usize) -> (String, TypeRecGroup<Prime>, TypeRecAnalysis) {
    let mut source = String::from("module x;\nrec {\n");
    for index in 0..count {
        source.push_str(&format!(
            "newtype Loop{index} : . | Loop{index} {{ constructor make{index}; projector read{index}; }};\n"
        ));
    }
    source.push_str("}\n");
    let module = crate::prime::lower::lower_module(
        crate::pass::parser::parse(&source).expect("parse independent group"),
    )
    .expect("lower ordinary type declarations directly to Prime");
    let Item::TypeRecGroup(group) = module.items.into_iter().next().unwrap() else {
        panic!("expected the complete written group");
    };
    let analysis = analyze_type_rec_members(&group.members);
    assert_eq!(analysis.components.len(), count);
    assert_eq!(analysis.cyclic_components, analysis.components);
    (source, group, analysis)
}

fn expected_partition(group: &TypeRecGroup<Prime>) -> Vec<Item<Prime>> {
    group
        .members
        .iter()
        .map(|member| {
            let TypeRecMember::Newtype(newtype) = member else {
                panic!("fixture consists only of self-recursive newtypes");
            };
            let mut newtype = newtype.clone();
            newtype.rec_span = Some(group.rec_span.unwrap());
            Item::Newtype(newtype)
        })
        .collect()
}

fn expected_fix(group: &TypeRecGroup<Prime>) -> Fix {
    let layout = group.source_layout.as_ref().unwrap();
    let spans = &layout.member_spans;
    let mut parts = Vec::new();
    for (index, span) in spans.iter().enumerate() {
        if index != 0 {
            parts.push(FixReplacementPart::Text("\n".to_owned()));
        }
        assert_eq!(span.start, layout.member_marker_offsets[index]);
        parts.push(FixReplacementPart::Text("rec ".to_owned()));
        parts.push(FixReplacementPart::Source(*span));
        parts.push(FixReplacementPart::Text(";".to_owned()));
    }
    let mut whitespace = vec![Span::new(
        group.open_brace_span.unwrap().end,
        spans[0].start,
    )];
    for pair in spans.windows(2) {
        whitespace.push(Span::new(pair[0].end, pair[0].end));
        whitespace.push(Span::new(pair[0].end + 1, pair[1].start));
    }
    whitespace.push(Span::new(
        spans.last().unwrap().end,
        spans.last().unwrap().end,
    ));
    whitespace.push(Span::new(
        spans.last().unwrap().end + 1,
        group.close_brace_span.unwrap().start,
    ));
    Fix::machine_applicable(
        "Fix recursive type groups",
        vec![FixEdit::from_parts(group.meta.span, parts, whitespace)],
    )
    .allowing_follow_on_reanalysis_outside(group.meta.span)
}

fn apply_fix(source: &str, fix: &Fix) -> String {
    let mut repaired = source.to_owned();
    let mut edits = fix.edits.iter().collect::<Vec<_>>();
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
    for edit in edits {
        for span in &edit.required_whitespace {
            assert!(
                source[span.start as usize..span.end as usize]
                    .chars()
                    .all(char::is_whitespace)
            );
        }
        let replacement = if edit.replacement_parts.is_empty() {
            edit.replacement.clone()
        } else {
            edit.replacement_parts
                .iter()
                .map(|part| match part {
                    FixReplacementPart::Text(text) => text.as_str(),
                    FixReplacementPart::Source(span) => {
                        &source[span.start as usize..span.end as usize]
                    }
                })
                .collect::<String>()
        };
        repaired.replace_range(
            edit.span.start as usize..edit.span.end as usize,
            &replacement,
        );
    }
    repaired
}

#[test]
fn recursive_group_repairs_reconstruct_parent_separators() {
    for body in [
        "type A = .",
        "; type A = .;",
        "newtype A : . { constructor make; projector get }; type B = A",
        "; newtype A : . | B { constructor make_a; projector get_a }; newtype B : . | A { constructor make_b; projector get_b }; type C = .;",
    ] {
        for suffix in ["", ";", " // outer comment\n;"] {
            let source = format!("module x; rec {{ {body} }}{suffix}");
            let module =
                crate::prime::lower::lower_module(crate::pass::parser::parse(&source).unwrap())
                    .unwrap();
            let Item::TypeRecGroup(group) = &module.items[0] else {
                panic!("group")
            };
            let fix = if group.members.len() == 1 {
                type_rec_unwrap_fix(group, false).expect("singleton source repair")
            } else {
                type_rec_partition_fix(group, &analyze_type_rec_members(&group.members))
                    .expect("partition source repair")
            };
            let repaired = apply_fix(&source, &fix);
            assert_eq!(
                source.matches("outer comment").count(),
                repaired.matches("outer comment").count()
            );
            let module =
                crate::prime::lower::lower_module(crate::pass::parser::parse(&repaired).unwrap())
                    .unwrap();
            Resolver::check_module(&module).unwrap_or_else(|error| panic!("{repaired}\n{error:?}"));
        }
    }
}

#[test]
#[cfg(feature = "surface")]
fn implicit_group_wrap_inserts_only_missing_braced_peer_separator() {
    fn source_module(source: &str) -> crate::ast::Module<crate::ast::Lowered> {
        let parsed = crate::pass::parser::parse(source).unwrap();
        let desugared = crate::pass::desugar::desugar_module(parsed).unwrap();
        let (mut modules, _) = crate::pass::label_elab::elaborate_package(
            vec![(std::path::PathBuf::from("test.kio"), desugared)],
            None,
        )
        .unwrap();
        modules.pop().unwrap().1
    }

    for suffix in ["", ";", " // between declaration and suffix\n;"] {
        let source = format!(
            "module x; newtype A : . | B {{ constructor make_a; projector get_a }}{suffix}\nnewtype B : . | A {{ constructor make_b; projector get_b }}"
        );
        let module = source_module(&source);
        let error = Resolver::check_module(&module).expect_err("implicit mutual group");
        let [fix] = error.diagnostic().fixes() else {
            panic!("exact group fix: {error:?}")
        };
        let mut repaired = source.to_owned();
        let mut edits = fix.edits.iter().collect::<Vec<_>>();
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
        for edit in edits {
            assert!(edit.replacement_parts.is_empty());
            repaired.replace_range(
                edit.span.start as usize..edit.span.end as usize,
                &edit.replacement,
            );
        }
        let module = source_module(&repaired);
        Resolver::check_module(&module).unwrap_or_else(|error| panic!("{repaired}\n{error:?}"));
        assert_eq!(
            source.matches("between declaration").count(),
            repaired.matches("between declaration").count()
        );
    }
}

#[test]
fn cyclic_component_classification_projected_emission_is_linear() {
    const COUNT: usize = 128;
    let (_, group, analysis) = independent_group(COUNT);
    let expected = expected_partition(&group);
    reset_type_rec_classification_work();
    let actual = emit_type_rec_partition(group, &analysis);
    let work = type_rec_classification_work();
    assert_eq!(actual, expected);
    assert_eq!(work, (COUNT, COUNT));
}

#[test]
fn cyclic_component_classification_split_fix_is_linear() {
    const COUNT: usize = 128;
    let (source, group, analysis) = independent_group(COUNT);
    reset_type_rec_classification_work();
    let fix = type_rec_partition_fix(&group, &analysis).expect("complete split fix");
    let work = type_rec_classification_work();
    assert_eq!(fix, expected_fix(&group));
    let repaired = apply_fix(&source, &fix);
    let expected = format!("module x;\n{}\n", (0..COUNT).map(|index| format!(
        "rec newtype Loop{index} : . | Loop{index} {{ constructor make{index}; projector read{index}; }};"
    )).collect::<Vec<_>>().join("\n"));
    assert_eq!(repaired, expected);
    let module =
        crate::prime::lower::lower_module(crate::pass::parser::parse(&repaired).unwrap()).unwrap();
    Resolver::check_module(&module).expect("every split singleton independently resolves");
    assert_eq!(work, (COUNT, COUNT));
}

#[test]
fn cyclic_component_classification_resolver_diagnostic_is_linear() {
    const COUNT: usize = 128;
    let (_, group, analysis) = independent_group(COUNT);
    reset_type_rec_classification_work();
    let error = Resolver::validate_type_rec_group_with_analysis(&group, &analysis)
        .expect_err("independent self-cycles cannot form one written group");
    let work = type_rec_classification_work();
    let mut expected = Error::parse(
        group.rec_span.unwrap(),
        "this `rec` group contains multiple independent components",
    );
    for (index, member) in group.members.iter().enumerate() {
        let TypeRecMember::Newtype(newtype) = member else {
            unreachable!()
        };
        expected = expected.with_secondary(
            newtype.name_span,
            format!("`Loop{index}` starts an independent recursive component"),
        );
    }
    expected = expected
        .with_help("keep exactly one genuinely mutual recursive component in each `rec` group")
        .with_fix(expected_fix(&group));
    assert_eq!(error.diagnostic(), expected.diagnostic());
    assert_eq!(work, (COUNT * 2, COUNT * 2));
}

#[test]
fn cyclic_component_classification_preserves_mixed_partitions() {
    let source = "module x; rec {\n\
        type Alias = Left;\n\
        /// Left member.\n\
        pub newtype Left : Right { pub constructor make_left; projector read_left; };\n\
        newtype Right : Left { constructor make_right; projector read_right; };\n\
        newtype Alone : Alone { constructor make_alone; projector read_alone; };\n\
        type Base = .;\n}";
    let mut module =
        crate::prime::lower::lower_module(crate::pass::parser::parse(source).unwrap()).unwrap();
    let Item::TypeRecGroup(group) = module.items.pop().unwrap() else {
        unreachable!()
    };
    let analysis = analyze_type_rec_members(&group.members);
    let expected = vec![
        Item::TypeRecGroup(TypeRecGroup {
            members: group.members[1..3].to_vec(),
            doc: None,
            source_layout: None,
            rec_span: group.rec_span,
            open_brace_span: group.open_brace_span,
            close_brace_span: group.close_brace_span,
            deferred_rec_labels_diagnostic: None,
            meta: Meta::new(group.meta.span),
        }),
        type_rec_member_item(group.members[0].clone()),
        {
            let TypeRecMember::Newtype(mut newtype) = group.members[3].clone() else {
                unreachable!()
            };
            newtype.rec_span = group.rec_span;
            Item::Newtype(newtype)
        },
        type_rec_member_item(group.members[4].clone()),
    ];
    let actual = emit_type_rec_partition(group, &analysis);
    assert_eq!(actual, expected);
    module.items = actual;
    Resolver::check_module(&module).expect("mixed partition independently resolves");
    assert!(
        module
            .items
            .iter()
            .any(|item| matches!(item, Item::TypeRecGroup(_)))
    );
}
