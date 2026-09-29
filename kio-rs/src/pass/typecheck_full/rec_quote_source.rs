//! Compose suspended source operands from retained, closed typing results.
//!
//! Ordinary children are sequenced with lets. Only a suspended child's callback
//! and an affected parent's computation need new annotations; those annotations
//! come from the same owner-close transaction as the checked source.

use std::collections::{HashMap, HashSet};

use super::{InternedType, RecordedElaboration, UserElaboratorReplayEnv};
use crate::ast::{
    CallArg, ElaboratorCall, Expr, Lowered, Meta, Param, PathSegment, RecQuotePlan, Signature,
    SignatureParam, Type,
};
use crate::error::Error;
use crate::pass::substitute::{self, PrePrime};
use crate::span::Span;

type RuntimeExpr = Expr<PrePrime>;
type RuntimeType = Type<PrePrime>;

pub(super) fn fresh_name() -> String {
    crate::normalization::fresh_checked_term_value_name("quoted_source")
}

pub(super) fn local(name: String, span: Span) -> RuntimeExpr {
    Expr::Path {
        occurrence: (),
        segments: vec![PathSegment::new(name, span)],
        meta: Meta::new(span),
        ext: (),
    }
}

pub(super) fn call(function: RuntimeExpr, argument: RuntimeExpr, span: Span) -> RuntimeExpr {
    Expr::synth_call(function, vec![CallArg::Value(argument)], span)
}

pub(super) fn arrow(input: RuntimeType, result: RuntimeType, span: Span) -> RuntimeType {
    Type::Function {
        param: Box::new(input),
        ret: Box::new(result),
        abi_arity: 1,
        caps: (),
        meta: Meta::new(span),
    }
}

pub(super) fn lambda(
    name: String,
    input: RuntimeType,
    result: RuntimeType,
    body: RuntimeExpr,
    span: Span,
) -> RuntimeExpr {
    Expr::FnExpr {
        occurrence: (),
        sig: Signature::new(vec![SignatureParam::Value(Param {
            name,
            ty: Some(input),
            pattern: (),
            meta: Meta::new(span),
        })]),
        ret_ty: Some(result),
        body: Box::new(body),
        meta: Meta::new(span),
        caps: (),
    }
}

fn bind(name: String, value: RuntimeExpr, body: RuntimeExpr, span: Span) -> RuntimeExpr {
    Expr::Let {
        occurrence: (),
        name,
        name_span: span,
        ty: None,
        pattern: (),
        value: Box::new(value),
        body: Box::new(body),
        meta: Meta::new(span),
    }
}

pub(super) fn public_type(
    ty: &InternedType,
    ambient: &HashSet<String>,
    replay: &mut UserElaboratorReplayEnv<'_>,
) -> RuntimeType {
    assert!(
        !crate::pass::typecheck_core::type_contains_goal(ty.as_type()),
        "source composition requires closed types"
    );
    let mut requalifier = super::user_elaborator_replay_requalifier(replay);
    let written = if ty.identity_is_canonical() {
        requalifier.rewrite(ty.as_type(), ambient)
    } else {
        ty.clone_type()
    };
    let result = crate::ast::convert_type::<crate::ast::UncheckedPrime, PrePrime>(
        &substitute::substitute_type_for_eval(
            &written,
            replay.elaborations,
            replay.module_path,
            replay.package,
            Some(replay.module),
        ),
    );
    let imports = requalifier
        .take_imports()
        .into_iter()
        .map(|import| {
            let crate::ast::ImportKind::Qualified { path, alias } = import.kind else {
                unreachable!("requalification imports are qualified")
            };
            super::UserElaboratorTemplateGeneratedImport {
                module_path: super::module_import_path(&path),
                alias,
            }
        })
        .collect::<Vec<_>>();
    replay
        .elaborations
        .record_prime_requalification_imports(replay.module_path, &imports);
    replay
        .elaborations
        .record_generated_type_aliases(replay.module_path, &requalifier.take_local_aliases());
    result
}

fn ordinary(source: &Expr<Lowered>, replay: &mut UserElaboratorReplayEnv<'_>) -> RuntimeExpr {
    substitute::substitute_expr_for_replayed_elaboration(
        source,
        replay.elaborations,
        replay.module_path,
        replay.package,
        replay.module,
    )
}

struct Completed<'a> {
    source: &'a Expr<Lowered>,
    computation: Option<RuntimeExpr>,
}

