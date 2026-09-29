use super::*;

pub(super) struct Callback<'code> {
    pub source: callback::Source<'code>,
    pub finish: Finish,
}

pub(super) enum Finish {
    Either {
        left_ty: EvalRef<EvalAstType>,
        right_ty: EvalRef<EvalAstType>,
        result_ty: EvalRef<EvalAstType>,
        value: EvalRef<CheckedTerm>,
        left_name: String,
        left_body: Option<EvalRef<CheckedTerm>>,
        right_name: Option<String>,
    },
    Conditional {
        direct_type: Option<EvalRef<EvalAstType>>,
        projected: bool,
        left: Option<Value>,
    },
    #[cfg(feature = "surface")]
    ProjectedEither {
        prepared: Box<crate::pass::typecheck_core::apply::fills::ProjectedEitherPrepared>,
        right: Option<Value>,
        left: Option<Value>,
    },
}

pub(super) enum Step {
    Next(Finish, Option<Value>),
    Done(Option<Value>),
}

pub(super) fn prepare(
    source: &callback::Source<'_>,
    ctx: &EvalCtx<'_>,
) -> Option<(Finish, EvalArgs)> {
    let args = source.runtime();
    let header = if source.builtin == ComptimeBuiltin::IntrinsicEither {
        3
    } else {
        2
    };
    if args.len() != header + if source.code.is_some() { 0 } else { 2 } {
        return None;
    }
    #[cfg(feature = "surface")]
    if source
        .args
        .iter()
        .any(|arg| matches!(arg, Value::Projected(_)))
    {
        let proof = source.proof()?;
        if source.builtin == ComptimeBuiltin::IntrinsicEither {
            let left_name = fresh_checked_term_value_name("left");
            let right_name = fresh_checked_term_value_name("right");
            let (prepared, left, right) =
                crate::pass::typecheck_core::apply::fills::projected_intrinsic_either_prepare(
                    proof, &args[0], &args[1], &args[2], left_name, right_name,
                )
                .ok()?;
            return Some((
                Finish::ProjectedEither {
                    prepared: Box::new(prepared),
                    right: Some(projected_value(right)),
                    left: None,
                },
                EvalArgs::One([projected_value(left)]),
            ));
        }
        return Some((
            Finish::Conditional {
                direct_type: None,
                projected: true,
                left: None,
            },
            EvalArgs::Empty,
        ));
    }
    if source.builtin == ComptimeBuiltin::IntrinsicIfThenElse {
        let direct_type = if source.code.is_some() {
            let typ = canonical_refl_type(&args[0], ctx)?;
            as_checked_term_ref(&args[1])?;
            Some(EvalRef::new(typ))
        } else {
            None
        };
        return Some((
            Finish::Conditional {
                direct_type,
                projected: false,
                left: None,
            },
            EvalArgs::Empty,
        ));
    }
    let sum_type = canonical_refl_type(&args[0], ctx)?;
    let Type::Sum { left, right, .. } = unfold_canonical_refl_type(&sum_type, ctx) else {
        return None;
    };
    let result_ty = canonical_refl_type(&args[1], ctx)?;
    let value = as_checked_term(&args[2])?;
    if !checked_type_equiv(&value, &sum_type, ctx) {
        return None;
    }
    let left_name = fresh_checked_term_value_name("left");
    let payload = checked_term_local_value(left_name.clone(), (*left).clone());
    Some((
        Finish::Either {
            left_ty: EvalRef::new(*left),
            right_ty: EvalRef::new(*right),
            result_ty: EvalRef::new(result_ty),
            value,
            left_name,
            left_body: None,
            right_name: None,
        },
        EvalArgs::One([payload]),
    ))
}

