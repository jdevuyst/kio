use super::*;

pub(super) struct Callback<'code> {
    pub source: callback::Source<'code>,
    pub finish: Finish,
}

pub(super) enum Finish {
    Either {
        left_ty: ReflectedType,
        right_ty: ReflectedType,
        result_ty: ReflectedType,
        value: EvalRef<CheckedTerm>,
        left_name: String,
        left_body: Option<EvalRef<CheckedTerm>>,
        right_name: Option<String>,
    },
    Conditional {
        direct_type: Option<ReflectedType>,
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
            let typ = reflected_type_carrier(&args[0])?.clone();
            as_checked_term_ref(&args[1])?;
            Some(typ)
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
    let sum_type = unfold_reflected_carrier(reflected_type_carrier(&args[0])?, ctx);
    if !matches!(sum_type.as_type(), Type::Sum { .. }) {
        return None;
    }
    let left = sum_type.child(reflection_bindings::TypeChild::Left);
    let right = sum_type.child(reflection_bindings::TypeChild::Right);
    let result_ty = reflected_type_carrier(&args[1])?.clone();
    let value = as_checked_term(&args[2])?;
    if !checked_reflected_type_equiv(&value, &sum_type, source.builtin, ctx) {
        return None;
    }
    let left_name = fresh_checked_term_value_name("left");
    let payload = reflected_local_value(left_name.clone(), left.clone());
    Some((
        Finish::Either {
            left_ty: left,
            right_ty: right,
            result_ty,
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
                if !checked_reflected_type_equiv(&body, &result_ty, source.builtin, ctx) {
                    return Step::Done(None);
                }
                if let Some(left_body) = left_body {
                    Step::Done(reflected_checked_value(
                        ProjectedTermConstruction::Either {
                            left_ty,
                            right_ty,
                            value,
                            left_name,
                            left_body,
                            right_name: right_name.expect("prepared right binder"),
                            right_body: body,
                        },
                        result_ty,
                    ))
                } else {
                    let right_name = fresh_checked_term_value_name("right");
                    let payload = reflected_local_value(right_name.clone(), right_ty.clone());
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
    direct_type: Option<ReflectedType>,
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
        Some(typ) => typ,
        None => reflected_type_carrier(&args[0])?.clone(),
    };
    let condition = as_checked_term(&args[1])?;
    let true_body = as_checked_term(&left)?;
    let false_body = as_checked_term(&right)?;
    if !checked_reflected_type_equiv(&true_body, &result_ty, source.builtin, ctx)
        || !checked_reflected_type_equiv(&false_body, &result_ty, source.builtin, ctx)
    {
        return None;
    }
    reflected_checked_value(
        ProjectedTermConstruction::IfThenElse {
            condition,
            true_body,
            false_body,
        },
        result_ty,
    )
}
