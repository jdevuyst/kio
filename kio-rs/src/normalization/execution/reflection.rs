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

pub(super) struct TypeFold {
    pub items: Vec<Value>,
    pub remainder: Option<Value>,
}

pub(super) fn type_fold_items(
    builtin: ComptimeBuiltin,
    typ: &Value,
    ctx: &EvalCtx<'_>,
) -> Option<TypeFold> {
    use reflection_bindings::TypeChild;
    let mut cursor = reflected_type_carrier(typ)?.clone();
    if !neutral_type_observation_is_known(builtin, &cursor, ctx) {
        return None;
    }
    let mut items = Vec::new();
    loop {
        cursor = unfold_reflected_carrier(&cursor, ctx);
        match (builtin, cursor.as_type()) {
            (ComptimeBuiltin::TypeForallPrefixFold, Type::Forall { .. }) => {
                let (variable, body) = cursor.open_forall().expect("known forall head");
                items.push(Value::ReflTypeVar(variable));
                cursor = body;
            }
            (ComptimeBuiltin::TypeForallPrefixFold, Type::Infer { .. }) => return None,
            (ComptimeBuiltin::TypeForallPrefixFold, _) => {
                return Some(TypeFold {
                    items,
                    remainder: Some(Value::ReflType(cursor)),
                });
            }
            (ComptimeBuiltin::TypeProductSpineFold, Type::Product { .. })
            | (ComptimeBuiltin::TypeSumSpineFold, Type::Sum { .. }) => {
                items.push(Value::ReflType(cursor.child(TypeChild::Left)));
                cursor = cursor.child(TypeChild::Right);
            }
            (ComptimeBuiltin::TypeProductSpineFold | ComptimeBuiltin::TypeSumSpineFold, _) => {
                items.push(Value::ReflType(cursor));
                return Some(TypeFold {
                    items,
                    remainder: None,
                });
            }
            (ComptimeBuiltin::TypeArgsFold, Type::Path { args, .. }) => {
                items.extend(
                    (0..args.len())
                        .map(|index| Value::ReflType(cursor.child(TypeChild::Argument(index)))),
                );
                return Some(TypeFold {
                    items,
                    remainder: None,
                });
            }
            (ComptimeBuiltin::TypeArgsFold, _) => {
                return Some(TypeFold {
                    items,
                    remainder: None,
                });
            }
            _ => unreachable!("compiled type fold carries its extractor"),
        }
    }
}

pub(super) fn primitive_type_fold_items(
    builtin: ComptimeBuiltin,
    args: &[Value],
    ctx: &EvalCtx<'_>,
) -> Option<TypeFold> {
    let [typ, _, _] = reflection_runtime_args(builtin, args) else {
        return None;
    };
    #[cfg(feature = "surface")]
    if let Value::Projected(typ) = typ {
        let proof = marked_comptime_proof_from_value(args.first()?)?;
        if builtin == ComptimeBuiltin::TypeForallPrefixFold {
            use crate::pass::typecheck_core::apply::fills::{
                ProjectedTypeView, projected_type_view,
            };
            let mut cursor = typ.as_ref().clone();
            let mut items = Vec::new();
            loop {
                match projected_type_view(proof, &cursor).ok()? {
                    ProjectedTypeView::Forall { variable, body, .. } => {
                        items.push(Value::ReflTypeVar(variable));
                        cursor = body;
                    }
                    _ => {
                        return Some(TypeFold {
                            items,
                            remainder: Some(projected_value(cursor)),
                        });
                    }
                }
            }
        }
        let items = match builtin {
            ComptimeBuiltin::TypeProductSpineFold | ComptimeBuiltin::TypeSumSpineFold => {
                projected_spine(proof, typ, builtin == ComptimeBuiltin::TypeProductSpineFold)
                    .ok()?
            }
            ComptimeBuiltin::TypeArgsFold => {
                crate::pass::typecheck_core::apply::fills::projected_type_arguments(proof, typ)
                    .ok()??
            }
            _ => unreachable!("projected type fold carries its extractor"),
        };
        return Some(TypeFold {
            items: items.into_iter().map(projected_value).collect(),
            remainder: None,
        });
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
            | EvalExpr::ReflIntrinsicBoolean { .. }
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
        EvalExpr::ReflIntrinsicBoolean { left, .. } => [Some(left), None, None],
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
    let observer = match expr {
        EvalExpr::ReflTypeView { .. } => Some(ComptimeBuiltin::TypeView),
        EvalExpr::ReflTypeInstantiate { .. } => Some(ComptimeBuiltin::TypeInstantiate),
        EvalExpr::ReflTypeRootPredicate { name, .. } => ComptimeBuiltin::from_public_name(name),
        _ => None,
    };
    if let Some(builtin) = observer
        && neutral_reflection_is_pending(builtin, args.as_slice(), ctx)
    {
        return Value::Stuck(
            EvalRef::new(Value::Primitive(builtin)),
            args.into_vec().into(),
        );
    }
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
            let result = reflection_binary_type_value(ComptimeBuiltin::TypeProduct, &left, &right);
            finish_direct_reflection("__type_product__", result, vec![left, right], ctx, start)
        }
        EvalExpr::ReflTypeSum { left: _, right: _ } => {
            let left = values.next().expect("direct reflection operand");
            let right = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = reflection_binary_type_value(ComptimeBuiltin::TypeSum, &left, &right);
            finish_direct_reflection("__type_sum__", result, vec![left, right], ctx, start)
        }
        EvalExpr::ReflTypeView { typ: _ } => {
            let typ = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            finish_direct_reflection(
                "__type_view__",
                reflected_type_carrier(&typ).and_then(|typ| reflected_type_view_value(typ, ctx)),
                vec![typ],
                ctx,
                start,
            )
        }
        EvalExpr::ReflTypeEqual { left: _, right: _ } => {
            let left = values.next().expect("direct reflection operand");
            let right = values.next().expect("direct reflection operand");
            let start = direct_reflection_start(ctx);
            let result = reflection_type_equal_value(&left, &right, ctx);
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
            let result = reflection_type_var_equal_value(&left, &right, ctx);
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
                reflected_type_carrier(&scheme),
                reflected_type_carrier(&arg),
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
