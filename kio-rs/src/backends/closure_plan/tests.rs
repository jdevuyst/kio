use super::*;
use crate::ast::{Meta, Param, SignatureGroup};
use crate::backends::boundary_facade::PreparedBoundaryScheme;
use crate::span::Span;
use std::collections::BTreeSet;

fn meta() -> Meta<Routed> {
    Meta::new(Span::new(0, 0))
}

fn unit() -> Expr<Routed> {
    Expr::Unit {
        occurrence: (),
        source_trivia: (),
        meta: meta(),
    }
}

fn unit_ty() -> Type<Routed> {
    Type::Unit { meta: meta() }
}

fn type_param(name: &str) -> TypeParam {
    TypeParam {
        presentation: Default::default(),
        name: name.into(),
        span: Span::new(0, 0),
        kind: None,
    }
}

fn values(names: &[&str]) -> SignatureGroup<Routed> {
    SignatureGroup::Value(
        names
            .iter()
            .map(|name| Param {
                name: (*name).into(),
                ty: Some(unit_ty()),
                pattern: (),
                meta: meta(),
            })
            .collect(),
    )
}

fn reference(name: &str) -> Expr<Routed> {
    Expr::LowBoundRef {
        occurrence: (),
        name: name.into(),
        meta: meta(),
        ext: (),
    }
}

fn closure(sig: Signature<Routed>, body: Expr<Routed>) -> Expr<Routed> {
    Expr::FnExpr {
        occurrence: (),
        sig,
        ret_ty: Some(unit_ty()),
        body: Box::new(body),
        meta: meta(),
        caps: Default::default(),
    }
}

fn names(plan: &ExecutableBodyPlan<'_>, ids: &[BindingId]) -> Vec<String> {
    ids.iter()
        .map(|id| plan.binding(*id).name.to_owned())
        .collect()
}

#[test]
fn residual_stages_keep_only_needed_preceding_inputs_without_removing_slots() {
    let sig = Signature::from_groups(vec![values(&["outer", "unused"])]);
    let body = closure(
        Signature::from_groups(vec![
            values(&["first", "_"]),
            SignatureGroup::Type(vec![type_param("T")]),
            values(&["last"]),
        ]),
        Expr::EnrichedTuple {
            occurrence: (),
            items: vec![reference("outer"), reference("first"), reference("last")],
            synth_ty: Type::Product {
                left: Box::new(unit_ty()),
                right: Box::new(Type::Product {
                    left: Box::new(unit_ty()),
                    right: Box::new(unit_ty()),
                    meta: meta(),
                }),
                meta: meta(),
            },
            meta: meta(),
            ext: (),
        },
    );
    let plan = ExecutableBodyPlan::new(&sig, &body);
    let entry = &plan.entries[1];
    assert_eq!(names(&plan, &entry.stages[0].captures), ["outer"]);
    assert_eq!(names(&plan, &entry.stages[1].captures), ["outer", "first"]);
    assert_eq!(names(&plan, &entry.stages[2].captures), ["outer", "first"]);
    assert_eq!(names(&plan, &entry.stages[0].bindings), ["first", "_"]);
    assert!(plan.entries[0].captures().is_empty());
}

#[test]
fn let_initializer_and_shadowed_body_capture_distinct_lexical_identities() {
    let sig = Signature::from_groups(vec![values(&["x"])]);
    let nested_sig = || Signature::from_groups(vec![values(&[])]);
    let body = Expr::Let {
        occurrence: (),
        name: "x".into(),
        name_span: Span::new(0, 0),
        ty: None,
        pattern: (),
        value: Box::new(closure(nested_sig(), reference("x"))),
        body: Box::new(closure(nested_sig(), reference("x"))),
        meta: meta(),
    };
    let plan = ExecutableBodyPlan::new(&sig, &body);
    let initializer = plan.entries[1].captures()[0];
    let shadowed = plan.entries[2].captures()[0];
    assert_ne!(initializer, shadowed);
    assert_eq!(plan.binding(initializer).name, plan.binding(shadowed).name);
    assert!(matches!(
        plan.binding(initializer).origin,
        BindingOrigin::Parameter { .. }
    ));
    assert!(matches!(
        plan.binding(shadowed).origin,
        BindingOrigin::Local { .. }
    ));
}

