//! Native Boolean spellings for the primitive-Boolean representation cohort.
//!
//! Recovery has already removed branch thunks and staged strict operands.
//! These local identities only choose syntax for that conditional. Backends
//! retaining an exact custom Boolean carrier use the original conditional so
//! returning a selected carrier never constructs a replacement literal.

use crate::ast::{Expr, Routed};

#[derive(Clone, Copy)]
pub(crate) enum NativeBoolean<'a> {
    And(&'a Expr<Routed>),
    Or(&'a Expr<Routed>),
    Not,
}

/// Classify a checked conditional when the backend represents both its
/// condition and Boolean result as the host's primitive Boolean. Branches
/// contain already-recovered computations: a returned condition must be an
/// identical bound value, never merely an expression with equal syntax.
pub(crate) fn native_boolean<'a>(
    condition: &Expr<Routed>,
    then_branch: &'a Expr<Routed>,
    else_branch: &'a Expr<Routed>,
) -> Option<NativeBoolean<'a>> {
    if matches!(then_branch, Expr::BoolLit { value: false, .. })
        && matches!(else_branch, Expr::BoolLit { value: true, .. })
    {
        return Some(NativeBoolean::Not);
    }
    if same_bound_value(condition, else_branch)
        || matches!(else_branch, Expr::BoolLit { value: false, .. })
    {
        return Some(NativeBoolean::And(then_branch));
    }
    if same_bound_value(condition, then_branch)
        || matches!(then_branch, Expr::BoolLit { value: true, .. })
    {
        return Some(NativeBoolean::Or(else_branch));
    }
    None
}

fn same_bound_value(left: &Expr<Routed>, right: &Expr<Routed>) -> bool {
    matches!((left, right),
        (Expr::LowBoundRef { name: left, .. }, Expr::LowBoundRef { name: right, .. })
            if left == right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Meta;
    use crate::span::Span;

    fn bound(name: &str) -> Expr<Routed> {
        Expr::LowBoundRef {
            occurrence: Default::default(),
            name: name.to_owned(),
            meta: Meta::new(Span::new(0, 0)),
            ext: (),
        }
    }

    #[test]
    fn exact_bound_condition_selects_short_circuit_syntax() {
        let left = bound("left");
        let right = bound("right");
        assert!(matches!(
            native_boolean(&left, &right, &left),
            Some(NativeBoolean::And(_))
        ));
        assert!(matches!(
            native_boolean(&left, &left, &right),
            Some(NativeBoolean::Or(_))
        ));
        assert!(native_boolean(&left, &right, &bound("other")).is_none());
    }

    #[test]
    fn repeated_computation_is_not_a_reusable_condition() {
        let computed = Expr::LowClosureCall {
            occurrence: Default::default(),
            name: "compute".to_owned(),
            type_args: Vec::new(),
            args: Vec::new(),
            meta: Meta::new(Span::new(0, 0)),
            ext: (),
        };
        assert!(native_boolean(&computed, &bound("right"), &computed).is_none());
        assert!(native_boolean(&computed, &computed, &bound("right")).is_none());
    }
}
