use super::*;
use crate::normalization::tests::test_eval_function;

#[test]
fn empty_application_does_not_retry_a_refused_reflection_callback() {
    let metrics = Arc::new(EvalMetrics::default());
    let ctx = EvalCtx::new().with_metrics(metrics.clone());
    let residual = crate::normalization::apply(
        Value::Primitive(ComptimeBuiltin::TypeForall),
        vec![
            Value::Primitive(ComptimeBuiltin::ComptimeProof),
            Value::ReflTypeArity(0),
            Value::EvalClosure {
                function: test_eval_function(
                    &["binder"],
                    EvalExpr::Seq {
                        value: EvalRef::new(EvalExpr::CallPrimitive {
                            builtin: ComptimeBuiltin::TypeUnit,
                            args: Vec::new(),
                        }),
                        body: EvalRef::new(EvalExpr::Unit),
                    },
                ),
                captured: Vec::new().into(),
            },
        ],
        &ctx,
    );
    assert!(matches!(residual, Value::Stuck(..)));
    let before = metrics.snapshot();
    assert_eq!(before.reflection_calls, 1);
    let result = crate::normalization::apply(residual.clone(), Vec::new(), &ctx);
    let after = metrics.snapshot();
    assert_eq!(after.apply_fn_calls, before.apply_fn_calls);
    assert_eq!(after.reflection_calls, before.reflection_calls);
    assert!(nf_eq(&residual, &result, &ctx));
}

#[test]
fn reflection_fold_constructs_deep_forall_types_on_the_default_stack() {
    let depth = 10_000;
    let body = EvalExpr::FnExpr {
        function: test_eval_function(&["binder"], EvalExpr::Local(0)),
        captured_slots: vec![0].into(),
        empty_closure_cache: Arc::new(OnceLock::new()),
    };
    let step = test_eval_function(
        &["acc"],
        EvalExpr::CallPrimitive {
            builtin: ComptimeBuiltin::TypeForall,
            args: vec![
                EvalRef::new(EvalExpr::Const(Value::ReflTypeArity(0))),
                EvalRef::new(body),
            ],
        },
    );
    let expr = EvalExpr::CallPrimitive {
        builtin: ComptimeBuiltin::TypeArityFold,
        args: vec![
            EvalRef::new(EvalExpr::Const(Value::ReflTypeArity(depth))),
            EvalRef::new(EvalExpr::ReflTypeUnit),
            EvalRef::new(EvalExpr::FnRef {
                cache_key: step.cache_key.clone(),
                function: step,
                share_closure: false,
            }),
        ],
    };
    eprintln!("starting ordinary reflection fold for {depth} forall layers");
    let ctx = EvalCtx::new();
    let value = eval(&expr, &mut EvalSlots::root(Vec::new()), &ctx);
    eprintln!("completed ordinary reflection fold before inspecting its result");
    let Value::ReflType(typ) = &value else {
        panic!("a successful reflected type fold returns a type")
    };
    let mut cursor = typ.as_type();
    let mut actual = 0;
    while let Type::Forall { body, .. } = cursor {
        actual += 1;
        cursor = body;
    }
    assert_eq!(actual, depth);
    assert!(matches!(cursor, Type::Unit { .. }));
    let Value::ReflType(typ) = value else {
        unreachable!()
    };
    let unit = Type::Unit { meta: zero_meta() };
    let function = CheckedTerm::from_node(
        CheckedTermNode::Local {
            name: "consume_type".to_owned(),
        },
        Type::Forall {
            param: TypeParam {
                name: "T".to_owned(),
                span: zero_span(),
                kind: None,
            },
            body: Box::new(unit.clone()),
            meta: zero_meta(),
        },
    );
    let term = CheckedTerm::from_node(
        CheckedTermNode::TermTypeApp {
            fn_value: EvalRef::new(function),
            arg: typ.into_type(),
        },
        unit,
    );
    let module = Module::<EvalPhase> {
        path: ModulePath {
            segments: vec![PathSegment::new("replay", zero_span())],
            span: zero_span(),
        },
        imports: Vec::new(),
        items: Vec::new(),
        meta: zero_meta(),
        doc: None,
    };
    let mut requalifier = crate::pass::typecheck_core::PrimeTypeRequalifier::new(&module);
    let binders = HashSet::new();
    eprintln!("entering real template replay with the generated {depth}-layer type");
    let output = ownership::Owned::new(term.instantiate_template_pre_prime(
        &[],
        zero_span(),
        &mut |expr, _| crate::ast::convert_expr::<EvalPhase, PrePrime>(expr),
        &mut |ty| {
            eprintln!("entered the production type requalification and conversion sequence");
            replay_type_to_pre_prime(ty, Some((&mut requalifier, &binders)))
        },
    ));
    let Expr::Call { args, .. } = &*output else {
        unreachable!()
    };
    let [CallArg::Type(typ)] = args.as_slice() else {
        unreachable!()
    };
    let mut cursor = typ;
    let mut actual = 0;
    while let Type::Forall { body, .. } = cursor {
        actual += 1;
        cursor = body;
    }
    assert_eq!(actual, depth);
    assert!(matches!(cursor, Type::Unit { .. }));
    drop(output);
    drop(term);
    eprintln!("generated forall type was replayed, verified and released");
}
