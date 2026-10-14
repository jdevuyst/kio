use super::*;
use crate::ast::{EnrichedArm, Meta, Param, Signature, SignatureGroup, Type};
use crate::span::Span;

fn meta() -> Meta<Routed> {
    Meta::new(Span::new(0, 0))
}
fn unit_ty() -> Type<Routed> {
    Type::Unit { meta: meta() }
}
fn unit() -> Expr<Routed> {
    Expr::Unit {
        occurrence: (),
        source_trivia: (),
        meta: meta(),
    }
}
fn reference(name: &str) -> Expr<Routed> {
    Expr::LowBoundRef {
        occurrence: (),
        name: name.to_owned(),
        meta: meta(),
        ext: (),
    }
}
fn sig(names: &[&str]) -> Signature<Routed> {
    Signature::from_groups(vec![SignatureGroup::Value(
        names
            .iter()
            .map(|name| Param {
                name: (*name).to_owned(),
                ty: Some(unit_ty()),
                pattern: (),
                meta: meta(),
            })
            .collect(),
    )])
}
fn call(name: &str, types: usize, args: Vec<Expr<Routed>>) -> Expr<Routed> {
    Expr::LowClosureCall {
        occurrence: (),
        name: name.to_owned(),
        type_args: (0..types).map(|_| unit_ty()).collect(),
        args,
        meta: meta(),
        ext: (),
    }
}
fn tuple(items: Vec<Expr<Routed>>) -> Expr<Routed> {
    Expr::EnrichedTuple {
        occurrence: (),
        items,
        synth_ty: Type::Product {
            left: Box::new(unit_ty()),
            right: Box::new(unit_ty()),
            meta: meta(),
        },
        meta: meta(),
        ext: (),
    }
}
fn invokes<'a, 'b>(entry: &'b ControlEntry<'a>) -> Vec<&'b Invoke<'a>> {
    entry
        .blocks
        .iter()
        .filter_map(|block| match &block.exit {
            Exit::Invoke(invoke) => Some(invoke),
            _ => None,
        })
        .collect()
}
fn rejected(action: impl FnOnce()) {
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)).is_err());
}

#[test]
fn pending_operands_preserve_earlier_results_and_only_live_bindings() {
    let signature = sig(&["f", "g", "x", "unused"]);
    let body = tuple(vec![
        call("f", 0, vec![]),
        call("g", 0, vec![]),
        reference("x"),
    ]);
    let bodies = ExecutableBodyPlan::new(&signature, &body);
    let control = ControlPlan::new(&bodies);
    let entry = control.entry(bodies.entry_for_body(&body).id);
    assert_eq!(entry.resumes.len(), 2);
    let names = |slots: &[Slot]| {
        slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Binding(id) => Some(bodies.binding(*id).name),
                Slot::Temporary(_) => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&entry.resumes[0].live), ["g", "x"]);
    assert_eq!(names(&entry.resumes[1].live), ["x"]);
    assert!(entry.resumes[1].live.contains(&entry.resumes[0].result));
    assert!(!entry.resumes[0].live.contains(&entry.resumes[0].result));
}

#[test]
fn leading_type_execution_precedes_argument_execution_and_final_value_stage() {
    let signature = sig(&["poly", "argument"]);
    let body = call("poly", 2, vec![call("argument", 0, vec![])]);
    let bodies = ExecutableBodyPlan::new(&signature, &body);
    let control = ControlPlan::new(&bodies);
    let entry = control.entry(bodies.entry_for_body(&body).id);
    let calls = invokes(entry);
    assert_eq!(
        calls.iter().map(|call| call.stage).collect::<Vec<_>>(),
        [
            TailStage::Type,
            TailStage::Type,
            TailStage::Value,
            TailStage::Value
        ]
    );
    assert_eq!(calls[0].type_index, Some(0));
    assert_eq!(calls[1].type_index, Some(1));
    assert!(std::ptr::eq(calls[0].source, &body) && std::ptr::eq(calls[1].source, &body));
    assert!(!std::ptr::eq(calls[2].source, &body));
    assert!(std::ptr::eq(calls[3].source, &body));
    assert_eq!(calls[3].completion, Completion::Return);
    assert!(entry.resumes[2].live.contains(&entry.resumes[1].result));
}

