use super::*;

fn scheme(name: &str) -> EvalAstType {
    Type::Forall {
        param: TypeParam {
            presentation: Default::default(),
            name: name.into(),
            span: zero_span(),
            kind: None,
        },
        body: Box::new(type_path(name)),
        meta: zero_meta(),
    }
}

fn written_scheme(name: &str) -> EvalAstType {
    let Type::Forall { mut param, .. } = scheme(name) else {
        unreachable!()
    };
    param.presentation = crate::ast::TypeParamPresentation::written();
    param.rename("Canonical".into());
    Type::Forall {
        param,
        body: Box::new(type_path("Canonical")),
        meta: zero_meta(),
    }
}

fn forall_view_body(view: &Value) -> &EvalAstType {
    let mut payload = view;
    for _ in 0..6 {
        let Value::Data(data) = payload else {
            panic!("forall sum arm")
        };
        payload = &data.args()[0];
    }
    let Value::Data(pair) = payload else {
        panic!("forall packet")
    };
    let Value::Data(rest) = &pair.args()[1] else {
        panic!("forall rest")
    };
    let Value::ReflType(body) = &rest.args()[1] else {
        panic!("forall body")
    };
    body.as_type()
}

#[test]
fn type_view_cache_guards_nested_written_names_without_changing_reflection() {
    for names in [["A", "B", "A"], ["B", "A", "B"]] {
        let ctx = EvalCtx::new();
        for name in names {
            let mut ty = scheme("Outer");
            let Type::Forall { body, .. } = &mut ty else {
                unreachable!()
            };
            **body = written_scheme(name);
            let view = type_view_value(&ty, &ctx).unwrap();
            assert_eq!(
                crate::pass::typecheck_core::display_type(forall_view_body(&view)),
                format!("[{name}] {name}")
            );
            assert_eq!(
                crate::pass::typecheck_core::display_reflected_type(forall_view_body(&view)),
                "[Canonical] Canonical"
            );
        }
        assert_eq!(ctx.type_view_cache.inner.lock().unwrap().len(), 1);
    }
}

#[test]
fn exact_call_memo_guards_written_names_in_arguments_and_captures() {
    for captured in [false, true] {
        for names in [["A", "B", "A"], ["B", "A", "B"]] {
            let metrics = Arc::new(EvalMetrics::default());
            let ctx = EvalCtx::new().with_metrics(metrics.clone());
            let function = test_eval_function(
                &["argument"],
                EvalExpr::ReflTypeView {
                    typ: EvalRef::new(EvalExpr::Local(0)),
                },
            );
            assert!(function.memo_candidate);
            for name in names {
                let mut input = scheme("Outer");
                let Type::Forall { body, .. } = &mut input else {
                    unreachable!()
                };
                **body = written_scheme(name);
                let input = refl_type_value(input);
                let (captures, args) = if captured {
                    (vec![input], vec![Value::Unit])
                } else {
                    (Vec::new(), vec![input])
                };
                ctx.type_view_cache.inner.lock().unwrap().clear();
                let result = apply(
                    Value::EvalClosure {
                        function: function.clone(),
                        captured: captures.into(),
                    },
                    args,
                    &ctx,
                );
                assert_eq!(
                    crate::pass::typecheck_core::display_type(forall_view_body(&result)),
                    format!("[{name}] {name}")
                );
            }
            let stats = metrics.snapshot();
            assert_eq!(stats.exact_call_memo_misses, 2, "captured={captured}");
            assert_eq!(stats.exact_call_memo_stores, 1);
            assert_eq!(stats.exact_call_memo_hits, 1);
            assert_eq!(ctx.exact_call_memo.inner.lock().unwrap().len(), 1);
        }
    }
}

fn assert_forall_view(view: &Value, expected: &str) {
    let mut payload = view;
    for _ in 0..6 {
        let Value::Data(data) = payload else {
            panic!("sum arm: {payload:?}")
        };
        let [next] = data.args() else {
            panic!("sum payload: {data:?}")
        };
        payload = next;
    }
    let Value::Data(pair) = payload else {
        panic!("forall packet: {payload:?}")
    };
    let [Value::ReflTypeVar(variable), rest] = pair.args() else {
        panic!("forall binder: {pair:?}")
    };
    assert_eq!(variable.name, expected);
    assert_eq!(variable.arity, 0);
    let Value::Data(rest) = rest else {
        panic!("forall tail")
    };
    let [Value::ReflTypeArity(0), Value::ReflType(body)] = rest.args() else {
        panic!("forall body: {rest:?}")
    };
    assert_eq!(body.as_type(), &type_path(expected));
}

