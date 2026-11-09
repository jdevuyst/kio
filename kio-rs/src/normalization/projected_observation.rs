use super::*;
use crate::pass::typecheck_core::apply::fills::ProjectedObservation;

fn requirements(
    builtin: ComptimeBuiltin,
) -> Option<(usize, &'static [(usize, ProjectedObservation)])> {
    use ComptimeBuiltin::*;
    use ProjectedObservation::*;
    Some(match builtin {
        TypeView => (1, &[(0, View)]),
        TypeIsHost | TypeArityFn | TypeIsProductRoot | TypeIsSumRoot | TypeArgsFold => {
            (if builtin == TypeArgsFold { 3 } else { 1 }, &[(0, Root)])
        }
        TypeDisplay | TypeShortName => (1, &[(0, Complete)]),
        TypeEqual | TypeInstantiate | TypeHostConverters => (2, &[(0, Complete), (1, Complete)]),
        TermHostConvert => (
            4,
            &[(0, Complete), (1, Complete), (2, Complete), (3, Complete)],
        ),
        TermSpecialize => (3, &[(0, ForallPrefix), (1, Complete), (2, Complete)]),
        TypeForallPrefixFold => (3, &[(0, ForallPrefix)]),
        TypeProductSpineFold => (3, &[(0, ProductSpine)]),
        TypeSumSpineFold => (3, &[(0, SumSpine)]),
        TypeApply | TermTypeApp | IntrinsicFst | IntrinsicSnd | IntrinsicLeft | IntrinsicRight => {
            (2, &[(0, Root)])
        }
        TermFn => (2, &[(0, FunctionParameters)]),
        TermCall => (3, &[(0, Root)]),
        IntrinsicEither => (5, &[(0, Root)]),
        IntrinsicAbsurd => (2, &[(0, Root)]),
        ComptimeProof
        | Type
        | CheckedTerm
        | TypeVar
        | TypeName
        | TypeArity
        | DiagnosticText
        | ComptimeBool
        | ComptimeStr
        | FillCtx
        | Fill
        | FillResult
        | ReflectType
        | TypeUnit
        | TypeBottom
        | TypeProduct
        | TypeSum
        | TypeArrow
        | TypeForall
        | TypeVarFn
        | TypeNameType
        | TypeNameEqual
        | TypeVarEqual
        | TypeVarArity
        | TypeArityZero
        | TypeAritySucc
        | TypeArityEqual
        | TypeArityFold
        | TypeNameParamAritiesFold
        | TypeNameDisplay
        | TypeVarDisplay
        | TypeArityDisplay
        | DiagnosticConcat
        | MacroError
        | TypeError
        | StructuralRecur
        | TermType
        | TermLet
        | TermTypeFn
        | TermUnit
        | IntrinsicPair
        | IntrinsicIfThenElse => return None,
    })
}

