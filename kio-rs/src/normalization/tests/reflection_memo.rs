use super::*;

fn scheme(name: &str) -> EvalAstType {
    Type::Forall {
        param: TypeParam {
            name: name.into(),
            span: zero_span(),
            kind: None,
        },
        body: Box::new(type_path(name)),
        meta: zero_meta(),
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
    let [Value::ReflTypeVar { name, arity }, rest] = pair.args() else {
        panic!("forall binder: {pair:?}")
    };
    assert_eq!(name, expected);
    assert_eq!(*arity, 0);
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
fn exact_call_memo_does_not_replay_fresh_binders_through_helpers_or_contexts() {
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
    let mut names = HashSet::new();
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
        assert!(
            names.insert(param.name.clone()),
            "fresh result was replayed"
        );
    }
    assert!(cache.inner.lock().unwrap().is_empty());
    assert_eq!(metrics.snapshot().exact_call_memo_hits, 0);
    assert_eq!(metrics.snapshot().exact_call_memo_stores, 0);
    assert_eq!(metrics.snapshot().exact_call_memo_misses, 6);
}

#[test]
fn term_type_fn_mint_also_invalidates_enclosing_memo_ticket() {
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
    assert_eq!(
        ctx.reflected_binder_epoch
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    assert!(ctx.exact_call_memo.inner.lock().unwrap().is_empty());
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
    param.kind = Some(crate::ast::Kind::Star);
    param.span = Span::new(10, 20);
    meta.span = Span::new(5, 30);
    assert_eq!(ReflectionTypeKey::new(&a), ReflectionTypeKey::new(&b));
    let Type::Forall { param, .. } = &mut b else {
        unreachable!()
    };
    param.kind = Some(crate::ast::Kind::arrow_chain(2));
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
            param.kind = Some(kind);
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