fn call(function: &EvalRef<EvalFunction>, args: Vec<Value>, ctx: &EvalCtx<'_>) -> Value {
    apply(
        Value::EvalClosure {
            function: function.clone(),
            captured: Vec::new().into(),
        },
        args,
        ctx,
    )
}

#[test]
fn type_view_cache_keeps_each_binder_and_its_body_in_both_orders() {
    for names in [["A", "B"], ["B", "A"]] {
        let ctx = EvalCtx::new();
        let first = scheme(names[0]);
        let second = scheme(names[1]);
        assert!(canonical_refl_type_equiv(&first, &second, &ctx));
        let a = type_view_value(&first, &ctx).unwrap();
        let b = type_view_value(&second, &ctx).unwrap();
        let again = type_view_value(&first, &ctx).unwrap();
        assert_forall_view(&a, names[0]);
        assert_forall_view(&b, names[1]);
        assert_forall_view(&again, names[0]);
        let (Value::Data(a), Value::Data(again)) = (&a, &again) else {
            panic!("views")
        };
        assert!(
            EvalRef::ptr_eq(a, again),
            "same-identity cache hit must remain"
        );
        assert_eq!(ctx.type_view_cache.inner.lock().unwrap().len(), 2);
    }
}

#[test]
fn exact_call_memo_keeps_reflected_input_identity_without_a_view_cache_hit() {
    for names in [["A", "B"], ["B", "A"]] {
        let metrics = Arc::new(EvalMetrics::default());
        let cache = SharedExactCallMemoCache::default();
        let ctx = EvalCtx::new()
            .with_metrics(metrics.clone())
            .with_exact_call_memo_cache(cache.clone());
        let function = test_eval_function(
            &["typ"],
            EvalExpr::ReflTypeView {
                typ: EvalRef::new(EvalExpr::Local(0)),
            },
        );
        assert!(function.memo_candidate);
        for name in names {
            // An empty view cache makes a stale result attributable to call memoization.
            ctx.type_view_cache.inner.lock().unwrap().clear();
            let view = call(&function, vec![refl_type_value(scheme(name))], &ctx);
            assert_forall_view(&view, name);
        }
        assert_eq!(metrics.snapshot().exact_call_memo_misses, 2);
        assert_eq!(metrics.snapshot().exact_call_memo_stores, 2);
        assert_eq!(metrics.snapshot().exact_call_memo_hits, 0);
        let shared = EvalCtx::new()
            .with_metrics(metrics.clone())
            .with_exact_call_memo_cache(cache);
        assert_forall_view(
            &call(&function, vec![refl_type_value(scheme(names[0]))], &shared),
            names[0],
        );
        assert!(shared.type_view_cache.inner.lock().unwrap().is_empty());
        assert_eq!(metrics.snapshot().exact_call_memo_hits, 1);
    }
}

