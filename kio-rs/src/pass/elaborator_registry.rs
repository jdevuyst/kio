//! User-elaborator raw-syntax reanchoring.
//!
//! A user elaborator's Kio' implementation is prepared once per elaborator,
//! while each distinct input shape is cached as a typed checked-term template.
//! Structured checked-term nodes reanchor themselves during replay; raw Kio'
//! leaves use this walker to rebase internal helper-binding names and source
//! spans onto the call site.

#[cfg(test)]
use crate::ast::Lowered;
use crate::span::Span;

#[cfg(test)]
type Expr = crate::ast::Expr<Lowered>;

#[cfg(test)]
fn reanchor_elaborator_template(template: &Expr, span: Span) -> Expr {
    reanchor_elaborator_expr(template, span)
}

pub(crate) fn reanchor_elaborator_template_pre_prime(
    template: &crate::ast::Expr<crate::pass::substitute::PrePrime>,
    span: Span,
) -> crate::ast::Expr<crate::pass::substitute::PrePrime> {
    reanchor_elaborator_expr(template, span)
}

fn reanchor_elaborator_expr<P>(expr: &crate::ast::Expr<P>, span: Span) -> crate::ast::Expr<P>
where
    P: crate::ast::Phase<ExprResolved = (), ParamPatternExt = (), FnExprCapabilities = ()> + Clone,
{
    let node_span = elaborator_template_span_at_call(expr.span(), span);
    match expr {
        crate::ast::Expr::Path { segments, ext, .. } => crate::ast::Expr::Path {
            occurrence: Default::default(),
            segments: segments
                .iter()
                .map(|segment| {
                    crate::ast::PathSegment::new(
                        rebase_elaborator_name(&segment.name, node_span),
                        node_span,
                    )
                })
                .collect(),
            meta: crate::ast::Meta::new(node_span),
            ext: *ext,
        },
        crate::ast::Expr::Call {
            callee, args, ext, ..
        } => crate::ast::Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(reanchor_elaborator_expr(callee, span)),
            args: args
                .iter()
                .map(|arg| reanchor_elaborator_call_arg(arg, span))
                .collect(),
            meta: crate::ast::Meta::new(node_span),
            ext: *ext,
        },
        crate::ast::Expr::FnExpr {
            sig,
            ret_ty,
            body,
            caps,
            ..
        } => crate::ast::Expr::FnExpr {
            occurrence: Default::default(),
            sig: reanchor_elaborator_signature(sig, span),
            ret_ty: ret_ty.clone(),
            body: Box::new(reanchor_elaborator_expr(body, span)),
            meta: crate::ast::Meta::new(node_span),
            caps: *caps,
        },
        crate::ast::Expr::Let {
            name,
            name_span,
            ty,
            pattern,
            value: let_value,
            body,
            ..
        } => crate::ast::Expr::Let {
            occurrence: Default::default(),
            name: rebase_elaborator_name(name, node_span),
            name_span: *name_span,
            ty: ty.clone(),
            pattern: *pattern,
            value: Box::new(reanchor_elaborator_expr(let_value, span)),
            body: Box::new(reanchor_elaborator_expr(body, span)),
            meta: crate::ast::Meta::new(node_span),
        },
        crate::ast::Expr::Seq {
            value: seq_value,
            body,
            ..
        } => crate::ast::Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(reanchor_elaborator_expr(seq_value, span)),
            body: Box::new(reanchor_elaborator_expr(body, span)),
            meta: crate::ast::Meta::new(node_span),
        },
        crate::ast::Expr::Unit { .. } => crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: crate::ast::Meta::new(node_span),
        },
        crate::ast::Expr::StrLit {
            value, annotation, ..
        } => crate::ast::Expr::StrLit {
            occurrence: Default::default(),
            value: value.clone(),
            annotation: annotation.clone(),
            meta: crate::ast::Meta::new(node_span),
        },
        crate::ast::Expr::IntLit {
            digits, annotation, ..
        } => crate::ast::Expr::IntLit {
            occurrence: Default::default(),
            digits: digits.clone(),
            annotation: annotation.clone(),
            meta: crate::ast::Meta::new(node_span),
        },
        crate::ast::Expr::FloatLit {
            digits, annotation, ..
        } => crate::ast::Expr::FloatLit {
            occurrence: Default::default(),
            digits: digits.clone(),
            annotation: annotation.clone(),
            meta: crate::ast::Meta::new(node_span),
        },
        crate::ast::Expr::BoolLit {
            value, annotation, ..
        } => crate::ast::Expr::BoolLit {
            occurrence: Default::default(),
            value: *value,
            annotation: annotation.clone(),
            meta: crate::ast::Meta::new(node_span),
        },
        other => other.clone(),
    }
}

pub(crate) fn elaborator_template_span_at_call(template_span: Span, call_span: Span) -> Span {
    Span::new(
        call_span.start,
        call_span.end.saturating_add(template_span.end),
    )
}

fn reanchor_elaborator_call_arg<P>(
    arg: &crate::ast::CallArg<P>,
    span: Span,
) -> crate::ast::CallArg<P>
where
    P: crate::ast::Phase<ExprResolved = (), ParamPatternExt = (), FnExprCapabilities = ()> + Clone,
{
    match arg {
        crate::ast::CallArg::Type(ty) => crate::ast::CallArg::Type(ty.clone()),
        crate::ast::CallArg::Value(expr) => {
            crate::ast::CallArg::Value(reanchor_elaborator_expr(expr, span))
        }
    }
}

