use super::*;

fn unit() -> EvalAstType {
    Type::Unit { meta: zero_meta() }
}
fn typ(name: &str) -> EvalAstType {
    Type::synth_path(vec![name.to_owned()], Vec::new(), zero_span())
}
fn raw(name: &str, ty: EvalAstType) -> EvalRef<CheckedTerm> {
    EvalRef::new(CheckedTerm::new(path_expr(name), ty))
}
fn type_name(ty: &EvalAstType) -> String {
    match ty {
        Type::Path { segments, .. } => segments[0].name.clone(),
        Type::Product { left, right, .. } => format!("{}*{}", type_name(left), type_name(right)),
        Type::Unit { .. } => "unit".to_owned(),
        _ => unreachable!(),
    }
}

#[test]
fn replay_preserves_packet_prefix_effects_and_preorder_spans_on_fallback() {
    let left = raw("left", typ("L"));
    let right = raw("right", typ("R"));
    let packet = EvalRef::new(CheckedTerm::from_node(
        CheckedTermNode::IntrinsicPair { left, right },
        unit(),
    ));
    let function = EvalRef::new(CheckedTerm::from_node(
        CheckedTermNode::TermTypeApp {
            fn_value: raw("callee", unit()),
            arg: typ("T"),
        },
        unit(),
    ));
    let node = CheckedTermNode::TermCall {
        fn_value: function,
        params: vec![typ("P0"), typ("P1"), typ("P2")],
        arg_packet: packet,
    };
    let events = RefCell::new(Vec::new());
    let mut next = 0;
    let span = Span::new(100, 140);
    let result = instantiate(
        &node,
        &[],
        span,
        &mut next,
        &mut |expr, raw_span| {
            assert_eq!(raw_span, span);
            let Expr::Path { segments, .. } = expr else {
                unreachable!()
            };
            events
                .borrow_mut()
                .push(format!("raw:{}", segments[0].name));
            crate::ast::convert_expr::<EvalPhase, PrePrime>(expr)
        },
        &mut |ty| {
            events.borrow_mut().push(format!("type:{}", type_name(ty)));
            convert_type::<EvalPhase, PrePrime>(ty)
        },
    );
    assert_eq!(
        events.into_inner(),
        [
            "raw:callee",
            "type:T",
            "raw:left",
            "type:L",
            "type:R",
            "raw:left",
            "raw:right",
            "type:P0",
            "type:P1*P2",
            "type:P1",
            "type:P2"
        ]
    );
    assert_eq!(next, 6);
    let Expr::Call { args, meta, .. } = result else {
        unreachable!()
    };
    let mut start = 0;
    assert_eq!(meta.span, checked_template_span(span, &mut start));
    assert_eq!(args.len(), 4);
    assert!(
        matches!(&args[0], CallArg::Type(ty) if matches!(ty, Type::Path { segments, .. } if segments[0].name == "T"))
    );
    for arg in &args[1..] {
        let CallArg::Value(Expr::Call { callee, .. }) = arg else {
            unreachable!()
        };
        assert!(
            matches!(callee.as_ref(), Expr::Path { segments, .. } if matches!(segments.last().unwrap().name.as_str(), "__fst__" | "__snd__"))
        );
    }
}

fn deep_lets(depth: usize) -> EvalRef<CheckedTerm> {
    let value = EvalRef::new(CheckedTerm::new(
        Expr::Unit {
            occurrence: Default::default(),
            meta: zero_meta(),
        },
        unit(),
    ));
    let mut body = value.clone();
    for index in 0..depth {
        body = EvalRef::new(CheckedTerm::from_node(
            CheckedTermNode::TermLet {
                name: format!("v{index}"),
                value: value.clone(),
                body,
            },
            unit(),
        ));
    }
    body
}

