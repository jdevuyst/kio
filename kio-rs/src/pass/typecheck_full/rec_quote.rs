//! Invocation-local source facts for recursive quotation.
//!
//! The source inventory selects exact checked occurrences, not source positions.
//! Pending types live in the ordinary publication transaction; this table receives
//! only its closed output. Runtime composition therefore cannot read live goals.

use std::collections::{HashMap, HashSet};

use super::{Elaborations, InternedType};
use crate::ast::{
    CallArg, ElaboratorCall, Expr, ExpressionOccurrenceId, Lowered, NodeId, RecQuotePlan,
};

#[derive(Debug, Default)]
pub(super) struct RecQuoteCaptures {
    sources: HashSet<ExpressionOccurrenceId>,
    calls: HashMap<NodeId, (ExpressionOccurrenceId, ExpressionOccurrenceId)>,
    completed: HashMap<ExpressionOccurrenceId, InternedType>,
    expansions: HashMap<ExpressionOccurrenceId, ClosedRecQuoteExpansion>,
    computations: HashMap<NodeId, crate::ast::Expr<crate::pass::substitute::PrePrime>>,
}

#[derive(Debug)]
pub(crate) struct ClosedRecQuoteExpansion {
    pub(crate) node_id: NodeId,
    pub(crate) continuation: Expr<Lowered>,
    pub(crate) public_type: InternedType,
    pub(crate) runtime_type: InternedType,
    pub(crate) ambient_type_names: HashSet<String>,
}

/// Invocation-local authority for escape classification. The shared template
/// cache contains only the public recipe; this record survives terminal
/// adaptation solely until this invocation's common replay boundary.
#[derive(Debug)]
pub(crate) struct QuotedPublicRecipe {
    pub(crate) checked: crate::normalization::EvalRef<crate::normalization::CheckedTerm>,
    pub(crate) terminal_adapted: bool,
}

impl RecQuoteCaptures {
    pub(super) fn merge(&mut self, other: Self) {
        self.sources.extend(other.sources);
        self.calls.extend(other.calls);
        self.completed.extend(other.completed);
        self.expansions.extend(other.expansions);
        self.computations.extend(other.computations);
    }
}

impl Elaborations {
    pub(crate) fn register_rec_quote_sources(&mut self, source: &Expr<Lowered>) {
        let captures = self.rec_quote.get_or_insert_with(Default::default);
        let root = source.site().id;
        let mut pending = vec![source];
        while let Some(source) = pending.pop() {
            if captures.sources.insert(source.site().id) {
                match source {
                    Expr::UserElaborator { ext, .. }
                    | Expr::Ufcs {
                        bang: Some(_), ext, ..
                    } => {
                        captures.calls.insert(*ext, (source.site().id, root));
                    }
                    _ => {}
                }
                push_source_children(source, &mut pending);
            }
        }
    }

    pub(crate) fn captures_rec_quote_source(&self, id: ExpressionOccurrenceId) -> bool {
        self.rec_quote
            .as_ref()
            .is_some_and(|captures| captures.sources.contains(&id))
    }

    pub(crate) fn captures_rec_quote_expression(&self, source: &Expr<Lowered>) -> bool {
        self.rec_quote
            .as_ref()
            .is_some_and(|captures| captures.sources.contains(&source.site().id))
    }

    pub(super) fn record_rec_quote_source_type(
        &mut self,
        id: ExpressionOccurrenceId,
        ty: InternedType,
    ) {
        let captures = self
            .rec_quote
            .as_mut()
            .expect("quotation publication has a registered source inventory");
        assert!(
            captures.sources.contains(&id),
            "quotation publication changed source occurrence"
        );
        assert!(
            !crate::pass::typecheck_core::type_contains_goal(ty.as_type()),
            "quotation publication retained an open goal"
        );
        captures.completed.insert(id, ty);
    }