fn reanchor_elaborator_signature<P>(
    sig: &crate::ast::Signature<P>,
    span: Span,
) -> crate::ast::Signature<P>
where
    P: crate::ast::Phase<ExprResolved = (), ParamPatternExt = (), FnExprCapabilities = ()> + Clone,
{
    crate::ast::Signature::from_parts(
        sig.params
            .iter()
            .map(|param| match param {
                crate::ast::SignatureParam::Type(tp) => {
                    crate::ast::SignatureParam::Type(tp.clone())
                }
                crate::ast::SignatureParam::Value(vp) => {
                    crate::ast::SignatureParam::Value(crate::ast::Param {
                        name: rebase_elaborator_name(&vp.name, span),
                        ty: vp.ty.clone(),
                        pattern: (),
                        meta: crate::ast::Meta::new(span),
                    })
                }
            })
            .collect(),
        sig.groups.clone(),
    )
}

fn rebase_elaborator_name(name: &str, span: Span) -> String {
    for prefix in ["_align_", "_ease_", "_fit_"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            let mut parts = rest.splitn(3, '_');
            let Some(side) = parts.next() else {
                return name.to_owned();
            };
            let Some(_old_offset) = parts.next() else {
                return name.to_owned();
            };
            let Some(counter) = parts.next() else {
                return name.to_owned();
            };
            return format!(
                "__elab_{}_{side}_s{}_{counter}__",
                prefix.trim_matches('_'),
                span.start
            );
        }
    }
    for prefix in ["_spine_one_l_", "_spine_one_r_", "_spine_l_", "_spine_r_"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            let Some((_old_offset, counter)) = rest.split_once('_') else {
                return name.to_owned();
            };
            return format!(
                "__elab_{}_s{}_{counter}__",
                prefix.trim_matches('_'),
                span.start
            );
        }
    }
    for prefix in ["_flat_in_", "_flat_l1_", "_flat_l2_", "_flat_r_"] {
        if name.strip_prefix(prefix).is_some() {
            return format!("__elab_{}_s{}__", prefix.trim_matches('_'), span.start);
        }
    }
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_reanchoring_preserves_explicit_empty_value_group() {
        let template = Expr::FnExpr {
            occurrence: Default::default(),
            sig: crate::ast::Signature::from_groups(vec![crate::ast::SignatureGroup::Value(
                Vec::new(),
            )]),
            ret_ty: None,
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: crate::ast::Meta::new(Span::new(0, 0)),
            }),
            meta: crate::ast::Meta::new(Span::new(0, 0)),
            caps: (),
        };

        let instantiated = reanchor_elaborator_template(&template, Span::new(42, 50));
        let Expr::FnExpr { sig, .. } = instantiated else {
            panic!("function template must instantiate to a function expression");
        };

        assert!(sig.params.is_empty());
        assert_eq!(
            sig.groups,
            vec![crate::ast::SignatureGroupKind::Value { len: 0 }]
        );
    }

    #[test]
    fn rebase_elaborator_name_rewrites_algebraic_and_fit_names() {
        let span = Span::new(42, 50);

        assert_eq!(
            rebase_elaborator_name("_align_l_s0_n3", span),
            "__elab_align_l_s42_n3__"
        );
        assert_eq!(
            rebase_elaborator_name("_ease_r_s0_n7", span),
            "__elab_ease_r_s42_n7__"
        );
        assert_eq!(
            rebase_elaborator_name("_fit_l_s0_n2", span),
            "__elab_fit_l_s42_n2__"
        );
    }

    #[test]
    fn rebase_elaborator_name_rewrites_spine_names() {
        let span = Span::new(99, 120);

        assert_eq!(
            rebase_elaborator_name("_spine_l_s0_n1", span),
            "__elab_spine_l_s99_n1__"
        );
        assert_eq!(
            rebase_elaborator_name("_spine_r_s0_n2", span),
            "__elab_spine_r_s99_n2__"
        );
        assert_eq!(
            rebase_elaborator_name("_spine_one_l_s0_n3", span),
            "__elab_spine_one_l_s99_n3__"
        );
        assert_eq!(
            rebase_elaborator_name("_spine_one_r_s0_n4", span),
            "__elab_spine_one_r_s99_n4__"
        );
    }

    #[test]
    fn rebase_elaborator_name_rewrites_flatten_names() {
        let span = Span::new(7, 8);

        assert_eq!(
            rebase_elaborator_name("_flat_in_s0", span),
            "__elab_flat_in_s7__"
        );
        assert_eq!(
            rebase_elaborator_name("_flat_l1_s0", span),
            "__elab_flat_l1_s7__"
        );
        assert_eq!(
            rebase_elaborator_name("_flat_l2_s0", span),
            "__elab_flat_l2_s7__"
        );
        assert_eq!(
            rebase_elaborator_name("_flat_r_s0", span),
            "__elab_flat_r_s7__"
        );
        assert_eq!(rebase_elaborator_name("user_name", span), "user_name");
    }
}