#[test]
fn exact_call_memo_reuses_semantic_closed_binders_through_helpers_or_contexts() {
    let factory = test_eval_function(
        &[],
        EvalExpr::CallPrimitive {
            builtin: ComptimeBuiltin::TypeForall,
            args: vec![
                EvalRef::new(EvalExpr::Const(Value::ReflTypeArity(0))),
                EvalRef::new(EvalExpr::Const(Value::Primitive(
                    ComptimeBuiltin::TypeVarFn,
                ))),
            ],
        },
    );
    let wrapper = test_eval_function(
        &[],
        EvalExpr::CallFn {
            function: factory.clone(),
            args: Vec::new(),
        },
    );
    let cache = SharedExactCallMemoCache::default();
    let metrics = Arc::new(EvalMetrics::default());
    let ctx = EvalCtx::new()
        .with_metrics(metrics.clone())
        .with_exact_call_memo_cache(cache.clone());
    let shared = EvalCtx::new()
        .with_metrics(metrics.clone())
        .with_exact_call_memo_cache(cache.clone());
    let mut first = None;
    for (function, context) in [
        (&factory, &ctx),
        (&factory, &ctx),
        (&wrapper, &ctx),
        (&wrapper, &shared),
    ] {
        let value = call(function, Vec::new(), context);
        let Some(Type::Forall { param, body, .. }) = as_refl_type_ref(&value) else {
            panic!("fresh scheme: {value:?}")
        };
        assert_eq!(body.as_ref(), &type_path(&param.name));
        if let Some(previous) = &first {
            assert!(nf_eq(previous, &value, context));
        } else {
            first = Some(value);
        }
    }
    assert!(!cache.inner.lock().unwrap().is_empty());
    assert!(metrics.snapshot().exact_call_memo_hits >= 2);
    assert!(metrics.snapshot().exact_call_memo_stores >= 2);
}

#[test]
fn discarded_closed_term_type_fn_does_not_veto_semantic_memo_reuse() {
    let callback = Value::EvalClosure {
        function: test_eval_function(&["var"], EvalExpr::ReflTermUnit),
        captured: Vec::new().into(),
    };
    let function = test_eval_function(
        &[],
        EvalExpr::Let {
            value: EvalRef::new(EvalExpr::CallPrimitive {
                builtin: ComptimeBuiltin::TermTypeFn,
                args: vec![
                    EvalRef::new(EvalExpr::Const(Value::ReflTypeArity(0))),
                    EvalRef::new(EvalExpr::Const(callback)),
                ],
            }),
            body: EvalRef::new(EvalExpr::ReflTypeEqual {
                left: EvalRef::new(EvalExpr::ReflTypeUnit),
                right: EvalRef::new(EvalExpr::ReflTypeUnit),
            }),
        },
    );
    let ctx = EvalCtx::new();
    for _ in 0..2 {
        assert!(matches!(
            call(&function, Vec::new(), &ctx),
            Value::Bool(true)
        ));
    }
    assert!(!ctx.exact_call_memo.inner.lock().unwrap().is_empty());
}

#[test]
fn reflection_keys_keep_nested_identity_arity_kind_and_path_slots() {
    let wrap = |ty| {
        Type::synth_path(
            vec!["owner".into(), "Box".into()],
            vec![Type::Product {
                left: Box::new(Type::Sum {
                    left: Box::new(ty),
                    right: Box::new(Type::Bottom { meta: zero_meta() }),
                    meta: zero_meta(),
                }),
                right: Box::new(Type::Unit { meta: zero_meta() }),
                meta: zero_meta(),
            }],
            zero_span(),
        )
    };
    assert_ne!(
        ReflectionTypeKey::new(&wrap(scheme("A"))),
        ReflectionTypeKey::new(&wrap(scheme("B")))
    );
    let arrow = |abi_arity| Type::Function {
        param: Box::new(Type::Unit { meta: zero_meta() }),
        ret: Box::new(Type::Unit { meta: zero_meta() }),
        meta: zero_meta(),
        abi_arity,
        caps: (),
    };
    assert_ne!(
        ReflectionTypeKey::new(&arrow(0)),
        ReflectionTypeKey::new(&arrow(1))
    );
    assert!(canonical_refl_type_equiv(
        &arrow(0),
        &arrow(1),
        &EvalCtx::new()
    ));
    let a = scheme("A");
    let mut b = a.clone();
    let Type::Forall { param, meta, .. } = &mut b else {
        unreachable!()
    };
    param.kind = Some(Box::new(crate::ast::Kind::Star));
    param.span = Span::new(10, 20);
    meta.span = Span::new(5, 30);
    assert_eq!(ReflectionTypeKey::new(&a), ReflectionTypeKey::new(&b));
    let Type::Forall { param, .. } = &mut b else {
        unreachable!()
    };
    param.kind = Some(Box::new(crate::ast::Kind::arrow_chain(2)));
    assert_ne!(ReflectionTypeKey::new(&a), ReflectionTypeKey::new(&b));
}

