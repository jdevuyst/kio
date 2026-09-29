use super::*;

pub(super) struct Fold<'code> {
    pub step: Handler<'code>,
    pub items: FoldItems,
}

pub(super) enum FoldItems {
    Values(std::vec::IntoIter<Value>),
    Count(usize),
}

impl FoldItems {
    pub fn next(&mut self) -> Option<Option<Value>> {
        match self {
            Self::Values(values) => values.next().map(Some),
            Self::Count(count) => {
                *count = count.checked_sub(1)?;
                Some(None)
            }
        }
    }
}

pub(super) fn type_fold_items(
    builtin: ComptimeBuiltin,
    typ: &Value,
    ctx: &EvalCtx<'_>,
) -> Option<Vec<Value>> {
    let typ = canonical_refl_type(typ, ctx)?;
    let items = match builtin {
        ComptimeBuiltin::TypeProductSpineFold => product_spine(&typ, ctx),
        ComptimeBuiltin::TypeSumSpineFold => sum_spine(&typ, ctx),
        ComptimeBuiltin::TypeArgsFold => type_args(&typ, ctx),
        ComptimeBuiltin::TypeFunctionParamsFold => function_param_types(&typ, ctx),
        _ => unreachable!("compiled type fold carries its extractor"),
    };
    Some(items.into_iter().map(refl_type_value).collect())
}

pub(super) fn primitive_type_fold_items(
    builtin: ComptimeBuiltin,
    args: &[Value],
    ctx: &EvalCtx<'_>,
) -> Option<Vec<Value>> {
    let [typ, _, _] = reflection_runtime_args(builtin, args) else {
        return None;
    };
    #[cfg(feature = "surface")]
    if args.iter().any(|arg| matches!(arg, Value::Projected(_))) {
        let proof = marked_comptime_proof_from_value(args.first()?)?;
        let typ = projected_value_from_value(typ)?;
        let items = match builtin {
            ComptimeBuiltin::TypeProductSpineFold | ComptimeBuiltin::TypeSumSpineFold => {
                projected_spine(proof, typ, builtin == ComptimeBuiltin::TypeProductSpineFold)
                    .ok()?
            }
            ComptimeBuiltin::TypeFunctionParamsFold => {
                projected_function_params(proof, typ).ok()?
            }
            ComptimeBuiltin::TypeArgsFold => {
                crate::pass::typecheck_core::apply::fills::projected_type_arguments(proof, typ)
                    .ok()??
            }
            _ => unreachable!("projected type fold carries its extractor"),
        };
        return Some(items.into_iter().map(projected_value).collect());
    }
    type_fold_items(builtin, typ, ctx)
}

pub(super) fn is_direct(expr: &EvalExpr) -> bool {
    matches!(
        expr,
        EvalExpr::ReflTypeUnit
            | EvalExpr::ReflTypeBottom
            | EvalExpr::ReflTermUnit
            | EvalExpr::ReflTypeFold { .. }
            | EvalExpr::ReflTermLet { .. }
            | EvalExpr::ReflTermFn { .. }
            | EvalExpr::ReflTermTypeFn { .. }
            | EvalExpr::ReflIntrinsicEither { .. }
            | EvalExpr::ReflIntrinsicIfThenElse { .. }
            | EvalExpr::ReflTypeProduct { .. }
            | EvalExpr::ReflTypeSum { .. }
            | EvalExpr::ReflTypeEqual { .. }
            | EvalExpr::ReflTypeNameEqual { .. }
            | EvalExpr::ReflTypeVarEqual { .. }
            | EvalExpr::ReflTypeArityEqual { .. }
            | EvalExpr::ReflTypeView { .. }
            | EvalExpr::ReflTypeInstantiate { .. }
            | EvalExpr::ReflTypeRootPredicate { .. }
            | EvalExpr::ReflTermType { .. }
            | EvalExpr::ReflTermCall { .. }
            | EvalExpr::ReflTermTypeApp { .. }
            | EvalExpr::ReflIntrinsicPair { .. }
            | EvalExpr::ReflIntrinsicProjection { .. }
            | EvalExpr::ReflIntrinsicInjection { .. }
            | EvalExpr::ReflIntrinsicAbsurd { .. }
    )
}