#[test]
fn capture_used_only_by_nested_entry_propagates_through_parent() {
    let sig = Signature::from_groups(vec![values(&["x", "unused"])]);
    let body = closure(
        Signature::from_groups(vec![values(&[])]),
        closure(Signature::from_groups(vec![values(&[])]), reference("x")),
    );
    let plan = ExecutableBodyPlan::new(&sig, &body);
    assert_eq!(
        plan.nested_capture_names(),
        [vec!["x".to_owned()], vec!["x".to_owned()]]
    );
    assert_eq!(plan.entries[1].captures(), plan.entries[2].captures());
}

fn shadowed_type_plan() -> (Signature<Routed>, ValuePlan) {
    let sig = Signature::from_groups(vec![
        SignatureGroup::Type(vec![type_param("A")]),
        SignatureGroup::Type(vec![type_param("A")]),
        values(&[]),
    ]);
    let ty = sig.signature_ty(
        Type::synth_path(vec!["A".into()], vec![], Span::new(0, 0)),
        Span::new(0, 0),
    );
    let facade = BoundaryFacadePlan::from_prepared_scheme(
        PreparedBoundaryScheme::try_new(&ty, &BTreeSet::new()).unwrap(),
    );
    (sig, ValuePlan::from_facade(&facade))
}

#[test]
fn witness_capture_uses_exact_ordered_shadowed_type_scope() {
    let (sig, values) = shadowed_type_plan();
    let body = unit();
    let plan = ExecutableBodyPlan::new(&sig, &body);
    let entry = plan.entry_for_body(&body);
    let stages = entry.typed_stages(&values);
    let scope = stages
        .iter()
        .zip(&entry.stages)
        .filter_map(|(value, source)| value.binder.zip(source.type_binding))
        .collect::<Vec<_>>();
    let mut captures = WitnessCaptures::default();
    captures.record(
        &plan,
        WitnessContext::Execute {
            entry: entry.id,
            stage: 2,
        },
        &values,
        match values.use_at(stages[2].use_id) {
            ValueUse::Function { result, .. } => *result,
            _ => unreachable!(),
        },
        &scope,
    );
    assert_eq!(captures.captures(entry.id, 2), &[scope[1].1]);
    assert!(captures.captures(entry.id, 1).is_empty());
}

#[test]
#[should_panic(expected = "complete ordered lexical type scope")]
fn witness_capture_rejects_swapped_same_spelling_lexical_type_ids() {
    let (sig, values) = shadowed_type_plan();
    let body = unit();
    let plan = ExecutableBodyPlan::new(&sig, &body);
    let entry = plan.entry_for_body(&body);
    let stages = entry.typed_stages(&values);
    let scope = vec![
        (
            stages[0].binder.unwrap(),
            entry.stages[1].type_binding.unwrap(),
        ),
        (
            stages[1].binder.unwrap(),
            entry.stages[0].type_binding.unwrap(),
        ),
    ];
    WitnessCaptures::default().record(
        &plan,
        WitnessContext::Execute {
            entry: entry.id,
            stage: 2,
        },
        &values,
        match values.use_at(stages[2].use_id) {
            ValueUse::Function { result, .. } => *result,
            _ => unreachable!(),
        },
        &scope,
    );
}