    pub(crate) fn rec_quote_source_type(
        &self,
        id: ExpressionOccurrenceId,
    ) -> Option<&InternedType> {
        self.rec_quote.as_ref()?.completed.get(&id)
    }

    pub(super) fn record_rec_quote_expansion(
        &mut self,
        source: ExpressionOccurrenceId,
        expansion: ClosedRecQuoteExpansion,
    ) {
        let captures = self
            .rec_quote
            .as_mut()
            .expect("quotation expansion has a registered source inventory");
        captures.expansions.insert(source, expansion);
    }

    pub(crate) fn rec_quote_expansion(
        &self,
        source: ExpressionOccurrenceId,
    ) -> Option<&ClosedRecQuoteExpansion> {
        self.rec_quote.as_ref()?.expansions.get(&source)
    }

    pub(super) fn rec_quote_call(
        &self,
        node: NodeId,
    ) -> Option<(ExpressionOccurrenceId, &ClosedRecQuoteExpansion)> {
        let captures = self.rec_quote.as_ref()?;
        let (source, root) = captures.calls.get(&node)?;
        Some((
            *source,
            captures
                .expansions
                .get(root)
                .expect("quoted call owner has closed"),
        ))
    }

    pub(super) fn record_rec_quote_computation(
        &mut self,
        node: NodeId,
        computation: crate::ast::Expr<crate::pass::substitute::PrePrime>,
    ) {
        self.rec_quote
            .as_mut()
            .expect("registered quoted call")
            .computations
            .insert(node, computation);
    }

    pub(super) fn rec_quote_computation(
        &self,
        node: NodeId,
    ) -> Option<&crate::ast::Expr<crate::pass::substitute::PrePrime>> {
        self.rec_quote.as_ref()?.computations.get(&node)
    }
}

pub(super) fn finish_expansions(
    elaborations: &mut Elaborations,
    env: &crate::pass::typecheck_core::ModuleEnv<'_, Lowered>,
) -> Result<(), crate::error::Error> {
    if elaborations.rec_quote.is_none() {
        return Ok(());
    }
    let mut replay = super::UserElaboratorReplayEnv {
        module_path: &env.module_path,
        module: env.module,
        elaborations,
        package: env.package,
        memo: &env.memo,
    };
    for definition in env.fn_defs.values() {
        finish_source_expansions(&definition.body, &mut replay)?;
    }
    Ok(())
}

/// A completed quoted expression can occur inside an otherwise ordinary
/// elaborator operand. Consume its local carrier before ordinary substitution.
pub(super) fn finish_source_expansions(
    source: &Expr<Lowered>,
    replay: &mut super::UserElaboratorReplayEnv<'_>,
) -> Result<(), crate::error::Error> {
    if replay.elaborations.rec_quote.is_none() {
        return Ok(());
    }
    let mut pending = vec![source];
    let mut expansions = Vec::new();
    while let Some(source) = pending.pop() {
        match source {
            Expr::RecQuote { plan, .. } => match plan.as_ref() {
                RecQuotePlan::Expansion {
                    value,
                    continuation,
                    ..
                } => {
                    expansions.push(source);
                    pending.push(value);
                    pending.push(continuation);
                }
                RecQuotePlan::Operand { computation, .. } => pending.push(computation),
            },
            _ => push_source_children(source, &mut pending),
        }
    }
    for source in expansions.into_iter().rev() {
        finish_expansion(source, replay)?;
    }
    Ok(())
}

