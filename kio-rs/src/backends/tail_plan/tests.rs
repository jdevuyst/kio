use super::*;
use crate::ast::{EnrichedArm, Meta, Signature, SignatureGroup};
use crate::span::Span;

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

fn unit_ty() -> crate::ast::Type<Routed> {
    crate::ast::Type::Unit { meta: meta() }
}

fn call() -> Expr<Routed> {
    Expr::LowClosureCall {
        occurrence: (),
        name: "callback".into(),
        type_args: Vec::new(),
        args: Vec::new(),
        meta: meta(),
        ext: (),
    }
}

fn seq(value: Expr<Routed>, body: Expr<Routed>) -> Expr<Routed> {
    Expr::Seq {
        occurrence: (),
        value: Box::new(value),
        body: Box::new(body),
        meta: meta(),
    }
}

fn conditional() -> Expr<Routed> {
    Expr::EnrichedConditional {
        occurrence: (),
        cond: Box::new(call()),
        then_branch: Box::new(call()),
        else_branch: Box::new(call()),
        result_ty: unit_ty(),
        meta: meta(),
        ext: (),
    }
}

#[test]
fn only_selected_result_edges_inherit_tail_context() {
    let branch = conditional();
    for edge in [ResultChild::ThenBranch, ResultChild::ElseBranch] {
        assert_eq!(
            TailContext::body(&branch).result_child(edge).tail_stage(),
            Some(TailStage::Value)
        );
        assert_eq!(
            TailContext::operand(&branch)
                .result_child(edge)
                .tail_stage(),
            None
        );
    }
    let Expr::EnrichedConditional { cond, .. } = &branch else {
        unreachable!()
    };
    assert_eq!(TailContext::operand(cond).tail_stage(), None);

    let matched = Expr::EnrichedMatch {
        occurrence: (),
        scrutinee: Box::new(call()),
        arms: vec![
            EnrichedArm {
                param: "left".into(),
                body: call(),
                meta: meta(),
            },
            EnrichedArm {
                param: "right".into(),
                body: unit(),
                meta: meta(),
            },
        ],
        scrutinee_ty: unit_ty(),
        result_ty: unit_ty(),
        meta: meta(),
        ext: (),
    };
    assert!(TailContext::body(&matched).has_tail_transfer());
    assert!(!TailContext::operand(&matched).has_tail_transfer());
    assert!(
        TailContext::body(&matched)
            .result_child(ResultChild::MatchArm(1))
            .is_tail_position()
    );
    assert!(
        !TailContext::operand(&matched)
            .result_child(ResultChild::MatchArm(1))
            .is_tail_position()
    );
    assert_eq!(
        TailContext::body(&matched)
            .result_child(ResultChild::MatchArm(1))
            .tail_stage(),
        None
    );
    assert_eq!(
        TailContext::operand(&matched)
            .result_child(ResultChild::MatchArm(0))
            .tail_stage(),
        None
    );
}

#[test]
fn pending_operand_work_does_not_disable_the_enclosing_final_call() {
    let body = seq(call(), call());
    let context = TailContext::body(&body);
    assert!(context.has_tail_transfer());
    assert_eq!(
        context.result_child(ResultChild::SeqBody).tail_stage(),
        Some(TailStage::Value)
    );
    let Expr::Seq { value, .. } = &body else {
        unreachable!()
    };
    assert_eq!(TailContext::operand(value).tail_stage(), None);
    assert!(!TailContext::body(&seq(call(), unit())).has_tail_transfer());

    let outer = Expr::LowIndirectCall {
        occurrence: (),
        callee: Box::new(call()),
        type_args: Vec::new(),
        args: vec![call()],
        meta: meta(),
        ext: (),
    };
    let Expr::LowIndirectCall { callee, args, .. } = &outer else {
        unreachable!()
    };
    assert_eq!(
        TailContext::body(&outer).tail_stage(),
        Some(TailStage::Value)
    );
    assert_eq!(TailContext::operand(callee).tail_stage(), None);
    assert_eq!(TailContext::operand(&args[0]).tail_stage(), None);
}

