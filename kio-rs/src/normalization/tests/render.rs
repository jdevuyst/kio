use super::{
    Env, EvalCtx, EvalRef, Expr, PathSegment, Value, apply, data_value, eval, find_module_fn,
    lowered_package, resolve_module_newtype_member, value_to_repl_string, value_to_string,
    zero_span,
};

#[test]
fn repl_rendering_requalifies_distinct_newtype_owners_without_alias_collisions() {
    let pkg = lowered_package(&[
        (
            "a/one.kio",
            "module a/one; pub type { Wrap: . { pub mk; pub get; }; };",
        ),
        (
            "b/two.kio",
            "module b/two; pub type { Wrap: . { pub mk; pub get; }; };",
        ),
        (
            "left.kio",
            "module left; import a/one as _q0; fn keep() -> . { () }",
        ),
        (
            "right.kio",
            "module right; import b/two as _q0; fn keep() -> . { () }",
        ),
        ("occupied.kio", "module occupied; fn keep() -> . { () }"),
        (
            "consumer.kio",
            "module consumer; import occupied as _q1; fn _q0() -> . { () }",
        ),
    ]);
    let left = &pkg.module("left").expect("left module").module;
    let right = &pkg.module("right").expect("right module").module;
    let consumer = &pkg.module("consumer").expect("consumer module").module;
    let path = |parts: &[&str]| {
        parts
            .iter()
            .map(|part| PathSegment::synth((*part).to_owned(), zero_span()))
            .collect::<Vec<_>>()
    };
    let left_ctx = EvalCtx::for_module(&pkg, left, "left").expect("left context");
    let right_ctx = EvalCtx::for_module(&pkg, right, "right").expect("right context");

    let left_ctor = resolve_module_newtype_member(&left_ctx, left, &path(&["_q0", "Wrap", "mk"]))
        .expect("left imported constructor");
    let right_projector =
        resolve_module_newtype_member(&right_ctx, right, &path(&["_q0", "Wrap", "get"]))
            .expect("right imported projector");
    let residual = data_value(
        "__pair__",
        vec![
            apply(
                Value::NewtypeMember(left_ctor),
                vec![Value::Unit],
                &left_ctx,
            ),
            Value::NewtypeMember(right_projector),
        ],
    );

    assert_eq!(
        value_to_string(&residual),
        "__pair__(_q0.Wrap.mk(()), _q0.Wrap.get)"
    );
    assert_eq!(
        value_to_repl_string(&residual, consumer),
        "import a/one as _q2;\nimport b/two as _q3;\n\n__pair__(_q2.Wrap.mk(()), _q3.Wrap.get)"
    );
}

#[test]
fn repl_rendering_avoids_residual_atom_heads_when_allocating_imports() {
    let pkg = lowered_package(&[
        (
            "inner/a.kio",
            "module inner/a; pub type { Wrap: . { pub mk; pub get; }; };",
        ),
        ("consumer.kio", "module consumer; fn keep() -> . { () }"),
    ]);
    let inner = &pkg.module("inner/a").expect("inner/a module").module;
    let consumer = &pkg.module("consumer").expect("consumer module").module;
    let ctx = EvalCtx::for_module(&pkg, inner, "inner/a").expect("inner/a context");
    let path = ["Wrap", "mk"]
        .into_iter()
        .map(|part| PathSegment::synth(part.to_owned(), zero_span()))
        .collect::<Vec<_>>();
    let ctor = resolve_module_newtype_member(&ctx, inner, &path).expect("inner/a.Wrap.mk");
    let wrapped = apply(Value::NewtypeMember(ctor), vec![Value::Unit], &ctx);
    let residual = data_value("__pair__", vec![Value::Atom("_q0".to_owned()), wrapped]);

    assert_eq!(
        value_to_repl_string(&residual, consumer),
        "import inner/a as _q1;\n\n__pair__(_q0, _q1.Wrap.mk(()))"
    );
}