#[test]
fn final_type_stage_returns_callable_as_data_without_an_extra_invocation() {
    let signature = sig(&["poly"]);
    let body = Expr::LowTypeApplication {
        occurrence: (),
        callee: Box::new(reference("poly")),
        type_arg: unit_ty(),
        meta: meta(),
        ext: (),
    };
    let bodies = ExecutableBodyPlan::new(&signature, &body);
    let control = ControlPlan::new(&bodies);
    let entry = control.entry(bodies.entry_for_body(&body).id);
    assert!(!entry.has_pending_work());
    assert_eq!(invokes(entry).len(), 1);
    assert_eq!(invokes(entry)[0].stage, TailStage::Type);
    assert_eq!(invokes(entry)[0].completion, Completion::Return);
}

#[test]
fn branch_join_binds_one_result_and_match_arms_keep_distinct_shadowed_inputs() {
    let signature = sig(&["condition", "sum", "x"]);
    let selected = Expr::EnrichedMatch {
        occurrence: (),
        scrutinee: Box::new(reference("sum")),
        arms: vec![
            EnrichedArm {
                param: "x".into(),
                body: reference("x"),
                meta: meta(),
            },
            EnrichedArm {
                param: "x".into(),
                body: reference("x"),
                meta: meta(),
            },
        ],
        scrutinee_ty: Type::Sum {
            left: Box::new(unit_ty()),
            right: Box::new(unit_ty()),
            meta: meta(),
        },
        result_ty: unit_ty(),
        meta: meta(),
        ext: (),
    };
    let branch = Expr::EnrichedConditional {
        occurrence: (),
        cond: Box::new(call("condition", 0, vec![])),
        then_branch: Box::new(selected),
        else_branch: Box::new(reference("x")),
        result_ty: unit_ty(),
        meta: meta(),
        ext: (),
    };
    let body = tuple(vec![branch, unit()]);
    let bodies = ExecutableBodyPlan::new(&signature, &body);
    let control = ControlPlan::new(&bodies);
    let entry = control.entry(bodies.entry_for_body(&body).id);
    let arms = entry
        .blocks
        .iter()
        .find_map(|block| match &block.exit {
            Exit::Match { arms, .. } => Some(arms),
            _ => None,
        })
        .unwrap();
    assert_ne!(entry.blocks[arms[0]].input, entry.blocks[arms[1]].input);
    assert!(entry.blocks[arms[0]].live.is_empty() && entry.blocks[arms[1]].live.is_empty());
    assert_eq!(
        entry
            .temporaries
            .iter()
            .filter(|producer| producer.kind == ProducerKind::Join)
            .count(),
        2
    );
}

#[test]
fn swapped_branch_and_match_destinations_are_rejected_even_with_equal_liveness() {
    let signature = sig(&["condition", "sum"]);
    let body = Expr::EnrichedConditional {
        occurrence: (),
        cond: Box::new(reference("condition")),
        then_branch: Box::new(unit()),
        else_branch: Box::new(unit()),
        result_ty: unit_ty(),
        meta: meta(),
        ext: (),
    };
    let bodies = ExecutableBodyPlan::new(&signature, &body);
    let mut control = ControlPlan::new(&bodies);
    let entry = control.entry_mut(bodies.entry_for_body(&body).id);
    let Exit::Branch {
        then_block,
        else_block,
        ..
    } = &mut entry.blocks[0].exit
    else {
        panic!()
    };
    std::mem::swap(then_block, else_block);
    rejected(|| entry.validate(&bodies));

    let body = Expr::EnrichedMatch {
        occurrence: (),
        scrutinee: Box::new(reference("sum")),
        arms: vec![
            EnrichedArm {
                param: "left".into(),
                body: unit(),
                meta: meta(),
            },
            EnrichedArm {
                param: "right".into(),
                body: unit(),
                meta: meta(),
            },
        ],
        scrutinee_ty: Type::Sum {
            left: Box::new(unit_ty()),
            right: Box::new(unit_ty()),
            meta: meta(),
        },
        result_ty: unit_ty(),
        meta: meta(),
        ext: (),
    };
    let bodies = ExecutableBodyPlan::new(&signature, &body);
    let mut control = ControlPlan::new(&bodies);
    let entry = control.entry_mut(bodies.entry_for_body(&body).id);
    let Exit::Match { arms, .. } = &mut entry.blocks[0].exit else {
        panic!()
    };
    arms.swap(0, 1);
    rejected(|| entry.validate(&bodies));
}

