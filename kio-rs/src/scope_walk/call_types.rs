#[cfg(feature = "repl-core")]
use crate::ast::Expr;
use crate::ast::{Lowered, Type};
use crate::pass::typecheck_core::{AliasCtx, unfold_and_qualify};

/// The first written argument is selected by the resolved public callee head.
/// Later arguments require a resolved residual type: a written value can fill
/// several product slots, and omitted type arguments require inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallHead {
    Unknown,
    Value,
    TypeOrValue,
}

pub(crate) fn public_call_head(
    ty: &Type<Lowered>,
    aliases: &AliasCtx<'_, '_, Lowered>,
) -> CallHead {
    match unfold_and_qualify(ty, aliases) {
        Type::Function { .. } => CallHead::Value,
        Type::Forall { .. } => CallHead::TypeOrValue,
        _ => CallHead::Unknown,
    }
}

#[cfg(feature = "repl-core")]
pub(crate) fn expression_callee_shadowed(
    probe: &crate::pass::parser::ToolingProbe<'_>,
    callee: &Expr,
) -> bool {
    let Expr::Path { segments, .. } = callee else {
        return true;
    };
    let Some(head) = segments.first() else {
        return true;
    };
    let namespace = if crate::naming::is_type_reference_name(head.as_str()) {
        super::Namespace::Type
    } else {
        super::Namespace::Value
    };
    super::tooling::namespace_candidates(probe, head.span.start, namespace)
        .iter()
        .any(|candidate| candidate.label == head.name)
}