impl Finish {
    pub fn resume(self, source: &callback::Source<'_>, body: Value, ctx: &EvalCtx<'_>) -> Step {
        match self {
            Finish::Either {
                left_ty,
                right_ty,
                result_ty,
                value,
                left_name,
                left_body,
                right_name,
            } => {
                let Some(body) = as_checked_term(&body) else {
                    return Step::Done(None);
                };
                if !checked_type_equiv(&body, &result_ty, ctx) {
                    return Step::Done(None);
                }
                if let Some(left_body) = left_body {
                    let result_ty = result_ty.unwrap_or_clone();
                    Step::Done(Some(checked_term_node_value(
                        CheckedTermNode::IntrinsicEither {
                            left_ty: left_ty.unwrap_or_clone(),
                            right_ty: right_ty.unwrap_or_clone(),
                            result_ty: result_ty.clone(),
                            value,
                            left_name,
                            left_body,
                            right_name: right_name.expect("prepared right binder"),
                            right_body: body,
                        },
                        result_ty,
                    )))
                } else {
                    let right_name = fresh_checked_term_value_name("right");
                    let payload =
                        checked_term_local_value(right_name.clone(), right_ty.as_ref().clone());
                    Step::Next(
                        Finish::Either {
                            left_ty,
                            right_ty,
                            result_ty,
                            value,
                            left_name,
                            left_body: Some(body),
                            right_name: Some(right_name),
                        },
                        Some(payload),
                    )
                }
            }
            Finish::Conditional {
                direct_type,
                projected,
                left: None,
            } => {
                if source.code.is_some() && as_checked_term_ref(&body).is_none() {
                    return Step::Done(None);
                }
                Step::Next(
                    Finish::Conditional {
                        direct_type,
                        projected,
                        left: Some(body),
                    },
                    None,
                )
            }
            Finish::Conditional {
                direct_type,
                projected,
                left: Some(left),
            } => Step::Done(finish_conditional(
                source,
                direct_type,
                projected,
                left,
                body,
                ctx,
            )),
            #[cfg(feature = "surface")]
            Finish::ProjectedEither {
                prepared,
                right: Some(right),
                left: None,
            } => Step::Next(
                Finish::ProjectedEither {
                    prepared,
                    right: None,
                    left: Some(body),
                },
                Some(right),
            ),
            #[cfg(feature = "surface")]
            Finish::ProjectedEither {
                prepared,
                right: None,
                left: Some(left),
            } => {
                let value = source.proof().and_then(|proof| {
                    crate::pass::typecheck_core::apply::fills::projected_intrinsic_either_finish(
                        proof, *prepared, &left, &body,
                    )
                    .ok()
                    .map(projected_value)
                });
                Step::Done(value)
            }
            #[cfg(feature = "surface")]
            Finish::ProjectedEither { .. } => {
                unreachable!("projected branch callback stages alternate")
            }
        }
    }
}

fn finish_conditional(
    source: &callback::Source<'_>,
    direct_type: Option<EvalRef<EvalAstType>>,
    projected: bool,
    left: Value,
    right: Value,
    ctx: &EvalCtx<'_>,
) -> Option<Value> {
    let args = source.runtime();
    #[cfg(feature = "surface")]
    if projected
        || (source.code.is_none()
            && (matches!(left, Value::Projected(_)) || matches!(right, Value::Projected(_))))
    {
        let term = crate::pass::typecheck_core::apply::fills::projected_intrinsic_if_then_else(
            source.proof()?,
            &args[0],
            &args[1],
            &left,
            &right,
        )
        .ok()?;
        return Some(projected_value(term));
    }
    #[cfg(not(feature = "surface"))]
    let _ = projected;
    let result_ty = match direct_type {
        Some(typ) => typ.unwrap_or_clone(),
        None => canonical_refl_type(&args[0], ctx)?,
    };
    let condition = as_checked_term(&args[1])?;
    let true_body = as_checked_term(&left)?;
    let false_body = as_checked_term(&right)?;
    if !checked_type_equiv(&true_body, &result_ty, ctx)
        || !checked_type_equiv(&false_body, &result_ty, ctx)
    {
        return None;
    }
    Some(checked_term_node_value(
        CheckedTermNode::IntrinsicIfThenElse {
            result_ty: result_ty.clone(),
            condition,
            true_body,
            false_body,
        },
        result_ty,
    ))
}