#[test]
fn reflection_key_flattens_deep_types_and_complete_kinds() {
    std::thread::Builder::new()
        .stack_size(96 * 1024)
        .spawn(|| {
            use crate::ast::Kind;
            let mut kind = Kind::Star;
            for _ in 0..1024 {
                kind = Kind::Arrow(Box::new(kind), Box::new(Kind::Star));
            }
            let mut ty = scheme("F");
            let Type::Forall { param, .. } = &mut ty else {
                unreachable!()
            };
            param.kind = Some(Box::new(kind));
            for _ in 0..1024 {
                ty = Type::Product {
                    left: Box::new(Type::Unit { meta: zero_meta() }),
                    right: Box::new(ty),
                    meta: zero_meta(),
                };
            }
            let ty = EvalRef::new(ty);
            let key = ReflectionTypeKey::new(&ty);
            assert_eq!(key, key.clone());
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&key, &mut hash);
            let mut budget = EXACT_CALL_MEMO_KEY_BUDGET;
            assert!(type_traversal::charge_memo_type(&ty, &mut budget).is_none());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn function_view_observes_domain_and_result_across_callable_layouts() {
    let ctx = EvalCtx::new();
    let product = Type::Product {
        left: Box::new(type_path("A")),
        right: Box::new(Type::Product {
            left: Box::new(type_path("B")),
            right: Box::new(type_path("C")),
            meta: zero_meta(),
        }),
        meta: zero_meta(),
    };
    for (domain, layouts) in [
        (Type::Unit { meta: zero_meta() }, vec![0, 1]),
        (product, vec![1, 2, 3]),
    ] {
        let result = type_path("Result");
        let expected = value_sum_inject(
            4,
            8,
            value_pair(
                refl_type_value(domain.clone()),
                refl_type_value(result.clone()),
            ),
        );
        for abi_arity in layouts.into_iter().cycle().take(6) {
            let ty = Type::Function {
                param: Box::new(domain.clone()),
                ret: Box::new(result.clone()),
                abi_arity,
                caps: (),
                meta: zero_meta(),
            };
            let actual = type_view_value(&ty, &ctx).expect("function view");
            assert!(nf_eq(&actual, &expected, &ctx), "{actual:?}");
        }
        let arrow = apply(
            Value::Primitive(ComptimeBuiltin::TypeArrow),
            vec![
                refl_type_value(domain.clone()),
                refl_type_value(result.clone()),
            ],
            &ctx,
        );
        let Some(Type::Function {
            param,
            ret,
            abi_arity,
            ..
        }) = as_refl_type_ref(&arrow)
        else {
            panic!("constructed function: {arrow:?}")
        };
        assert_eq!(param.as_ref(), &domain);
        assert_eq!(ret.as_ref(), &result);
        assert_eq!(
            *abi_arity,
            usize::from(!matches!(domain, Type::Unit { .. }))
        );
        assert!(nf_eq(
            &type_view_value(as_refl_type_ref(&arrow).unwrap(), &ctx).unwrap(),
            &expected,
            &ctx
        ));
    }
    assert!(ComptimeBuiltin::from_public_name("__type_function_params_fold__").is_none());
}

#[test]
fn projected_function_constructor_preserves_closed_and_open_semantic_domains() {
    use crate::pass::typecheck_core::apply::fills::{
        ProjectedTypeView, focused_type_for_test, open_function_reflection_for_test,
        projected_type_view,
    };
    let ctx = EvalCtx::new();
    let (proof_value, _fills) = fresh_fills_capability_values();
    let proof = marked_comptime_proof_from_value(&proof_value).unwrap();
    let unit = Type::Unit { meta: zero_meta() };
    let product = Type::Product {
        left: Box::new(unit.clone()),
        right: Box::new(unit.clone()),
        meta: zero_meta(),
    };
    for domain in [unit.clone(), product] {
        let input = projected_value(focused_type_for_test(
            proof,
            ReflectedType::new_trusted(domain.clone()),
        ));
        let arrow = apply(
            Value::Primitive(ComptimeBuiltin::TypeArrow),
            vec![proof_value.clone(), input, refl_type_value(unit.clone())],
            &ctx,
        );
        let expected = Type::Function {
            param: Box::new(domain),
            ret: Box::new(unit.clone()),
            abi_arity: 1,
            caps: (),
            meta: zero_meta(),
        };
        let equal = apply(
            Value::Primitive(ComptimeBuiltin::TypeEqual),
            vec![proof_value.clone(), arrow, refl_type_value(expected)],
            &ctx,
        );
        assert!(matches!(equal, Value::Bool(true)), "{equal:?}");
    }

    let (proof, _foreign, parent) =
        open_function_reflection_for_test(ReflectedType::new_trusted(unit));
    let ProjectedTypeView::Function {
        param: closed,
        ret: open,
    } = projected_type_view(&proof, &parent).unwrap()
    else {
        panic!("open function shell")
    };
    let arrow = crate::pass::typecheck_core::apply::fills::projected_type_arrow(
        &proof,
        &projected_value(open),
        &projected_value(closed),
    )
    .unwrap();
    let ProjectedTypeView::Function { param, ret } = projected_type_view(&proof, &arrow).unwrap()
    else {
        panic!("constructed open function shell")
    };
    assert!(matches!(
        projected_type_view(&proof, &ret).unwrap(),
        ProjectedTypeView::Unit
    ));
    assert!(param.closed_reflected().is_none());
    assert!(projected_type_view(&proof, &param).is_err());
}

#[test]
fn type_var_views_keep_complete_owner_correspondence_and_distinct_live_bindings() {
    let ctx = EvalCtx::new();
    let a = ReflectedType::new_trusted(scheme("A"));
    let b = ReflectedType::new_trusted(scheme("B"));
    let (a_var, a_body) = a.open_forall().unwrap();
    let (b_var, b_body) = b.open_forall().unwrap();
    assert!(!a_var.same_reference(&b_var));
    for args in [
        vec![
            Value::ReflTypeVar(a_var.clone()),
            Value::ReflTypeVar(b_var.clone()),
        ],
        vec![Value::ReflTypeVar(b_var), Value::ReflTypeVar(a_var)],
    ] {
        assert!(matches!(
            apply(
                Value::Primitive(ComptimeBuiltin::TypeVarEqual),
                args.clone(),
                &ctx
            ),
            Value::Bool(true)
        ));
        let direct = test_eval_function(
            &["a", "b"],
            EvalExpr::ReflTypeVarEqual {
                left: EvalRef::new(EvalExpr::Local(0)),
                right: EvalRef::new(EvalExpr::Local(1)),
            },
        );
        assert!(matches!(call(&direct, args, &ctx), Value::Bool(true)));
    }
    assert!(matches!(
        apply(
            Value::Primitive(ComptimeBuiltin::TypeEqual),
            vec![Value::ReflType(a_body), Value::ReflType(b_body)],
            &ctx
        ),
        Value::Bool(true)
    ));
    let x = Value::ReflTypeVar(ReflectedVariable::live("T".into(), 0));
    let y = Value::ReflTypeVar(ReflectedVariable::live("T".into(), 0));
    assert!(matches!(
        apply(
            Value::Primitive(ComptimeBuiltin::TypeVarEqual),
            vec![x.clone(), x],
            &ctx
        ),
        Value::Bool(true)
    ));
    let z = Value::ReflTypeVar(ReflectedVariable::live("T".into(), 0));
    assert!(matches!(
        apply(
            Value::Primitive(ComptimeBuiltin::TypeVarEqual),
            vec![y, z],
            &ctx
        ),
        Value::Bool(false)
    ));
}

#[test]
fn incomplete_type_var_comparison_faults_even_on_the_same_handle() {
    let variable = ReflectedVariable {
        name: "A".into(),
        arity: 0,
        meaning: reflection_bindings::VariableMeaning::Opened(EvalRef::new(
            reflection_bindings::OpenedScheme::Incomplete,
        )),
    };
    let ctx = EvalCtx::new();
    let result = apply(
        Value::Primitive(ComptimeBuiltin::TypeVarEqual),
        vec![
            Value::ReflTypeVar(variable.clone()),
            Value::ReflTypeVar(variable),
        ],
        &ctx,
    );
    assert!(
        comptime_fault_message(&result, &ctx).is_some(),
        "{result:?}"
    );
}