impl Completed<'_> {
    fn resume(
        self,
        name: String,
        body: RuntimeExpr,
        runtime: &RuntimeType,
        ambient: &HashSet<String>,
        replay: &mut UserElaboratorReplayEnv<'_>,
    ) -> RuntimeExpr {
        let span = self.source.span();
        match self.computation {
            Some(computation) => {
                let ty = replay
                    .elaborations
                    .rec_quote_source_type(self.source.site().id)
                    .expect("suspended child has a closed public type")
                    .clone();
                let ty = public_type(&ty, ambient, replay);
                call(
                    computation,
                    lambda(name, ty, runtime.clone(), body, span),
                    span,
                )
            }
            None => bind(name, ordinary(self.source, replay), body, span),
        }
    }

    fn finish(
        self,
        continuation: RuntimeExpr,
        replay: &mut UserElaboratorReplayEnv<'_>,
    ) -> RuntimeExpr {
        let span = self.source.span();
        match self.computation {
            Some(computation) => call(computation, continuation, span),
            None => call(continuation, ordinary(self.source, replay), span),
        }
    }
}

/// Splice suspended children into the ordinary substituted runtime expression.
/// Canonical type slots, application stages and residual eta belong exclusively
/// to substitution. Only holes surviving there are executable source operands.
fn replay_children(
    source: &Expr<Lowered>,
    children: Vec<Completed<'_>>,
    continuation: RuntimeExpr,
    runtime: &RuntimeType,
    ambient: &HashSet<String>,
    replay: &mut UserElaboratorReplayEnv<'_>,
) -> Result<RuntimeExpr, Error> {
    use crate::pass::visit_mut::{TypecheckVisitMut, walk_expr};

    let mut bindings = HashMap::new();
    let mut holes = HashMap::new();
    for child in children {
        // An immutable path needs no sequencing binding. Let ordinary replay
        // retain its resolved call root and lexical scope; computed callees
        // still pass through the ordered source holes below.
        if child.computation.is_none() && matches!(child.source, Expr::Path { .. }) {
            continue;
        }
        let name = fresh_name();
        bindings.insert(child.source.site().id, name.clone());
        holes.insert(name, child);
    }
    let mut core = substitute::substitute_expr_with_completed_source_bindings(
        source,
        Some(&bindings),
        replay.elaborations,
        replay.module_path,
        replay.package,
        replay.module,
    );

    // Flattening eager lets must not extend an authored binder's capture scope.
    // Rename only this small replay shell; source subtrees are still opaque holes.
    #[derive(Default)]
    struct Freshen {
        names: HashMap<String, Vec<String>>,
        stack: Vec<String>,
    }
    impl TypecheckVisitMut<PrePrime> for Freshen {
        fn enter_value_binder(&mut self, name: &mut String, _: Span, _: Span) {
            let original = name.clone();
            *name = fresh_name();
            self.names
                .entry(original.clone())
                .or_default()
                .push(name.clone());
            self.stack.push(original);
        }
        fn exit_value_binder(&mut self, _: &mut String, _: Span, _: Span) {
            let original = self.stack.pop().expect("lexical binder entry");
            self.names.get_mut(&original).unwrap().pop();
        }
        fn visit_expr(&mut self, expr: &mut RuntimeExpr) {
            if let Expr::Path { segments, .. } = expr
                && let [name] = segments.as_mut_slice()
                && let Some(replacement) =
                    self.names.get(name.as_str()).and_then(|stack| stack.last())
            {
                name.name = replacement.clone();
            }
            walk_expr(self, expr);
        }
    }
    Freshen::default().visit_expr(&mut core);

    struct Restore<'a, 's, 'e, 'r> {
        holes: &'a HashMap<String, Completed<'s>>,
        replay: &'e mut UserElaboratorReplayEnv<'r>,
        error: Option<Error>,
    }
    impl TypecheckVisitMut<PrePrime> for Restore<'_, '_, '_, '_> {
        fn visit_expr(&mut self, expr: &mut RuntimeExpr) {
            if let Expr::Path { segments, .. } = expr
                && let [name] = segments.as_slice()
                && let Some(child) = self.holes.get(name.as_str())
            {
                if child.computation.is_some() {
                    self.error = Some(Error::type_(
                        child.source.span(),
                        "recursive value escapes in a function",
                    ));
                } else {
                    *expr = ordinary(child.source, self.replay);
                }
                return;
            }
            walk_expr(self, expr);
        }
    }

    enum Work {
        Enter(RuntimeExpr),
        Call(Vec<CallArg<PrePrime>>, Span, usize),
        Let(String, RuntimeExpr, Span),
    }
    enum Step<'a> {
        Ordinary(String, RuntimeExpr, Span),
        Source(String, Completed<'a>),
    }
    let mut pending = vec![Work::Enter(core)];
    let mut values = Vec::new();
    let mut steps = Vec::new();
    while let Some(work) = pending.pop() {
        match work {
            Work::Enter(expr) => {
                let span = expr.span();
                if let Expr::Path { segments, .. } = &expr
                    && let [name] = segments.as_slice()
                    && let Some(child) = holes.remove(name.as_str())
                {
                    let name = fresh_name();
                    values.push(local(name.clone(), span));
                    steps.push(Step::Source(name, child));
                    continue;
                }
                match expr {
                    Expr::Call { callee, args, .. } => {
                        let start = values.len();
                        let mut shell = Vec::with_capacity(args.len());
                        let mut operands = vec![*callee];
                        for arg in args {
                            match arg {
                                CallArg::Type(ty) => shell.push(CallArg::Type(ty)),
                                CallArg::Value(value) => {
                                    operands.push(value);
                                    shell.push(CallArg::Value(Expr::Unit {
                                        occurrence: (),
                                        meta: Meta::new(span),
                                    }));
                                }
                            }
                        }
                        pending.push(Work::Call(shell, span, start));
                        pending.extend(operands.into_iter().rev().map(Work::Enter));
                    }
                    Expr::Let {
                        name, value, body, ..
                    } => {
                        pending.push(Work::Let(name, *body, span));
                        pending.push(Work::Enter(*value));
                    }
                    Expr::Seq { value, body, .. } => {
                        pending.push(Work::Let(fresh_name(), *body, span));
                        pending.push(Work::Enter(*value));
                    }
                    mut expr @ Expr::FnExpr { .. } => {
                        let mut restore = Restore {
                            holes: &holes,
                            replay,
                            error: None,
                        };
                        restore.visit_expr(&mut expr);
                        if let Some(error) = restore.error {
                            return Err(error);
                        }
                        values.push(expr);
                    }
                    expr => values.push(expr),
                }
            }
            Work::Call(mut args, span, start) => {
                let mut children = values.drain(start..);
                let callee = children.next().expect("runtime call callee");
                for arg in &mut args {
                    if let CallArg::Value(value) = arg {
                        *value = children.next().expect("runtime call operand");
                    }
                }
                assert!(children.next().is_none());
                drop(children);
                let name = fresh_name();
                steps.push(Step::Ordinary(
                    name.clone(),
                    Expr::synth_call(callee, args, span),
                    span,
                ));
                values.push(local(name, span));
            }
            Work::Let(name, body, span) => {
                let value = values.pop().expect("runtime let value");
                steps.push(Step::Ordinary(name, value, span));
                pending.push(Work::Enter(body));
            }
        }
    }
    let result = values.pop().expect("one runtime replay result");
    assert!(values.is_empty());
    let mut body = call(continuation, result, source.span());
    for step in steps.into_iter().rev() {
        body = match step {
            Step::Ordinary(name, value, span) => bind(name, value, body, span),
            Step::Source(name, child) => child.resume(name, body, runtime, ambient, replay),
        };
    }
    Ok(body)
}

