use super::{
    Env, EvalCtx, Expr, PathSegment, Value, apply, data_value, eval, find_module_fn,
    lowered_package, resolve_module_newtype_member, value_to_repl_string, value_to_string,
    zero_span,
};

#[test]
fn repl_rendering_requalifies_distinct_newtype_owners_without_alias_collisions() {
    let pkg = lowered_package(&[
        (
            "a/one.kio",
            "module a/one; pub newtype Wrap : . { pub constructor mk; pub projector get; };",
        ),
        (
            "b/two.kio",
            "module b/two; pub newtype Wrap : . { pub constructor mk; pub projector get; };",
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
            "module inner/a; pub newtype Wrap : . { pub constructor mk; pub projector get; };",
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
             newtype Hidden[A, B, C] : A & B & C { constructor mk; projector get; };",
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