#[test]
fn let_result_and_constructor_payload_have_distinct_contexts() {
    let binding = Expr::Let {
        occurrence: (),
        name: "saved".into(),
        name_span: Span::new(0, 0),
        ty: None,
        pattern: (),
        value: Box::new(call()),
        body: Box::new(call()),
        meta: meta(),
    };
    assert_eq!(
        TailContext::body(&binding)
            .result_child(ResultChild::LetBody)
            .tail_stage(),
        Some(TailStage::Value)
    );
    assert_eq!(
        TailContext::operand(&binding)
            .result_child(ResultChild::LetBody)
            .tail_stage(),
        None
    );
    let constructor = Expr::LowNewtypeCtor {
        occurrence: (),
        newtype: "Box".into(),
        member: "make".into(),
        type_args: Vec::new(),
        payload: Box::new(call()),
        meta: meta(),
        ext: (),
    };
    assert!(!TailContext::body(&constructor).has_tail_transfer());
    let Expr::LowNewtypeCtor { payload, .. } = &constructor else {
        unreachable!()
    };
    assert_eq!(TailContext::operand(payload).tail_stage(), None);
}

#[test]
fn named_type_only_heads_do_not_invent_a_value_stage() {
    let parameter = crate::ast::TypeParam {
        name: "A".into(),
        span: Span::new(0, 0),
        kind: None,
        presentation: Default::default(),
    };
    let sig = Signature::from_groups(vec![SignatureGroup::Type(vec![parameter])]);
    let mut named = Expr::LowModuleCall {
        occurrence: (),
        mangled: "ordinary".into(),
        type_args: Vec::new(),
        args: Vec::new(),
        sig,
        ret_ty: Some(unit_ty()),
        meta: meta(),
        ext: (),
    };
    assert_eq!(TailContext::body(&named).tail_stage(), None);
    let Expr::LowModuleCall { type_args, .. } = &mut named else {
        unreachable!()
    };
    type_args.push(unit_ty());
    assert_eq!(
        TailContext::body(&named).tail_stage(),
        Some(TailStage::Type)
    );
    let Expr::LowModuleCall { sig, .. } = &mut named else {
        unreachable!()
    };
    sig.groups
        .push(crate::ast::SignatureGroupKind::Value { len: 0 });
    assert_eq!(
        TailContext::body(&named).tail_stage(),
        Some(TailStage::Value)
    );
}

#[test]
fn constructing_a_callable_is_not_executing_its_fresh_body() {
    let closure = Expr::FnExpr {
        occurrence: (),
        sig: Signature::from_groups(vec![SignatureGroup::Value(Vec::new())]),
        ret_ty: Some(unit_ty()),
        body: Box::new(call()),
        meta: meta(),
        caps: Default::default(),
    };
    assert_eq!(TailContext::body(&closure).tail_stage(), None);
    assert!(!TailContext::body(&closure).has_tail_transfer());
    let body = TailContext::operand(&closure).result_child(ResultChild::FunctionBody);
    assert_eq!(body.tail_stage(), Some(TailStage::Value));
    assert!(body.has_tail_transfer());
}

#[test]
fn type_application_is_executable_even_if_it_returns_a_callable() {
    let application = Expr::LowTypeApplication {
        occurrence: (),
        callee: Box::new(call()),
        type_arg: unit_ty(),
        meta: meta(),
        ext: (),
    };
    assert_eq!(
        TailContext::body(&application).tail_stage(),
        Some(TailStage::Type)
    );
    assert_eq!(TailContext::operand(&application).tail_stage(), None);
    assert!(TailContext::body(&application).has_tail_transfer());
}

#[test]
fn known_host_leaves_need_no_guest_dispatch_driver() {
    let host = Expr::LowHostCall {
        occurrence: (),
        name: "capability".into(),
        module_path: "host".into(),
        type_args: Vec::new(),
        args: Vec::new(),
        sig: Signature::from_groups(vec![SignatureGroup::Value(Vec::new())]),
        ret_ty: unit_ty(),
        meta: meta(),
        ext: (),
    };
    assert_eq!(TailContext::body(&host).tail_stage(), None);
    assert!(!TailContext::body(&host).has_tail_transfer());
}

#[test]
#[should_panic(expected = "a routed tail edge must match its expression owner")]
fn result_edges_cannot_be_attached_to_an_arbitrary_expression() {
    TailContext::body(&call()).result_child(ResultChild::LetBody);
}