pub(super) fn finish_expansion(
    carrier: &Expr<Lowered>,
    replay: &mut super::UserElaboratorReplayEnv<'_>,
) -> Result<(), crate::error::Error> {
    if replay
        .elaborations
        .elaboration_for(replay.module_path, carrier)
        .is_some()
    {
        return Ok(());
    }
    let Expr::RecQuote {
        plan, ext: node, ..
    } = carrier
    else {
        unreachable!("expansion carrier")
    };
    let RecQuotePlan::Expansion { value: source, .. } = plan.as_ref() else {
        unreachable!("expansion plan")
    };
    let node = *node;
    let span = carrier.span();
    let closed = replay
        .elaborations
        .rec_quote_expansion(source.site().id)
        .expect("completed expansion has its owner-close publication");
    assert_eq!(
        closed.node_id, node,
        "expansion identity survived source staging"
    );
    let runtime = closed.runtime_type.clone();
    let public = closed.public_type.clone();
    let continuation = closed.continuation.clone();
    let ambient = closed.ambient_type_names.clone();
    let runtime = super::rec_quote_source::public_type(&runtime, &ambient, replay);
    let computation = super::rec_quote_source::compose(source, &runtime, &ambient, replay)?;
    let continuation = crate::pass::substitute::substitute_expr_for_replayed_elaboration(
        &continuation,
        replay.elaborations,
        replay.module_path,
        replay.package,
        replay.module,
    );
    let body = match computation {
        Some(computation) => super::rec_quote_source::call(computation, continuation, span),
        None => {
            let value = crate::pass::substitute::substitute_expr_for_replayed_elaboration(
                source,
                replay.elaborations,
                replay.module_path,
                replay.package,
                replay.module,
            );
            super::rec_quote_source::call(continuation, value, span)
        }
    };
    assert!(!crate::pass::typecheck_core::type_contains_goal(
        public.as_type()
    ));
    let body = crate::pass::substitute::finish_replayed_elaboration(body, &ambient);
    replay
        .elaborations
        .record_unchecked_prime_elaboration(replay.module_path, node, body);
    Ok(())
}

/// Public source edges only. A suspension's computation and an expansion's
/// continuation have their own ordinary checking, not quoted value identities.
fn push_source_children<'a>(source: &'a Expr<Lowered>, pending: &mut Vec<&'a Expr<Lowered>>) {
    match source {
        Expr::Call { callee, args, .. } => {
            pending.push(callee);
            push_arguments(args, pending);
        }
        Expr::FnExpr { body, .. } => pending.push(body),
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            pending.push(value);
            pending.push(body);
        }
        Expr::Elaborator { call, .. } => match call {
            ElaboratorCall::FieldAccess { receiver, .. } => pending.push(receiver),
            ElaboratorCall::FieldUpdate { receiver, updates } => {
                pending.push(receiver);
                pending.extend(updates.iter().map(|update| &update.value));
            }
        },
        Expr::RecOrder { plan, .. } => {
            pending.push(&plan.value);
            pending.push(&plan.body);
        }
        Expr::RecQuote { .. } => {}
        Expr::UserElaborator { args, .. } => push_arguments(args, pending),
        Expr::Ufcs { receiver, args, .. } => {
            pending.push(receiver);
            push_arguments(args, pending);
        }
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::BlockCall { ext, .. }
        | Expr::RecCall { ext, .. }
        | Expr::RowLet { ext, .. }
        | Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::OpChain { ext, .. }
        | Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. }
        | Expr::LowHostCall { ext, .. }
        | Expr::LowModuleCall { ext, .. }
        | Expr::LowQualifiedModuleCall { ext, .. }
        | Expr::LowQualifiedNewtypeMember { ext, .. }
        | Expr::LowNewtypeCtor { ext, .. }
        | Expr::LowNewtypeProj { ext, .. }
        | Expr::LowClosureCall { ext, .. }
        | Expr::LowIndirectCall { ext, .. }
        | Expr::LowTypeApplication { ext, .. }
        | Expr::LowAbsurdCall { ext, .. }
        | Expr::LowCpsProjectorApply { ext, .. }
        | Expr::LowBoundRef { ext, .. }
        | Expr::LowHostFnValueRef { ext, .. }
        | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
    }
}

fn push_arguments<'a>(args: &'a [CallArg<Lowered>], pending: &mut Vec<&'a Expr<Lowered>>) {
    pending.extend(args.iter().filter_map(|argument| match argument {
        CallArg::Value(value) => Some(value),
        CallArg::Type(_) => None,
    }));
}