#[test]
fn malformed_resume_capture_or_result_destination_is_rejected() {
    let signature = sig(&["f", "x"]);
    let body = tuple(vec![call("f", 0, vec![]), reference("x")]);
    let bodies = ExecutableBodyPlan::new(&signature, &body);
    let id = bodies.entry_for_body(&body).id;
    let mut control = ControlPlan::new(&bodies);
    let entry = control.entries.get_mut(&id).unwrap();
    entry.resumes[0].live.clear();
    rejected(|| entry.validate(&bodies));
    entry.resumes[0].live = entry.blocks[entry.resumes[0].block].live.clone();
    entry.resumes[0].result = Slot::Temporary(entry.temporaries.len() - 1);
    rejected(|| entry.validate(&bodies));
}

#[test]
fn omitted_or_reordered_atomic_operands_fail_before_serialization() {
    let signature = sig(&["x", "y"]);
    let body = tuple(vec![reference("x"), reference("y")]);
    let bodies = ExecutableBodyPlan::new(&signature, &body);
    let mut control = ControlPlan::new(&bodies);
    let entry = control
        .entries
        .get_mut(&bodies.entry_for_body(&body).id)
        .unwrap();
    let Operation::Atom { operands, .. } = &mut entry.blocks[0].operations[0] else {
        panic!()
    };
    operands.swap(0, 1);
    rejected(|| entry.validate(&bodies));
    let Operation::Atom { operands, .. } = &mut entry.blocks[0].operations[0] else {
        panic!()
    };
    operands.swap(0, 1);
    operands.pop();
    rejected(|| entry.validate(&bodies));
}

#[test]
fn invoke_result_stage_and_cps_packet_are_exact_not_shape_guessed() {
    let signature = sig(&["receiver", "continuation", "other"]);
    let body = tuple(vec![
        Expr::LowCpsProjectorApply {
            occurrence: (),
            newtype: "Box".into(),
            module_path: "p/main".into(),
            type_args: Vec::new(),
            receiver: Box::new(reference("receiver")),
            continuation: Box::new(reference("continuation")),
            continuation_ty: Type::Function {
                param: Box::new(unit_ty()),
                ret: Box::new(Type::Function {
                    param: Box::new(unit_ty()),
                    ret: Box::new(unit_ty()),
                    abi_arity: 0,
                    caps: Default::default(),
                    meta: meta(),
                }),
                abi_arity: 1,
                caps: Default::default(),
                meta: meta(),
            },
            meta: meta(),
            ext: (),
        },
        reference("other"),
    ]);
    let bodies = ExecutableBodyPlan::new(&signature, &body);
    let mut control = ControlPlan::new(&bodies);
    let entry = control
        .entries
        .get_mut(&bodies.entry_for_body(&body).id)
        .unwrap();
    let Slot::Temporary(result) = entry.resumes[0].result else {
        panic!()
    };
    assert_eq!(entry.temporaries[result].kind, ProducerKind::ValueStage);
    entry.temporaries[result].kind = ProducerKind::TypeStage(0);
    rejected(|| entry.validate(&bodies));
    entry.temporaries[result].kind = ProducerKind::ValueStage;
    let Exit::Invoke(invoke) = &mut entry.blocks[0].exit else {
        panic!()
    };
    invoke.receiver = Some(invoke.callee);
    rejected(|| entry.validate(&bodies));
}