#[test]
fn replay_unwind_releases_pending_deep_results_and_fallback_packets() {
    let body = deep_lets(10_000);
    let node = CheckedTermNode::IntrinsicPair {
        left: body.clone(),
        right: raw("tripwire", unit()),
    };
    let mut raw_calls = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        instantiate(
            &node,
            &[],
            zero_span(),
            &mut 0,
            &mut |expr, _| {
                raw_calls += 1;
                assert!(
                    !matches!(expr, Expr::Path { .. }),
                    "tripwire after deep left result"
                );
                crate::ast::convert_expr::<EvalPhase, PrePrime>(expr)
            },
            &mut convert_type::<EvalPhase, PrePrime>,
        )
    }));
    assert!(result.is_err());
    assert_eq!(raw_calls, 10_002);

    let node = CheckedTermNode::TermCall {
        fn_value: raw("callee", unit()),
        params: vec![unit(), unit(), unit()],
        arg_packet: body,
    };
    let mut conversions = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        instantiate(
            &node,
            &[],
            zero_span(),
            &mut 0,
            &mut |expr, _| crate::ast::convert_expr::<EvalPhase, PrePrime>(expr),
            &mut |ty| {
                conversions += 1;
                assert!(conversions < 3, "tripwire after one complete projection");
                convert_type::<EvalPhase, PrePrime>(ty)
            },
        )
    }));
    assert!(result.is_err());
    assert_eq!(conversions, 3);
    let result = instantiate(
        &CheckedTermNode::Local {
            name: "reused".to_owned(),
        },
        &[],
        zero_span(),
        &mut 0,
        &mut |expr, _| crate::ast::convert_expr::<EvalPhase, PrePrime>(expr),
        &mut convert_type::<EvalPhase, PrePrime>,
    );
    assert!(matches!(result, Expr::Path { segments, .. } if segments[0].name == "reused"));
}

#[test]
fn replay_copies_deep_template_inputs_without_changing_metadata() {
    let mut input = Expr::IntLit {
        occurrence: Default::default(),
        digits: "17".to_owned(),
        annotation: convert_type::<EvalPhase, PrePrime>(&typ("I32")),
        meta: Meta::new(Span::new(40, 50)),
    };
    for index in 0..10_000 {
        input = Expr::Let {
            occurrence: Default::default(),
            name: format!("v{index}"),
            name_span: Span::new(11, 19),
            ty: None,
            pattern: (),
            value: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(Span::new(23, 29)),
            }),
            body: Box::new(input),
            meta: Meta::new(Span::new(10, 80)),
        };
    }
    let input = Owned::new(input);
    let output = instantiate(
        &CheckedTermNode::TemplateValue { index: 0 },
        std::slice::from_ref(&input),
        zero_span(),
        &mut 0,
        &mut |_, _| unreachable!(),
        &mut |_| unreachable!(),
    );
    let output = Owned::new(output);
    let mut source = &*input;
    let mut copied = &*output;
    let mut count = 0;
    while let Expr::Let {
        body,
        value,
        name,
        name_span,
        meta,
        ..
    } = source
    {
        let Expr::Let {
            body: copy_body,
            value: copy_value,
            name: copy_name,
            name_span: copy_span,
            meta: copy_meta,
            ..
        } = copied
        else {
            unreachable!()
        };
        assert_eq!(copy_name, name);
        assert_eq!(copy_span, name_span);
        assert_eq!(copy_meta, meta);
        assert_eq!(copy_value, value);
        source = body;
        copied = copy_body;
        count += 1;
    }
    assert_eq!(count, 10_000);
    assert_eq!(copied, source);
}

#[test]
fn signature_converter_unwind_releases_completed_deep_annotations() {
    let signature = Signature::new(
        ["first", "second"]
            .into_iter()
            .map(|name| Param {
                name: name.to_owned(),
                ty: Some(unit()),
                pattern: (),
                meta: zero_meta(),
            })
            .map(SignatureParam::Value)
            .collect(),
    );
    let mut conversions = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        instantiate_checked_signature(&signature, zero_span(), &mut |_| {
            conversions += 1;
            assert_eq!(conversions, 1, "tripwire after the first annotation");
            let mut ty = Type::Unit {
                meta: Meta::<PrePrime>::new(zero_span()),
            };
            for _ in 0..10_000 {
                ty = Type::Forall {
                    param: TypeParam {
                        name: "A".to_owned(),
                        span: zero_span(),
                        kind: None,
                    },
                    body: Box::new(ty),
                    meta: Meta::new(zero_span()),
                };
            }
            ty
        })
    }));
    assert!(result.is_err());
    assert_eq!(conversions, 2);
}