#[test]
fn repl_rendering_names_named_closures_and_suffix_parameters() {
    let pkg = lowered_package(&[
        (
            "provider.kio",
            "module provider; pub pure fn wrap(x: .)(y: .) -> . { x; y }",
        ),
        (
            "consumer.kio",
            "module consumer; import provider as p; fn expose() -> . { p.wrap }",
        ),
    ]);
    let consumer = &pkg.module("consumer").expect("consumer").module;
    let expose = find_module_fn(consumer, "expose").expect("expose fn");
    let ctx = EvalCtx::for_module(&pkg, consumer, "consumer").expect("consumer context");
    let closure = eval(&expose.body, &Env::new(), &ctx);

    assert_eq!(
        value_to_repl_string(&closure, consumer),
        "<closure provider.wrap(x)(y)>"
    );

    let suffix = apply(closure, vec![Value::Unit], &ctx);
    assert_eq!(
        value_to_repl_string(&suffix, consumer),
        "<closure provider.wrap(y)>"
    );
}

#[test]
fn repl_rendering_names_anonymous_closures_by_owner_and_source_span() {
    let pkg = lowered_package(&[
        (
            "provider.kio",
            "module provider; pub pure fn make() -> . -> . { .(z: .) -> . { z } }",
        ),
        (
            "consumer.kio",
            "module consumer; import provider as p; fn expose() -> . { p.make() }",
        ),
    ]);
    let provider = &pkg.module("provider").expect("provider").module;
    let make = find_module_fn(provider, "make").expect("make fn");
    let Expr::FnExpr { meta, .. } = &make.body else {
        panic!("make body should be a lambda");
    };
    let consumer = &pkg.module("consumer").expect("consumer").module;
    let expose = find_module_fn(consumer, "expose").expect("expose fn");
    let ctx = EvalCtx::for_module(&pkg, consumer, "consumer").expect("consumer context");
    let closure = eval(&expose.body, &Env::new(), &ctx);
    let expected = format!(
        "<closure provider.make::<fn@{}-{}>(z)>",
        meta.span.start, meta.span.end
    );

    assert_eq!(value_to_repl_string(&closure, consumer), expected);
}

#[test]
fn repl_rendering_preserves_private_saturated_and_residual_newtype_members() {
    let pkg = lowered_package(&[
        (
            "secret.kio",
            "module secret; \
             type { Hidden[A][ B][ C]: A & B & C { mk; get; }; };",
        ),
        ("consumer.kio", "module consumer; fn keep() -> . { () }"),
    ]);
    let secret = &pkg.module("secret").expect("secret").module;
    let consumer = &pkg.module("consumer").expect("consumer").module;
    let ctx = EvalCtx::for_module(&pkg, secret, "secret").expect("secret context");
    let path = |parts: &[&str]| {
        parts
            .iter()
            .map(|part| PathSegment::synth((*part).to_owned(), zero_span()))
            .collect::<Vec<_>>()
    };
    let ctor = resolve_module_newtype_member(&ctx, secret, &path(&["Hidden", "mk"]))
        .expect("private constructor");
    let projector = resolve_module_newtype_member(&ctx, secret, &path(&["Hidden", "get"]))
        .expect("private projector");
    let saturated = apply(
        Value::NewtypeMember(ctor.clone()),
        vec![
            Value::Atom("left".to_owned()),
            Value::Atom("middle".to_owned()),
            Value::Atom("right".to_owned()),
        ],
        &ctx,
    );
    let partial = apply(
        Value::NewtypeMember(ctor),
        vec![
            Value::Atom("partial_left".to_owned()),
            Value::Atom("partial_right".to_owned()),
        ],
        &ctx,
    );
    let residual_projector = apply(
        Value::NewtypeMember(projector),
        vec![Value::Atom("opaque".to_owned())],
        &ctx,
    );
    assert!(matches!(saturated, Value::Data(_)));
    assert!(matches!(partial, Value::Stuck(_, _)));
    assert!(matches!(residual_projector, Value::Stuck(_, _)));
    let residual = data_value(
        "__pair__",
        vec![
            saturated,
            data_value("__pair__", vec![partial, residual_projector]),
        ],
    );

    assert_eq!(
        value_to_repl_string(&residual, consumer),
        "import secret as _q0;\n\n\
         __pair__(_q0.Hidden.mk(__pair__(left, __pair__(middle, right))), __pair__(_q0.Hidden.mk(partial_left, partial_right), _q0.Hidden.get(opaque)))"
    );
}