pub(super) fn child(expr: &EvalExpr, index: usize) -> Option<&EvalRef<EvalExpr>> {
    let parts = match expr {
        EvalExpr::ReflTypeUnit | EvalExpr::ReflTypeBottom | EvalExpr::ReflTermUnit => {
            [None, None, None]
        }
        EvalExpr::ReflTypeFold { typ, init, .. } => [Some(typ), Some(init), None],
        EvalExpr::ReflTermLet {
            value_type, value, ..
        } => [Some(value_type), Some(value), None],
        EvalExpr::ReflTermFn { fn_type, .. } => [Some(fn_type), None, None],
        EvalExpr::ReflTermTypeFn { arity, .. } => [Some(arity), None, None],
        EvalExpr::ReflIntrinsicEither {
            sum_type,
            result_type,
            value,
            ..
        } => [Some(sum_type), Some(result_type), Some(value)],
        EvalExpr::ReflIntrinsicIfThenElse {
            result_type,
            condition,
            ..
        } => [Some(result_type), Some(condition), None],
        EvalExpr::ReflTypeProduct { left, right }
        | EvalExpr::ReflTypeSum { left, right }
        | EvalExpr::ReflTypeEqual { left, right }
        | EvalExpr::ReflTypeNameEqual { left, right }
        | EvalExpr::ReflTypeVarEqual { left, right }
        | EvalExpr::ReflTypeArityEqual { left, right }
        | EvalExpr::ReflIntrinsicPair { left, right } => [Some(left), Some(right), None],
        EvalExpr::ReflTypeView { typ } | EvalExpr::ReflTypeRootPredicate { typ, .. } => {
            [Some(typ), None, None]
        }
        EvalExpr::ReflTypeInstantiate { scheme, arg } => [Some(scheme), Some(arg), None],
        EvalExpr::ReflTermType { term } => [Some(term), None, None],
        EvalExpr::ReflTermCall {
            fn_type,
            fn_value,
            arg_packet,
        } => [Some(fn_type), Some(fn_value), Some(arg_packet)],
        EvalExpr::ReflTermTypeApp { fn_value, arg } => [Some(fn_value), Some(arg), None],
        EvalExpr::ReflIntrinsicProjection {
            product_type,
            value,
            ..
        } => [Some(product_type), Some(value), None],
        EvalExpr::ReflIntrinsicInjection {
            sum_type, value, ..
        } => [Some(sum_type), Some(value), None],
        EvalExpr::ReflIntrinsicAbsurd {
            bottom_value,
            result_type,
        } => [Some(bottom_value), Some(result_type), None],
        _ => unreachable!("direct reflection retains its operation"),
    };
    parts.get(index).copied().flatten()
}