/// `None` leaves an ordinary source on its existing replay path. A computation
/// contains only ordinary PrePrime syntax and has type (Public -> Runtime) -> Runtime.
pub(super) fn compose(
    source: &Expr<Lowered>,
    runtime: &RuntimeType,
    ambient: &HashSet<String>,
    replay: &mut UserElaboratorReplayEnv<'_>,
) -> Result<Option<RuntimeExpr>, Error> {
    enum Work<'a> {
        Enter(&'a Expr<Lowered>),
        Finish(&'a Expr<Lowered>, usize),
    }
    let mut pending = vec![Work::Enter(source)];
    let mut ready: Vec<Completed<'_>> = Vec::new();
    while let Some(work) = pending.pop() {
        match work {
            Work::Enter(node) => {
                let computation = match node {
                    Expr::RecQuote { plan, .. } => match plan.as_ref() {
                        RecQuotePlan::Operand { computation, .. } => {
                            Some(ordinary(computation, replay))
                        }
                        RecQuotePlan::Expansion { .. } => {
                            super::rec_quote::finish_expansion(node, replay)?;
                            None
                        }
                    },
                    Expr::UserElaborator { ext, .. }
                    | Expr::Ufcs {
                        ext, bang: Some(_), ..
                    } => replay.elaborations.rec_quote_computation(*ext).cloned(),
                    _ => {
                        pending.push(Work::Finish(node, ready.len()));
                        let start = pending.len();
                        match node {
                            Expr::Call { callee, args, .. } => {
                                pending.push(Work::Enter(callee));
                                for arg in args {
                                    if let CallArg::Value(value) = arg {
                                        pending.push(Work::Enter(value));
                                    }
                                }
                            }
                            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                                pending.push(Work::Enter(value));
                                pending.push(Work::Enter(body));
                            }
                            Expr::FnExpr { body, .. } => pending.push(Work::Enter(body)),
                            Expr::RecOrder { plan, .. } => {
                                if replay
                                    .elaborations
                                    .rec_order_runtime_for(replay.module_path, node)
                                    .is_some()
                                {
                                    pending.push(Work::Enter(&plan.value));
                                    pending.push(Work::Enter(&plan.body));
                                } else if matches!(
                                    replay
                                        .elaborations
                                        .elaboration_for(replay.module_path, node),
                                    Some(RecordedElaboration::Lowered(_))
                                ) {
                                    pending.push(Work::Enter(&plan.body));
                                }
                            }
                            Expr::Ufcs {
                                receiver,
                                args,
                                bang: None,
                                ..
                            } => {
                                pending.push(Work::Enter(receiver));
                                for arg in args {
                                    if let CallArg::Value(value) = arg {
                                        pending.push(Work::Enter(value));
                                    }
                                }
                            }
                            Expr::Elaborator { call, .. } => match call {
                                ElaboratorCall::FieldAccess { receiver, .. } => {
                                    pending.push(Work::Enter(receiver));
                                }
                                ElaboratorCall::FieldUpdate { receiver, updates } => {
                                    pending.push(Work::Enter(receiver));
                                    pending.extend(
                                        updates.iter().map(|update| Work::Enter(&update.value)),
                                    );
                                }
                            },
                            _ => {}
                        }
                        pending[start..].reverse();
                        continue;
                    }
                };
                ready.push(Completed {
                    source: node,
                    computation,
                });
            }
            Work::Finish(node, base) => {
                let mut children: Vec<_> = ready.drain(base..).collect();
                if matches!(node, Expr::RecOrder { .. }) && children.len() == 1 {
                    ready.push(Completed {
                        source: node,
                        computation: children.pop().unwrap().computation,
                    });
                    continue;
                }
                if children.iter().all(|child| child.computation.is_none()) {
                    ready.push(Completed {
                        source: node,
                        computation: None,
                    });
                    continue;
                }
                if matches!(node, Expr::FnExpr { .. }) {
                    return Err(Error::type_(
                        node.span(),
                        "recursive value escapes in a function",
                    ));
                }
                let closed = replay
                    .elaborations
                    .rec_quote_source_type(node.site().id)
                    .cloned()
                    .expect("affected source parent has a closed public type");
                let public = public_type(&closed, ambient, replay);
                let span = node.span();
                let name = fresh_name();
                let k = local(name.clone(), span);
                let body = match node {
                    Expr::Let { name, .. } => {
                        let body = children.pop().unwrap().finish(k, replay);
                        children
                            .pop()
                            .unwrap()
                            .resume(name.clone(), body, runtime, ambient, replay)
                    }
                    Expr::Seq { .. } => {
                        let body = children.pop().unwrap().finish(k, replay);
                        children
                            .pop()
                            .unwrap()
                            .resume(fresh_name(), body, runtime, ambient, replay)
                    }
                    Expr::RecOrder { plan, .. } => {
                        let body = children.pop().unwrap().finish(k, replay);
                        match children.pop() {
                            Some(value) => {
                                value.resume(plan.name.clone(), body, runtime, ambient, replay)
                            }
                            None => body,
                        }
                    }
                    Expr::Call { .. } | Expr::Ufcs { bang: None, .. } | Expr::Elaborator { .. } => {
                        replay_children(node, children, k, runtime, ambient, replay)?
                    }
                    _ => {
                        unreachable!("suspension propagation follows ordinary expression children")
                    }
                };
                let computation = lambda(
                    name,
                    arrow(public, runtime.clone(), span),
                    runtime.clone(),
                    body,
                    span,
                );
                ready.push(Completed {
                    source: node,
                    computation: Some(computation),
                });
            }
        }
    }
    Ok(ready.pop().expect("one source result").computation)
}
