//! Tail contexts over checked, routed expressions.
//!
//! These borrowed cursors add no node identity or persistent phase state. A
//! result edge preserves its enclosing return context; constructing a callable
//! does not execute its body. Emission adapters do not change these semantic
//! edges: an adapter on a tail application must preserve its transfer.

use crate::ast::{Expr, Routed};

/// The last executable application in a routed call. Type application is an
/// execution boundary even when its result is another callable value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TailStage {
    Value,
    Type,
}

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    NonTail,
    Tail,
}

/// Only these expression edges deliver the enclosing result unchanged. The
/// function-body edge starts a separate invocation rather than inheriting one.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ResultChild {
    LetBody,
    SeqBody,
    ThenBranch,
    ElseBranch,
    MatchArm(usize),
    FunctionBody,
}

#[derive(Clone, Copy)]
pub(crate) struct TailContext<'a> {
    expr: &'a Expr<Routed>,
    position: Position,
}

impl<'a> TailContext<'a> {
    /// Start the executing body of a declaration or callable stage.
    pub(crate) fn body(expr: &'a Expr<Routed>) -> Self {
        Self {
            expr,
            position: Position::Tail,
        }
    }

    /// Callees, arguments, initializers, conditions, receivers, and constructed
    /// payloads must finish before their enclosing operation can continue.
    pub(crate) fn operand(expr: &'a Expr<Routed>) -> Self {
        Self {
            expr,
            position: Position::NonTail,
        }
    }

    pub(crate) fn expr(self) -> &'a Expr<Routed> {
        self.expr
    }

    /// Whether this expression supplies the invocation's result, including a
    /// non-call leaf that completes without transferring to another callable.
    pub(crate) fn is_tail_position(self) -> bool {
        self.position == Position::Tail
    }

    /// Select the actual child, so callers cannot attach the enclosing result
    /// context to an argument or initializer by supplying an arbitrary node.
    pub(crate) fn result_child(self, edge: ResultChild) -> Self {
        let expr = match (self.expr, edge) {
            (Expr::Let { body, .. }, ResultChild::LetBody)
            | (Expr::Seq { body, .. }, ResultChild::SeqBody) => body,
            (Expr::EnrichedConditional { then_branch, .. }, ResultChild::ThenBranch) => then_branch,
            (Expr::EnrichedConditional { else_branch, .. }, ResultChild::ElseBranch) => else_branch,
            (Expr::EnrichedMatch { arms, .. }, ResultChild::MatchArm(index)) => {
                &arms
                    .get(index)
                    .expect("a routed tail edge selects an existing match arm")
                    .body
            }
            (Expr::FnExpr { body, .. }, ResultChild::FunctionBody) => return Self::body(body),
            _ => unreachable!("a routed tail edge must match its expression owner"),
        };
        Self {
            expr,
            position: self.position,
        }
    }

    /// The final application eligible for a resumable guest transfer. A known
    /// host call returns normally at its opaque boundary; an unknown callable
    /// may carry either a guest entry or a host implementation. The callee,
    /// arguments and earlier type stages execute in their ordinary order.
    pub(crate) fn tail_stage(self) -> Option<TailStage> {
        if !self.is_tail_position() {
            return None;
        }
        match self.expr {
            Expr::LowModuleCall { sig, type_args, .. }
            | Expr::LowQualifiedModuleCall { sig, type_args, .. }
                if sig.has_type_params() && !sig.has_value_group() =>
            {
                (!type_args.is_empty()).then_some(TailStage::Type)
            }
            Expr::LowTypeApplication { .. } => Some(TailStage::Type),
            Expr::LowModuleCall { .. }
            | Expr::LowQualifiedModuleCall { .. }
            | Expr::LowClosureCall { .. }
            | Expr::LowIndirectCall { .. }
            | Expr::LowCpsProjectorApply { .. } => Some(TailStage::Value),
            _ => None,
        }
    }

    /// Whether this invocation can reach a resumable guest application.
    /// Statically resolved host calls finish at their opaque host boundary;
    /// an unknown callable may instead carry a guest entry. Nested callable
    /// bodies are separate roots, and operands cannot transfer this result.
    /// A plain statement spine needs no allocation; only branching adds work
    /// to the local pending stack.
    pub(crate) fn has_tail_transfer(self) -> bool {
        if !self.is_tail_position() {
            return false;
        }
        let mut current = self;
        let mut pending = Vec::new();
        loop {
            if current.tail_stage().is_some() {
                return true;
            }
            match current.expr {
                Expr::Let { .. } => current = current.result_child(ResultChild::LetBody),
                Expr::Seq { .. } => current = current.result_child(ResultChild::SeqBody),
                Expr::EnrichedConditional { .. } => {
                    pending.push(current.result_child(ResultChild::ElseBranch));
                    current = current.result_child(ResultChild::ThenBranch);
                }
                Expr::EnrichedMatch { arms, .. } if !arms.is_empty() => {
                    pending.extend(
                        (1..arms.len())
                            .map(|index| current.result_child(ResultChild::MatchArm(index))),
                    );
                    current = current.result_child(ResultChild::MatchArm(0));
                }
                _ => match pending.pop() {
                    Some(next) => current = next,
                    None => return false,
                },
            }
        }
    }
}
