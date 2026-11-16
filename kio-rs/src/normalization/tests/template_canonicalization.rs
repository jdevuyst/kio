use super::*;

fn unit() -> Type<EvalPhase> {
    Type::Unit { meta: zero_meta() }
}

fn deep_forall(depth: usize) -> Type<EvalPhase> {
    let mut ty = unit();
    for index in 0..depth {
        ty = Type::Forall {
            param: TypeParam {
                name: format!("T{index}"),
                span: zero_span(),
                kind: None,
                presentation: Default::default(),
            },
            body: Box::new(ty),
            meta: zero_meta(),
        };
    }
    ty
}

fn application(arg: Type<EvalPhase>) -> CheckedTerm {
    CheckedTerm::from_node(
        CheckedTermNode::TermTypeApp {
            fn_value: EvalRef::new(CheckedTerm::from_node(
                CheckedTermNode::Local {
                    name: "polymorphic".to_owned(),
                },
                unit(),
            )),
            arg,
        },
        unit(),
    )
}

fn depth(term: &CheckedTerm) -> usize {
    let CheckedTermNode::TermTypeApp { arg, .. } = term.node.as_ref() else {
        panic!("canonicalization preserves type application")
    };
    let mut cursor = arg;
    let mut depth = 0;
    while let Type::Forall { body, .. } = cursor {
        depth += 1;
        cursor = body;
    }
    assert!(matches!(cursor, Type::Unit { .. }));
    depth
}

#[test]
fn template_canonicalization_handles_deep_type_arguments() {
    let term = application(deep_forall(10_000));
    let canonical = term.canonicalize_generated_names();
    assert_eq!(depth(&canonical), 10_000);
    drop(canonical);
    drop(term);
}

#[test]
fn template_canonicalization_materializes_deep_alias_bodies() {
    let mut module = parse_module("mod owner; type Deep = .;");
    let Item::TypeAlias(alias) = &mut module.module.items[0] else {
        panic!("fixture declares an alias")
    };
    alias.body = deep_forall(10_000);
    let term = application(Type::synth_path(
        vec!["owner".to_owned(), "Deep".to_owned()],
        Vec::new(),
        zero_span(),
    ));
    let canonical = term.canonicalize_template(&EvalCtx::new().with_root_module(&module));
    let Item::TypeAlias(alias) = &mut module.module.items[0] else {
        unreachable!()
    };
    let _body = ownership::Owned::new(std::mem::replace(&mut alias.body, unit()));
    assert_eq!(depth(&canonical), 10_000);
    drop(canonical);
    drop(term);
}

#[test]
fn template_canonicalization_preserves_qualified_nominals_and_rigid_heads() {
    let package = lowered_package(&[
        (
            "owner.kio",
            "mod owner; host type Actual; type Hidden = Actual;",
        ),
        (
            "caller.kio",
            "mod caller; import owner as different; host type Actual; type T = .; type F[A] = .;",
        ),
    ]);
    let caller = &package.module("caller").expect("caller").module;
    let ctx = EvalCtx::new()
        .with_package(Some(&package))
        .with_root_module(caller);
    let canonical_arg = |arg| {
        let term = application(arg);
        let canonical = term.canonicalize_template(&ctx);
        let CheckedTermNode::TermTypeApp { arg, .. } = canonical.node.as_ref() else {
            unreachable!()
        };
        type_traversal::clone_type(arg)
    };
    let path = |owner: &str, name: &str| {
        Type::synth_path(
            vec![owner.to_owned(), name.to_owned()],
            Vec::new(),
            zero_span(),
        )
    };
    assert_eq!(
        canonical_arg(path("owner", "Hidden")),
        path("owner", "Actual")
    );
    assert_eq!(
        canonical_arg(path("caller", "Actual")),
        path("caller", "Actual")
    );
    let rigid = Type::synth_path(vec!["F".to_owned()], vec![type_path("T")], zero_span());
    assert_eq!(canonical_arg(rigid.clone()), rigid);
}

#[test]
fn template_canonicalization_preserves_generated_binder_scope() {
    let source = Type::Forall {
        param: TypeParam {
            name: "_ct_type_n27".to_owned(),
            span: zero_span(),
            kind: None,
            presentation: Default::default(),
        },
        body: Box::new(Type::Product {
            left: Box::new(type_path("_ct_type_n27")),
            right: Box::new(Type::Forall {
                param: TypeParam {
                    name: "_ct_type_n27".to_owned(),
                    span: zero_span(),
                    kind: None,
                    presentation: Default::default(),
                },
                body: Box::new(type_path("_ct_type_n27")),
                meta: zero_meta(),
            }),
            meta: zero_meta(),
        }),
        meta: zero_meta(),
    };
    let canonical = application(source).canonicalize_template(&EvalCtx::new());
    let CheckedTermNode::TermTypeApp {
        arg: Type::Forall { param, body, .. },
        ..
    } = canonical.node.as_ref()
    else {
        panic!("outer forall")
    };
    let Type::Product { left, right, .. } = body.as_ref() else {
        panic!("product")
    };
    assert!(matches!(left.as_ref(), Type::Path { segments, .. } if segments[0].name == param.name));
    let Type::Forall {
        param: inner, body, ..
    } = right.as_ref()
    else {
        panic!("inner forall")
    };
    assert!(matches!(body.as_ref(), Type::Path { segments, .. } if segments[0].name == inner.name));
}