#[test]
fn construction_witness_runs_before_the_stage_input() {
    let sig = Signature::from_groups(vec![
        SignatureGroup::Type(vec![type_param("A")]),
        values(&[]),
        values(&[]),
    ]);
    let body = unit();
    let plan = ExecutableBodyPlan::new(&sig, &body);
    let entry = plan.entry_for_body(&body);
    let binder = entry.stages[0].type_binding.unwrap();
    let mut captures = WitnessCaptures::default();
    captures.require(
        &plan,
        WitnessContext::Construct {
            entry: entry.id,
            stage: 2,
        },
        binder,
    );
    assert_eq!(captures.captures(entry.id, 1), &[binder]);
    assert!(captures.captures(entry.id, 2).is_empty());
}

#[test]
fn sibling_match_arms_have_distinct_binders_and_restore_the_outer_scope() {
    let sig = Signature::from_groups(vec![values(&["x"])]);
    let matched = Expr::EnrichedMatch {
        occurrence: (),
        scrutinee: Box::new(unit()),
        arms: (0..2)
            .map(|_| crate::ast::EnrichedArm {
                param: "x".into(),
                body: closure(Signature::from_groups(vec![values(&[])]), reference("x")),
                meta: meta(),
            })
            .collect(),
        scrutinee_ty: Type::Sum {
            left: Box::new(unit_ty()),
            right: Box::new(unit_ty()),
            meta: meta(),
        },
        result_ty: unit_ty(),
        meta: meta(),
        ext: (),
    };
    let body = Expr::Seq {
        occurrence: (),
        value: Box::new(matched),
        body: Box::new(closure(
            Signature::from_groups(vec![values(&[])]),
            reference("x"),
        )),
        meta: meta(),
    };
    let plan = ExecutableBodyPlan::new(&sig, &body);
    let captures = plan
        .entries
        .iter()
        .skip(1)
        .map(|entry| entry.captures()[0])
        .collect::<HashSet<_>>();
    assert_eq!(captures.len(), 3);
    assert_eq!(
        plan.entries[3].captures(),
        &plan.entries[0].stages[0].bindings
    );
}

#[test]
fn empty_value_and_type_only_entries_keep_distinct_executing_stages() {
    let body = unit();
    let empty = Signature::from_groups(vec![]);
    let value = ExecutableBodyPlan::new(&empty, &body);
    assert_eq!(value.entries[0].stages.len(), 1);
    assert!(matches!(
        value.entries[0].stages[0].input,
        StageInput::Value([])
    ));
    let typed = Signature::from_groups(vec![SignatureGroup::Type(vec![
        type_param("A"),
        type_param("B"),
    ])]);
    let types = ExecutableBodyPlan::new(&typed, &body);
    assert_eq!(types.entries[0].stages.len(), 2);
    assert!(
        types.entries[0]
            .stages
            .iter()
            .all(|stage| matches!(stage.input, StageInput::Type(_)))
    );
}

#[test]
fn synthetic_cps_spines_share_checked_type_stages_and_payload_identity() {
    let function = |domain, result| Type::Function {
        param: Box::new(domain),
        ret: Box::new(result),
        abi_arity: 1,
        caps: Default::default(),
        meta: meta(),
    };
    let forall = |name: &str, body| Type::Forall {
        param: type_param(name),
        body: Box::new(body),
        meta: meta(),
    };
    let continuation = forall("X", forall("Y", function(unit_ty(), unit_ty())));
    let ty = forall("R", forall("S", function(continuation, unit_ty())));
    let facade = BoundaryFacadePlan::from_prepared_scheme(
        PreparedBoundaryScheme::try_new(&ty, &BTreeSet::new()).unwrap(),
    );
    let values = ValuePlan::from_facade(&facade);
    let recipe = cps_entry_plan(&values, values.root());
    let runtime = recipe.runtime();
    assert_eq!(runtime.type_stages, 2);
    assert_eq!(runtime.continuation_type_stages, 2);
    assert_eq!(runtime.captures(), &[CpsBinding::Payload]);
    assert_eq!(
        recipe.stage_witness_captures(&values, &[]),
        [vec![], vec![], vec![]]
    );
}