fn diagnostic_handler_closures() -> (Value, Value) {
    let pkg = lowered_package(&[(
        "handlers.kio",
        "module handlers; \
         host fn foo() -> .; \
         host fn bar() -> .; \
         pure fn make(handler: . -> .) -> . -> . { .() { handler(()) } } \
         fn left() -> . -> . { make(foo) } \
         fn right() -> . -> . { make(bar) }",
    )]);
    let module = &pkg.module("handlers").expect("handlers module").module;
    let ctx = EvalCtx::for_module(&pkg, module, "handlers").expect("handlers context");
    let closure = |name| {
        let function = find_module_fn(module, name).expect("handler function");
        eval(&function.body, &Env::new(), &ctx)
    };
    (closure("left"), closure("right"))
}

#[test]
fn diagnostic_closures_expose_distinct_captured_handlers_without_forcing() {
    let (left, right) = diagnostic_handler_closures();
    let (
        Value::EvalClosure {
            function: left_function,
            captured: left_captured,
        },
        Value::EvalClosure {
            function: right_function,
            captured: right_captured,
        },
    ) = (&left, &right)
    else {
        panic!("the handler bodies remain suspended closures")
    };
    assert_eq!(
        left_function.diagnostic_body,
        right_function.diagnostic_body
    );
    assert!(matches!(left_captured.as_slice(), [Value::Atom(name)] if name.ends_with("foo")));
    assert!(matches!(right_captured.as_slice(), [Value::Atom(name)] if name.ends_with("bar")));
    let left = value_to_string(&left);
    let right = value_to_string(&right);
    assert_ne!(left, right, "captured handlers distinguish these residuals");
    assert!(left.contains("foo"), "{left}");
    assert!(right.contains("bar"), "{right}");
    assert!(left.contains("captured values"), "{left}");
}

#[test]
fn diagnostic_closure_capture_expansion_has_a_depth_bound() {
    let (closure, _) = diagnostic_handler_closures();
    let Value::EvalClosure { function, .. } = closure else {
        panic!("expected the suspended handler")
    };
    let mut nested = Value::Atom("leaf".to_owned());
    for _ in 0..64 {
        nested = Value::EvalClosure {
            function: function.clone(),
            captured: vec![nested].into(),
        };
    }
    let rendered = value_to_string(&nested);
    assert!(rendered.contains("captured value omitted: depth limit"));
    assert!(!rendered.contains("leaf"));
}

#[test]
fn diagnostic_captured_data_depth_is_bounded() {
    let (closure, _) = diagnostic_handler_closures();
    let Value::EvalClosure { function, .. } = closure else {
        panic!("expected the suspended handler")
    };
    let mut nested = Value::Atom("leaf".to_owned());
    for _ in 0..512 {
        nested = data_value("__pair__", vec![Value::Unit, nested]);
    }
    let captured = Value::EvalClosure {
        function,
        captured: vec![nested].into(),
    };
    let rendered = value_to_string(&captured);
    assert!(rendered.contains("captured value omitted: depth limit"));
    assert!(!rendered.contains("leaf"));
}

#[test]
fn diagnostic_captured_residual_depth_is_bounded() {
    let (closure, _) = diagnostic_handler_closures();
    let Value::EvalClosure { function, .. } = closure else {
        panic!("expected the suspended handler")
    };
    for kind in 0..3 {
        let mut nested = Value::Atom("leaf".to_owned());
        for _ in 0..512 {
            nested = match kind {
                0 => Value::Seq {
                    value: EvalRef::new(Value::Atom("effect".to_owned())),
                    body: EvalRef::new(nested),
                },
                1 => Value::Stuck(
                    EvalRef::new(Value::Atom("call".to_owned())),
                    vec![nested].into(),
                ),
                _ => Value::CaseSplit {
                    scrutinee: EvalRef::new(Value::Atom("subject".to_owned())),
                    left_payload: "left".to_owned(),
                    left: EvalRef::new(nested),
                    right_payload: "right".to_owned(),
                    right: EvalRef::new(Value::Unit),
                },
            };
        }
        let captured = Value::EvalClosure {
            function: function.clone(),
            captured: vec![nested].into(),
        };
        let rendered = value_to_string(&captured);
        assert!(rendered.contains("captured value omitted: depth limit"));
        assert!(!rendered.contains("leaf"));
    }
}

#[test]
fn diagnostic_capture_depth_limit_leaves_ordinary_values_unchanged() {
    let mut nested = Value::Atom("leaf".to_owned());
    for _ in 0..32 {
        nested = data_value("__pair__", vec![Value::Unit, nested]);
    }
    let rendered = value_to_string(&nested);
    assert!(rendered.contains("leaf"));
    assert!(!rendered.contains("depth limit"));
}