pub(super) fn reduce_leaf(expr: &EvalExpr, args: EvalArgs, ctx: &EvalCtx<'_>) -> Value {
    let mut values = args.into_vec().into_iter();
    match expr {
        EvalExpr::ReflTypeUnit => {
            let start = direct_reflection_start(ctx);
            let value = refl_type_value(Type::Unit { meta: zero_meta() });
            record_direct_reflection_success("__type_unit__", ctx, start);
            value
        }
        EvalExpr::ReflTypeBottom => {
            let start = direct_reflection_start(ctx);
            let value = refl_type_value(Type::Bottom { meta: zero_meta() });
            record_direct_reflection_success("__type_bottom__", ctx, start);
            value
        }
        EvalExpr::ReflTypeProduct { left: _, right: _ } => {
            let left = values.next().expect("direct reflection operand");
            let right = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = match (
                canonical_refl_type(&left, ctx),
                canonical_refl_type(&right, ctx),
            ) {
                (Some(left_ty), Some(right_ty)) => Some(refl_type_value(Type::Product {
                    left: Box::new(left_ty),
                    right: Box::new(right_ty),
                    meta: zero_meta(),
                })),
                _ => None,
            };
            finish_direct_reflection("__type_product__", result, vec![left, right], ctx, start)
        }
        EvalExpr::ReflTypeSum { left: _, right: _ } => {
            let left = values.next().expect("direct reflection operand");
            let right = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = match (
                canonical_refl_type(&left, ctx),
                canonical_refl_type(&right, ctx),
            ) {
                (Some(left_ty), Some(right_ty)) => Some(refl_type_value(Type::Sum {
                    left: Box::new(left_ty),
                    right: Box::new(right_ty),
                    meta: zero_meta(),
                })),
                _ => None,
            };
            finish_direct_reflection("__type_sum__", result, vec![left, right], ctx, start)
        }
        EvalExpr::ReflTypeView { typ: _ } => {
            let typ = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            finish_direct_reflection(
                "__type_view__",
                canonical_refl_type(&typ, ctx).and_then(|typ| type_view_value(&typ, ctx)),
                vec![typ],
                ctx,
                start,
            )
        }
        EvalExpr::ReflTypeEqual { left: _, right: _ } => {
            let left = values.next().expect("direct reflection operand");
            let right = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = match (
                canonical_refl_type(&left, ctx),
                canonical_refl_type(&right, ctx),
            ) {
                (Some(left_ty), Some(right_ty)) => Some(predicate_value(
                    canonical_refl_type_equiv(&left_ty, &right_ty, ctx),
                )),
                _ => None,
            };
            finish_direct_reflection("__type_equal__", result, vec![left, right], ctx, start)
        }
        EvalExpr::ReflTypeNameEqual { left: _, right: _ } => {
            let left = values.next().expect("direct reflection operand");
            let right = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = match (&left, &right) {
                (
                    Value::ReflTypeName {
                        segments: left_segments,
                        param_arities: left_arities,
                    },
                    Value::ReflTypeName {
                        segments: right_segments,
                        param_arities: right_arities,
                    },
                ) => Some(predicate_value(
                    left_segments == right_segments && left_arities == right_arities,
                )),
                _ => None,
            };
            finish_direct_reflection("__type_name_equal__", result, vec![left, right], ctx, start)
        }
        EvalExpr::ReflTypeVarEqual { left: _, right: _ } => {
            let left = values.next().expect("direct reflection operand");
            let right = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = match (&left, &right) {
                (
                    Value::ReflTypeVar {
                        name: left_name,
                        arity: left_arity,
                    },
                    Value::ReflTypeVar {
                        name: right_name,
                        arity: right_arity,
                    },
                ) => Some(predicate_value(
                    left_name == right_name && left_arity == right_arity,
                )),
                _ => None,
            };
            finish_direct_reflection("__type_var_equal__", result, vec![left, right], ctx, start)
        }
        EvalExpr::ReflTypeArityEqual { left: _, right: _ } => {
            let left = values.next().expect("direct reflection operand");
            let right = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = match (as_refl_type_arity(&left), as_refl_type_arity(&right)) {
                (Some(left), Some(right)) => Some(predicate_value(left == right)),
                _ => None,
            };
            finish_direct_reflection(
                "__type_arity_equal__",
                result,
                vec![left, right],
                ctx,
                start,
            )
        }
        EvalExpr::ReflTypeInstantiate { scheme: _, arg: _ } => {
            let scheme = values.next().expect("direct reflection operand");
            let arg = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = match (
                canonical_refl_type(&scheme, ctx),
                canonical_refl_type(&arg, ctx),
            ) {
                (Some(scheme_ty), Some(arg_ty)) => {
                    Some(type_instantiate_value(scheme_ty, arg_ty, ctx))
                }
                _ => None,
            };
            finish_direct_reflection(
                "__type_instantiate__",
                result,
                vec![scheme, arg],
                ctx,
                start,
            )
        }
        EvalExpr::ReflTypeRootPredicate { name, typ: _ } => {
            let typ = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = canonical_refl_type(&typ, ctx).map(|typ| {
                let unfolded = unfold_canonical_refl_type(&typ, ctx);
                predicate_value(if *name == "__type_is_product_root__" {
                    matches!(unfolded, Type::Product { .. })
                } else {
                    matches!(unfolded, Type::Sum { .. })
                })
            });
            finish_direct_reflection(name, result, vec![typ], ctx, start)
        }
        EvalExpr::ReflTermType { term: _ } => {
            let term = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            finish_direct_reflection(
                "__term_type__",
                reflection_term_type_value(&term, ctx),
                vec![term],
                ctx,
                start,
            )
        }
        EvalExpr::ReflTermUnit => {
            let start = direct_reflection_start(ctx);
            let value = reflection_term_unit_value();
            record_direct_reflection_success("__term_unit__", ctx, start);
            value
        }
        EvalExpr::ReflTermCall {
            fn_type: _,
            fn_value: _,
            arg_packet: _,
        } => {
            let fn_type = values.next().expect("direct reflection operand");
            let fn_value = values.next().expect("direct reflection operand");
            let arg_packet = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            match reflection_term_call_value_owned(fn_type, fn_value, arg_packet, ctx) {
                Ok(value) => {
                    record_direct_reflection_success("__term_call__", ctx, start);
                    value
                }
                Err(args) => {
                    Value::Stuck(EvalRef::new(primitive_value("__term_call__")), args.into())
                }
            }
        }
        EvalExpr::ReflTermTypeApp {
            fn_value: _,
            arg: _,
        } => {
            let fn_value = values.next().expect("direct reflection operand");
            let arg = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            match reflection_term_type_app_value_owned(fn_value, arg, ctx) {
                Ok(value) => {
                    record_direct_reflection_success("__term_type_app__", ctx, start);
                    value
                }
                Err(args) => Value::Stuck(
                    EvalRef::new(primitive_value("__term_type_app__")),
                    args.into(),
                ),
            }
        }
        EvalExpr::ReflIntrinsicPair { left: _, right: _ } => {
            let left = values.next().expect("direct reflection operand");
            let right = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            match reflection_intrinsic_pair_value_owned(left, right) {
                Ok(value) => {
                    record_direct_reflection_success("__intrinsic_pair__", ctx, start);
                    value
                }
                Err(args) => Value::Stuck(
                    EvalRef::new(primitive_value("__intrinsic_pair__")),
                    args.into(),
                ),
            }
        }
        EvalExpr::ReflIntrinsicProjection {
            name,
            product_type: _,
            value: _,
        } => {
            let product_type = values.next().expect("direct reflection operand");
            let value = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            match reflection_intrinsic_projection_value_owned(name, product_type, value, ctx) {
                Ok(value) => {
                    record_direct_reflection_success(name, ctx, start);
                    value
                }
                Err(args) => Value::Stuck(EvalRef::new(primitive_value(name)), args.into()),
            }
        }
        EvalExpr::ReflIntrinsicInjection {
            name,
            sum_type: _,
            value: _,
        } => {
            let sum_type = values.next().expect("direct reflection operand");
            let value = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            match reflection_intrinsic_injection_value_owned(name, sum_type, value, ctx) {
                Ok(value) => {
                    record_direct_reflection_success(name, ctx, start);
                    value
                }
                Err(args) => Value::Stuck(EvalRef::new(primitive_value(name)), args.into()),
            }
        }
        EvalExpr::ReflIntrinsicAbsurd {
            bottom_value: _,
            result_type: _,
        } => {
            let bottom_value = values.next().expect("direct reflection operand");
            let result_type = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            match reflection_intrinsic_absurd_value_owned(bottom_value, result_type, ctx) {
                Ok(value) => {
                    record_direct_reflection_success("__intrinsic_absurd__", ctx, start);
                    value
                }
                Err(args) => Value::Stuck(
                    EvalRef::new(primitive_value("__intrinsic_absurd__")),
                    args.into(),
                ),
            }
        }
        _ => unreachable!("nonexecuting reflection operation"),
    }
}