pub(super) fn check(builtin: ComptimeBuiltin, args: &[Value], ctx: &EvalCtx<'_>) -> Option<Value> {
    let (arity, requirements) = requirements(builtin)?;
    let proof = marked_comptime_proof_from_value(args.first()?)?;
    let runtime = reflection_runtime_args(builtin, args);
    if runtime.len() != arity {
        return None;
    }
    // Only the immediate builtin operands are authenticated here; callback bodies
    // and captured values remain unevaluated.
    for operand in runtime {
        if let Value::Projected(value) = operand {
            value.observation_is_known(proof, ProjectedObservation::Root)?;
        }
    }
    let unresolved = requirements.iter().any(|(index, observation)| {
        let Value::Projected(value) = &runtime[*index] else {
            return false;
        };
        value.observation_is_known(proof, *observation) == Some(false)
    });
    unresolved.then(|| incomplete_observation_fault(builtin, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::typecheck_core::apply::fills::{
        ProjectedTypeView, open_noncanonical_function_reflection_for_test, projected_type_view,
        projected_type_with_proof_for_fuel_test, unresolved_type_reflections_for_test,
    };

    fn proof_value(proof: crate::pass::typecheck_core::apply::fills::MarkedComptimeProof) -> Value {
        Value::Data(EvalRef::new(DataValue::new_marked_comptime_proof(proof)))
    }

    fn unknown() -> (Value, Value) {
        let (proof, _, goal, _, _) = unresolved_type_reflections_for_test();
        (proof_value(proof), projected_value(goal))
    }

    fn observed(
        builtin: ComptimeBuiltin,
        proof: &Value,
        value: &Value,
        ctx: &EvalCtx<'_>,
    ) -> Value {
        apply(
            Value::Primitive(builtin),
            vec![proof.clone(), value.clone()],
            ctx,
        )
    }

    fn assert_fault(value: &Value, builtin: ComptimeBuiltin, ctx: &EvalCtx<'_>) {
        assert!(
            matches!(value, Value::Data(data) if data.incomplete_observation() == Some(builtin)),
            "{value:?}"
        );
        assert!(
            comptime_fault_message(value, ctx)
                .unwrap()
                .contains(builtin.public_name())
        );
        assert!(structural_recur_stuck_message(value, ctx).is_none());
        assert!(!exact_call_memo_result_shareable(value));
        let mut budget = EXACT_CALL_MEMO_KEY_BUDGET;
        assert!(value_memo_key(value, &mut budget).is_none());
        assert!(nf_eq_checked(value, value, ctx).is_err());
    }

    #[test]
    fn observation_faults_cover_saturated_observers_and_preserve_partial_transport() {
        let ctx = EvalCtx::new();
        let (proof, value) = unknown();
        for builtin in [
            ComptimeBuiltin::TypeView,
            ComptimeBuiltin::TypeDisplay,
            ComptimeBuiltin::TypeShortName,
            ComptimeBuiltin::TypeIsHost,
            ComptimeBuiltin::TypeArityFn,
            ComptimeBuiltin::TypeIsProductRoot,
            ComptimeBuiltin::TypeIsSumRoot,
        ] {
            assert_fault(&observed(builtin, &proof, &value, &ctx), builtin, &ctx);
            let partial = apply(Value::Primitive(builtin), vec![proof.clone()], &ctx);
            assert!(comptime_fault_message(&partial, &ctx).is_none());
        }
        let unit = refl_type_value(Type::Unit { meta: zero_meta() });
        for builtin in [
            ComptimeBuiltin::TypeEqual,
            ComptimeBuiltin::TypeInstantiate,
            ComptimeBuiltin::TypeHostConverters,
            ComptimeBuiltin::TypeApply,
        ] {
            let partial = apply(
                Value::Primitive(builtin),
                vec![proof.clone(), value.clone()],
                &ctx,
            );
            assert!(comptime_fault_message(&partial, &ctx).is_none());
            assert_fault(&apply(partial, vec![unit.clone()], &ctx), builtin, &ctx);
        }
        for builtin in [
            ComptimeBuiltin::TypeArgsFold,
            ComptimeBuiltin::TypeProductSpineFold,
            ComptimeBuiltin::TypeSumSpineFold,
        ] {
            assert_fault(
                &apply(
                    Value::Primitive(builtin),
                    vec![
                        proof.clone(),
                        value.clone(),
                        Value::Unit,
                        Value::Atom("unreached_step".into()),
                    ],
                    &ctx,
                ),
                builtin,
                &ctx,
            );
        }
        let (foreign, _) = unknown();
        let foreign = observed(ComptimeBuiltin::TypeView, &foreign, &value, &ctx);
        assert!(
            comptime_fault_message(&foreign, &ctx).is_none(),
            "foreign capability became an observation: {foreign:?}"
        );
        let ordinary = data_value("__unresolved_type_observation__", vec![Value::Unit]);
        assert!(comptime_fault_message(&ordinary, &ctx).is_none());
    }

    #[test]
    fn observation_known_heads_keep_opaque_children_and_rigid_names() {
        use crate::ast::{Lowered, Meta, TypeGoalRef};
        let ctx = EvalCtx::new();
        let unit = Type::<Lowered>::Unit {
            meta: Meta::new(zero_span()),
        };
        let goal = Type::<Lowered>::Goal {
            goal: TypeGoalRef::for_test(7, 0, 0),
            args: Vec::new(),
            meta: Meta::new(zero_span()),
            ext: (),
        };
        let product = Type::Product {
            left: Box::new(goal.clone()),
            right: Box::new(unit.clone()),
            meta: Meta::new(zero_span()),
        };
        let (proof, value) = projected_type_with_proof_for_fuel_test(product.clone(), product);
        let proof = proof_value(proof);
        let value = projected_value(value);
        for builtin in [
            ComptimeBuiltin::TypeView,
            ComptimeBuiltin::TypeIsProductRoot,
            ComptimeBuiltin::TypeIsHost,
            ComptimeBuiltin::TypeArityFn,
        ] {
            let result = observed(builtin, &proof, &value, &ctx);
            assert!(
                comptime_fault_message(&result, &ctx).is_none(),
                "{builtin:?}: {result:?}"
            );
            assert!(
                !matches!(result, Value::Stuck(..)),
                "{builtin:?}: {result:?}"
            );
        }
        assert_fault(
            &observed(ComptimeBuiltin::TypeDisplay, &proof, &value, &ctx),
            ComptimeBuiltin::TypeDisplay,
            &ctx,
        );
        let opaque_element_fold = apply(
            Value::Primitive(ComptimeBuiltin::TypeProductSpineFold),
            vec![
                proof,
                value,
                Value::Unit,
                Value::Atom("opaque_element_step".into()),
            ],
            &ctx,
        );
        assert!(comptime_fault_message(&opaque_element_fold, &ctx).is_none());

        let path = Type::synth_path(vec!["Known".into()], vec![goal.clone()], zero_span());
        let (proof, path) = projected_type_with_proof_for_fuel_test(path.clone(), path);
        let proof = proof_value(proof);
        let path = projected_value(path);
        assert_fault(
            &observed(ComptimeBuiltin::TypeView, &proof, &path, &ctx),
            ComptimeBuiltin::TypeView,
            &ctx,
        );
        for builtin in [ComptimeBuiltin::TypeIsHost, ComptimeBuiltin::TypeArityFn] {
            assert!(
                comptime_fault_message(&observed(builtin, &proof, &path, &ctx), &ctx).is_none()
            );
        }
        let args = apply(
            Value::Primitive(ComptimeBuiltin::TypeArgsFold),
            vec![
                proof,
                path,
                Value::Unit,
                Value::Atom("opaque_argument_step".into()),
            ],
            &ctx,
        );
        assert!(comptime_fault_message(&args, &ctx).is_none());
        let open_spine = Type::Product {
            left: Box::new(unit),
            right: Box::new(goal),
            meta: Meta::new(zero_span()),
        };
        let (proof, value) =
            projected_type_with_proof_for_fuel_test(open_spine.clone(), open_spine);
        assert_fault(
            &apply(
                Value::Primitive(ComptimeBuiltin::TypeProductSpineFold),
                vec![
                    proof_value(proof),
                    projected_value(value),
                    Value::Unit,
                    Value::Atom("step".into()),
                ],
                &ctx,
            ),
            ComptimeBuiltin::TypeProductSpineFold,
            &ctx,
        );

        let (proof, _, function) = open_noncanonical_function_reflection_for_test();
        let ProjectedTypeView::Function { param, .. } =
            projected_type_view(&proof, &function).unwrap()
        else {
            panic!("function shell")
        };
        let proof = proof_value(proof);
        let rigid = projected_value(param);
        assert!(matches!(
            observed(ComptimeBuiltin::TypeArityFn, &proof, &rigid, &ctx),
            Value::ReflTypeArity(0)
        ));
        assert!(
            matches!(observed(ComptimeBuiltin::TypeDisplay, &proof, &rigid, &ctx), Value::DiagnosticText(text) if text == "A")
        );
        assert!(matches!(
            observed(ComptimeBuiltin::TypeIsHost, &proof, &rigid, &ctx),
            Value::Bool(false)
        ));
    }

    fn observe_expr(proof: Value, value: Value, direct: bool) -> EvalExpr {
        if direct {
            EvalExpr::ProofedReflection {
                builtin: ComptimeBuiltin::TypeEqual,
                proof: EvalRef::new(EvalExpr::Const(proof)),
                direct: EvalRef::new(EvalExpr::ReflTypeEqual {
                    left: EvalRef::new(EvalExpr::Const(value)),
                    right: EvalRef::new(EvalExpr::ReflTypeUnit),
                }),
            }
        } else {
            EvalExpr::CallPrimitive {
                builtin: ComptimeBuiltin::TypeEqual,
                args: vec![
                    EvalRef::new(EvalExpr::Const(proof)),
                    EvalRef::new(EvalExpr::Const(value)),
                    EvalRef::new(EvalExpr::ReflTypeUnit),
                ],
            }
        }
    }

    #[test]
    fn observation_fault_survives_discard_masking_and_existing_direct_retry() {
        for direct in [false, true] {
            let metrics = Arc::new(EvalMetrics::default());
            let ctx = EvalCtx::new().with_metrics(metrics.clone());
            let (proof, value) = unknown();
            for body in [
                Value::Unit,
                Value::CheckedTerm(EvalRef::new(CheckedTerm::checked_error_for_test(
                    CheckedTermErrorKind::Macro,
                    "later macro error",
                ))),
                Value::CheckedTerm(EvalRef::new(CheckedTerm::checked_error_for_test(
                    CheckedTermErrorKind::Type,
                    "later type error",
                ))),
            ] {
                let function = super::super::tests::test_eval_function(
                    &["key"],
                    EvalExpr::Let {
                        value: EvalRef::new(observe_expr(proof.clone(), value.clone(), direct)),
                        body: EvalRef::new(EvalExpr::Const(body)),
                    },
                );
                assert!(function.memo_candidate);
                for _ in 0..2 {
                    let fault = apply(
                        Value::EvalClosure {
                            function: function.clone(),
                            captured: Vec::<Value>::new().into(),
                        },
                        vec![Value::Unit],
                        &ctx,
                    );
                    assert_fault(&fault, ComptimeBuiltin::TypeEqual, &ctx);
                }
            }
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.exact_call_memo_hits, 0, "{snapshot:?}");
            assert_eq!(snapshot.exact_call_memo_stores, 0, "{snapshot:?}");
            assert!(snapshot.exact_call_memo_misses > 0, "{snapshot:?}");
            assert!(ctx.operations.lock().unwrap().is_empty());
            assert!(matches!(
                apply(
                    Value::Primitive(ComptimeBuiltin::TypeUnit),
                    vec![proof],
                    &ctx
                ),
                Value::ReflType(_)
            ));
        }
    }

    #[test]
    fn observation_fault_is_not_created_by_an_untaken_body_or_hidden_by_a_nested_operation() {
        let ctx = EvalCtx::new();
        let (proof, value) = unknown();
        let function = super::super::tests::test_eval_function(
            &[],
            observe_expr(proof.clone(), value.clone(), false),
        );
        let dormant = Value::EvalClosure {
            function,
            captured: Vec::<Value>::new().into(),
        };
        let clean = Value::EvalClosure {
            function: super::super::tests::test_eval_function(&[], EvalExpr::Unit),
            captured: Vec::<Value>::new().into(),
        };
        let mut expr = EvalExpr::CoreIf {
            condition: EvalRef::new(EvalExpr::Bool(false)),
            then_thunk: EvalRef::new(EvalExpr::Const(dormant)),
            else_thunk: EvalRef::new(EvalExpr::Const(clean)),
        };
        let operation = ctx.enter_eval_operation();
        let result = eval_compiled_expr(&expr, &mut EvalSlots::root(Vec::new()), &ctx);
        assert!(matches!(operation.finish(result), Value::Unit));
        let EvalExpr::CoreIf { condition, .. } = &mut expr else {
            unreachable!()
        };
        *condition = EvalRef::new(EvalExpr::Bool(true));
        let operation = ctx.enter_eval_operation();
        let result = eval_compiled_expr(&expr, &mut EvalSlots::root(Vec::new()), &ctx);
        assert_fault(&operation.finish(result), ComptimeBuiltin::TypeEqual, &ctx);
        let warm_function = super::super::tests::test_eval_function(
            &["key"],
            EvalExpr::ReflTypeEqual {
                left: EvalRef::new(EvalExpr::ReflTypeUnit),
                right: EvalRef::new(EvalExpr::ReflTypeUnit),
            },
        );
        let metrics = Arc::new(EvalMetrics::default());
        let ctx = EvalCtx::new().with_metrics(metrics.clone());
        let warmed = || Value::EvalClosure {
            function: warm_function.clone(),
            captured: Vec::<Value>::new().into(),
        };
        assert!(matches!(
            apply(warmed(), vec![Value::Unit], &ctx),
            Value::Bool(true)
        ));
        assert_eq!(metrics.snapshot().exact_call_memo_stores, 1);
        let outer = ctx.enter_eval_operation();
        let nested = observed(ComptimeBuiltin::TypeView, &proof, &value, &ctx);
        assert_fault(&nested, ComptimeBuiltin::TypeView, &ctx);
        assert!(ctx.operation_has_fault());
        assert!(matches!(
            apply(warmed(), vec![Value::Unit], &ctx),
            Value::Bool(true)
        ));
        assert_eq!(metrics.snapshot().exact_call_memo_hits, 0);
        assert_eq!(metrics.snapshot().exact_call_memo_stores, 1);
        assert_eq!(metrics.snapshot().exact_call_memo_key_skips, 1);
        assert_fault(&outer.finish(Value::Unit), ComptimeBuiltin::TypeView, &ctx);
        assert!(!ctx.operation_has_fault());
    }
}
