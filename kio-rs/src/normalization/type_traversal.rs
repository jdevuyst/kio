//! Local traversals of types constructed by evaluator callbacks.

use super::ownership::Owned;
use super::*;
use crate::ast::Kind;

macro_rules! clone_phase_type {
    ($phase:ty, $children:ident, $clone_type:ident) => {
        fn $children<'a>(ty: &'a Type<$phase>, mut visit: impl FnMut(&'a Type<$phase>)) {
            match ty {
                Type::Path { args, .. } => args.iter().for_each(visit),
                Type::Unit { .. } | Type::Bottom { .. } => {}
                Type::Function { param, ret, .. } => {
                    visit(param);
                    visit(ret);
                }
                Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                    visit(left);
                    visit(right);
                }
                Type::Forall { body, .. } => visit(body),
                Type::LabelSugar { ext, .. } | Type::Infer { ext, .. } | Type::Goal { ext, .. } => {
                    match *ext {}
                }
            }
        }

        pub(super) fn $clone_type(root: &Type<$phase>) -> Type<$phase> {
            let mut pending = vec![(root, None)];
            let mut values: Vec<Owned<Type<$phase>>> = Vec::new();
            while let Some((ty, base)) = pending.pop() {
                let Some(base) = base else {
                    pending.push((ty, Some(values.len())));
                    let start = pending.len();
                    $children(ty, |child| pending.push((child, None)));
                    pending[start..].reverse();
                    continue;
                };
                let mut ready = values.drain(base..);
                let mut child = || {
                    ready
                        .next()
                        .expect("each type child was reconstructed")
                        .take()
                };
                let out = match ty {
                    Type::Path {
                        segments,
                        args,
                        meta,
                    } => Type::Path {
                        segments: segments.clone(),
                        args: args.iter().map(|_| child()).collect(),
                        meta: meta.clone(),
                    },
                    Type::Unit { meta } => Type::Unit { meta: meta.clone() },
                    Type::Bottom { meta } => Type::Bottom { meta: meta.clone() },
                    Type::Function {
                        meta,
                        abi_arity,
                        caps,
                        ..
                    } => Type::Function {
                        param: Box::new(child()),
                        ret: Box::new(child()),
                        meta: meta.clone(),
                        abi_arity: *abi_arity,
                        caps: *caps,
                    },
                    Type::Product { meta, .. } => Type::Product {
                        left: Box::new(child()),
                        right: Box::new(child()),
                        meta: meta.clone(),
                    },
                    Type::Sum { meta, .. } => Type::Sum {
                        left: Box::new(child()),
                        right: Box::new(child()),
                        meta: meta.clone(),
                    },
                    Type::Forall { param, meta, .. } => Type::Forall {
                        param: TypeParam {
                            name: param.name.clone(),
                            span: param.span,
                            kind: param.kind.as_ref().map(clone_kind),
                        },
                        body: Box::new(child()),
                        meta: meta.clone(),
                    },
                    Type::LabelSugar { ext, .. }
                    | Type::Infer { ext, .. }
                    | Type::Goal { ext, .. } => match *ext {},
                };
                assert!(
                    ready.next().is_none(),
                    "reconstructed type consumes every child"
                );
                drop(ready);
                values.push(Owned::new(out));
            }
            values.pop().expect("type root exists").take()
        }
    };
}
clone_phase_type!(EvalPhase, children, clone_type);
clone_phase_type!(PrePrime, pre_prime_children, clone_pre_prime_type);

pub(super) fn canonicalize_function_abi(root: &mut EvalAstType) {
    let mut pending = vec![root];
    while let Some(ty) = pending.pop() {
        match ty {
            Type::Function {
                param,
                ret,
                abi_arity,
                ..
            } => {
                *abi_arity = usize::from(!matches!(param.as_ref(), Type::Unit { .. }));
                pending.push(ret);
                pending.push(param);
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                pending.push(right);
                pending.push(left);
            }
            Type::Path { args, .. } => pending.extend(args.iter_mut().rev()),
            Type::Forall { body, .. } => pending.push(body),
            Type::Unit { .. } | Type::Bottom { .. } => {}
            Type::LabelSugar { ext, .. } | Type::Infer { ext, .. } | Type::Goal { ext, .. } => {
                match *ext {}
            }
        }
    }
}

pub(super) fn to_pre_prime(
    root: &EvalAstType,
    mut bound: HashSet<String>,
    mut qualify: impl FnMut(&EvalAstType, &HashSet<String>) -> Vec<PathSegment>,
) -> Type<PrePrime> {
    enum Work<'a> {
        Visit(&'a EvalAstType),
        Finish(&'a EvalAstType, usize),
        Unbind(&'a str),
    }
    let mut pending = vec![Work::Visit(root)];
    let mut values: Vec<Owned<Type<PrePrime>>> = Vec::new();
    while let Some(work) = pending.pop() {
        let (ty, base) = match work {
            Work::Visit(ty) => {
                if let Type::Forall { param, .. } = ty
                    && bound.insert(param.name.clone())
                {
                    pending.push(Work::Unbind(&param.name));
                }
                pending.push(Work::Finish(ty, values.len()));
                let start = pending.len();
                children(ty, |child| pending.push(Work::Visit(child)));
                pending[start..].reverse();
                continue;
            }
            Work::Unbind(name) => {
                bound.remove(name);
                continue;
            }
            Work::Finish(ty, base) => (ty, base),
        };
        // Qualification observes child paths first, exactly as the ordinary
        // requalifier does. Completed children stay owned across that callback.
        let segments = if let Type::Path { segments, meta, .. } = ty {
            Some(qualify(
                &Type::Path {
                    segments: segments.clone(),
                    args: Vec::new(),
                    meta: meta.clone(),
                },
                &bound,
            ))
        } else {
            None
        };
        let mut ready = values.drain(base..);
        let mut child = || ready.next().expect("each replay type child exists").take();
        let result = match ty {
            Type::Path { args, meta, .. } => Type::synth_path_segments(
                segments.expect("path was qualified"),
                args.iter().map(|_| child()).collect(),
                meta.span,
            ),
            Type::Unit { meta } => Type::Unit {
                meta: Meta::new(meta.span),
            },
            Type::Bottom { meta } => Type::Bottom {
                meta: Meta::new(meta.span),
            },
            Type::Function {
                meta, abi_arity, ..
            } => Type::Function {
                param: Box::new(child()),
                ret: Box::new(child()),
                meta: Meta::new(meta.span),
                abi_arity: *abi_arity,
                caps: (),
            },
            Type::Product { meta, .. } => Type::Product {
                left: Box::new(child()),
                right: Box::new(child()),
                meta: Meta::new(meta.span),
            },
            Type::Sum { meta, .. } => Type::Sum {
                left: Box::new(child()),
                right: Box::new(child()),
                meta: Meta::new(meta.span),
            },
            Type::Forall { param, meta, .. } => Type::Forall {
                param: TypeParam {
                    name: param.name.clone(),
                    span: param.span,
                    kind: param.kind.as_ref().map(clone_kind),
                },
                body: Box::new(child()),
                meta: Meta::new(meta.span),
            },
            Type::LabelSugar { ext, .. } | Type::Infer { ext, .. } | Type::Goal { ext, .. } => {
                match *ext {}
            }
        };
        assert!(ready.next().is_none(), "replay type consumes every child");
        drop(ready);
        values.push(Owned::new(result));
    }
    values.pop().expect("replay type root exists").take()
}

/// Charge the complete type/key structure before entering recursive key code.
/// Oversized reflected types remain evaluable but do not enter the call memo.
pub(super) fn charge_memo_type(ty: &EvalAstType, budget: &mut usize) -> Option<()> {
    enum Part<'a> {
        Type(&'a EvalAstType),
        Kind(&'a Kind),
    }
    let mut pending = vec![Part::Type(ty)];
    while let Some(part) = pending.pop() {
        *budget = budget.checked_sub(1)?;
        match part {
            Part::Type(ty) => {
                if let Type::Path { args, .. } = ty
                    && args.len() > *budget
                {
                    return None;
                }
                children(ty, |child| pending.push(Part::Type(child)));
                if let Type::Forall { param, .. } = ty
                    && let Some(kind) = &param.kind
                {
                    pending.push(Part::Kind(kind));
                }
            }
            Part::Kind(Kind::Star) => {}
            Part::Kind(Kind::Arrow(left, right)) => {
                pending.push(Part::Kind(left));
                pending.push(Part::Kind(right));
            }
        }
        if pending.len() > *budget {
            return None;
        }
    }
    Some(())
}

pub(super) fn clone_kind(root: &Kind) -> Kind {
    let mut pending = vec![(root, false)];
    let mut values: Vec<Owned<Kind>> = Vec::new();
    while let Some((kind, finish)) = pending.pop() {
        match kind {
            Kind::Star => values.push(Owned::new(Kind::Star)),
            Kind::Arrow(left, right) if !finish => {
                pending.push((kind, true));
                pending.push((right, false));
                pending.push((left, false));
            }
            Kind::Arrow(..) => {
                let right = values.pop().expect("right kind exists").take();
                let left = values.pop().expect("left kind exists").take();
                values.push(Owned::new(Kind::Arrow(Box::new(left), Box::new(right))));
            }
        }
    }
    values.pop().expect("kind root exists").take()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forall(body: EvalAstType, kind: Option<Kind>) -> EvalAstType {
        Type::Forall {
            param: TypeParam {
                name: "A".to_owned(),
                span: Span::new(31, 37),
                kind,
            },
            body: Box::new(body),
            meta: Meta::new(Span::new(29, 93)),
        }
    }

    #[test]
    fn replay_conversion_matches_qualification_abi_and_metadata() {
        use crate::ast::{HostType, Import, ImportItem, ImportKind};
        use crate::pass::typecheck_core::PrimeTypeRequalifier;

        let span = Span::new(13, 73);
        let module_path = |name| ModulePath {
            segments: vec![PathSegment::new(name, span)],
            span,
        };
        let binder = |name: &str| TypeParam {
            name: name.to_owned(),
            kind: Some(Kind::arrow_chain(2)),
            span,
        };
        let path = |module: &str, name: &str, args| {
            Type::synth_path_segments(
                vec![
                    PathSegment::new(module, Span::new(20, 27)),
                    PathSegment::new(name, Span::new(29, 37)),
                ],
                args,
                span,
            )
        };
        let module = Module::<EvalPhase> {
            path: module_path("local"),
            imports: vec![
                Import {
                    kind: ImportKind::Qualified {
                        path: module_path("remote"),
                        alias: "q".to_owned(),
                    },
                    span,
                    leading_trivia: Vec::new(),
                    trailing_trivia: Vec::new(),
                },
                Import {
                    kind: ImportKind::Selective {
                        from: module_path("selected"),
                        items: vec![ImportItem::Name {
                            name: "S".to_owned(),
                            span,
                            leading_trivia: Vec::new(),
                        }],
                    },
                    span,
                    leading_trivia: Vec::new(),
                    trailing_trivia: Vec::new(),
                },
            ],
            items: vec![Item::HostType(HostType {
                name: "Local".to_owned(),
                type_params: vec![binder("P")],
                role: None,
                owned: false,
                meta: Meta::new(span),
                doc: None,
            })],
            meta: Meta::new(span),
            doc: None,
        };
        let mut ty = path(
            "outer",
            "Root",
            vec![
                path("child", "Child", Vec::new()),
                Type::Forall {
                    param: binder("q"),
                    body: Box::new(Type::Forall {
                        param: binder("q"),
                        body: Box::new(path("remote", "R", Vec::new())),
                        meta: Meta::new(span),
                    }),
                    meta: Meta::new(span),
                },
                path("remote", "R", Vec::new()),
                Type::Forall {
                    param: binder("Local"),
                    body: Box::new(path("local", "Local", Vec::new())),
                    meta: Meta::new(span),
                },
                path("local", "Local", Vec::new()),
                path("selected", "S", Vec::new()),
                Type::Function {
                    param: Box::new(Type::Unit {
                        meta: Meta::new(span),
                    }),
                    ret: Box::new(Type::Function {
                        param: Box::new(Type::Product {
                            left: Box::new(Type::Unit { meta: zero_meta() }),
                            right: Box::new(Type::Bottom {
                                meta: Meta::new(span),
                            }),
                            meta: Meta::new(span),
                        }),
                        ret: Box::new(Type::Sum {
                            left: Box::new(path("local", "Prebound", Vec::new())),
                            right: Box::new(forall(path("selected", "S", Vec::new()), None)),
                            meta: zero_meta(),
                        }),
                        abi_arity: 9,
                        caps: (),
                        meta: Meta::new(span),
                    }),
                    abi_arity: 8,
                    caps: (),
                    meta: zero_meta(),
                },
            ],
        );
        let original = ty.clone();
        let mut canonical = ty.clone();
        crate::pass::typecheck_core::canonicalize_type_expression_function_abi(&mut canonical);
        canonicalize_function_abi(&mut ty);
        assert_eq!(ty, canonical);
        assert_ne!(ty, original);
        assert_eq!(
            replay_type_to_pre_prime(&ty, None),
            convert_type::<EvalPhase, PrePrime>(&ty)
        );

        let bound = HashSet::from(["Prebound".to_owned()]);
        let mut expected_policy = PrimeTypeRequalifier::new(&module);
        let expected = convert_type::<EvalPhase, PrePrime>(&expected_policy.rewrite(&ty, &bound));
        let mut actual_policy = PrimeTypeRequalifier::new(&module);
        let actual = replay_type_to_pre_prime(&ty, Some((&mut actual_policy, &bound)));
        assert_eq!(actual, expected);
        let imports = actual_policy.take_imports();
        assert_eq!(imports, expected_policy.take_imports());
        assert_eq!(imports.len(), 3);
        let aliases = actual_policy.take_local_aliases();
        assert_eq!(aliases, expected_policy.take_local_aliases());
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[0].params[0].kind, Some(Kind::arrow_chain(2)));
    }

    #[test]
    fn replay_conversion_scopes_binders_and_qualifies_children_before_parents() {
        let path = |name: &str, args| Type::synth_path(vec![name.to_owned()], args, zero_span());
        let bind = |name: &str, body| Type::Forall {
            param: TypeParam {
                name: name.to_owned(),
                kind: None,
                span: zero_span(),
            },
            body: Box::new(body),
            meta: zero_meta(),
        };
        let ty = path(
            "root",
            vec![
                bind(
                    "A",
                    path(
                        "outer",
                        vec![
                            bind("A", path("inner", Vec::new())),
                            path("sibling", Vec::new()),
                        ],
                    ),
                ),
                bind("Existing", path("prebound", Vec::new())),
                path("last", Vec::new()),
            ],
        );
        let mut events = Vec::new();
        let out = to_pre_prime(
            &ty,
            HashSet::from(["Existing".to_owned()]),
            |path, bound| {
                let Type::Path { segments, args, .. } = path else {
                    unreachable!()
                };
                assert!(args.is_empty());
                let mut names = bound.iter().map(String::as_str).collect::<Vec<_>>();
                names.sort_unstable();
                events.push(format!("{}:{}", segments[0].name, names.join(",")));
                segments.clone()
            },
        );
        assert_eq!(out, convert_type::<EvalPhase, PrePrime>(&ty));
        assert_eq!(
            events,
            [
                "inner:A,Existing",
                "sibling:A,Existing",
                "outer:A,Existing",
                "prebound:Existing",
                "last:Existing",
                "root:Existing"
            ]
        );
    }

    #[test]
    fn replay_type_callback_unwind_releases_deep_inputs_and_completed_children() {
        let mut deep = Type::Unit { meta: zero_meta() };
        for _ in 0..10_000 {
            deep = forall(deep, None);
        }
        let term = CheckedTerm::from_node(
            CheckedTermNode::TermTypeApp {
                fn_value: EvalRef::new(CheckedTerm::from_node(
                    CheckedTermNode::TermTypeApp {
                        fn_value: EvalRef::new(CheckedTerm::from_node(
                            CheckedTermNode::Local {
                                name: "f".to_owned(),
                            },
                            forall(forall(Type::Unit { meta: zero_meta() }, None), None),
                        )),
                        arg: Type::Function {
                            param: Box::new(Type::Unit { meta: zero_meta() }),
                            ret: Box::new(Type::Unit { meta: zero_meta() }),
                            abi_arity: 7,
                            caps: (),
                            meta: zero_meta(),
                        },
                    },
                    forall(Type::Unit { meta: zero_meta() }, None),
                )),
                arg: deep,
            },
            Type::Unit { meta: zero_meta() },
        );
        let mut conversions = 0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            term.instantiate_template_pre_prime(
                &[],
                zero_span(),
                &mut |_, _| unreachable!(),
                &mut |ty| {
                    conversions += 1;
                    if conversions == 1 {
                        assert!(matches!(ty, Type::Function { abi_arity: 0, .. }));
                        return replay_type_to_pre_prime(ty, None);
                    }
                    let mut depth = 0;
                    let mut cursor = ty;
                    while let Type::Forall { body, .. } = cursor {
                        depth += 1;
                        cursor = body;
                    }
                    assert_eq!(depth, 10_000);
                    let _converted = Owned::new(replay_type_to_pre_prime(ty, None));
                    panic!("tripwire after complete deep type conversion")
                },
            )
        }));
        assert!(result.is_err());
        assert_eq!(conversions, 2);
        let CheckedTermNode::TermTypeApp { arg, .. } = term.node.as_ref() else {
            unreachable!()
        };
        let root = Owned::new(Type::Product {
            left: Box::new(clone_type(arg)),
            right: Box::new(Type::synth_path(
                vec!["tripwire".to_owned()],
                Vec::new(),
                zero_span(),
            )),
            meta: zero_meta(),
        });
        let mut qualifications = 0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            to_pre_prime(&root, HashSet::new(), |_, _| {
                qualifications += 1;
                panic!("tripwire after deep left child conversion")
            })
        }));
        assert!(result.is_err());
        assert_eq!(qualifications, 1);
        assert!(matches!(
            replay_type_to_pre_prime(&Type::Unit { meta: zero_meta() }, None),
            Type::Unit { .. }
        ));
    }

    #[test]
    fn reflected_type_memo_budget_counts_structure_at_the_exact_boundary() {
        for (arity, admitted) in [(94, true), (95, true), (96, false)] {
            let value = refl_type_value(Type::synth_path(
                vec!["F".to_owned()],
                vec![Type::Unit { meta: zero_meta() }; arity],
                zero_span(),
            ));
            let mut budget = EXACT_CALL_MEMO_KEY_BUDGET;
            assert_eq!(value_memo_key(&value, &mut budget).is_some(), admitted);
            if admitted {
                assert_eq!(budget, EXACT_CALL_MEMO_KEY_BUDGET - arity - 1);
            }
        }
        let value = refl_type_value(forall(
            Type::Unit { meta: zero_meta() },
            Some(Kind::arrow_chain(1)),
        ));
        assert!(value_memo_key(&value, &mut 4).is_none());
        let mut budget = 5;
        assert!(value_memo_key(&value, &mut budget).is_some());
        assert_eq!(budget, 0);

        let values =
            vec![refl_type_value(Type::Unit { meta: zero_meta() }); EXACT_CALL_MEMO_KEY_BUDGET];
        let mut budget = EXACT_CALL_MEMO_KEY_BUDGET;
        assert_eq!(
            values_memo_keys(&values, &mut budget).unwrap().len(),
            EXACT_CALL_MEMO_KEY_BUDGET
        );
        assert_eq!(budget, 0);
        assert!(value_memo_key(&Value::Unit, &mut budget).is_none());
    }

    #[test]
    fn type_reconstruction_preserves_every_field_and_nested_kind() {
        let meta = Meta::new(Span::new(11, 97));
        let unit = Type::Unit { meta: meta.clone() };
        let bottom = Type::Bottom { meta: meta.clone() };
        let ty = Type::Function {
            param: Box::new(Type::Product {
                left: Box::new(unit.clone()),
                right: Box::new(bottom.clone()),
                meta: meta.clone(),
            }),
            ret: Box::new(Type::Sum {
                left: Box::new(forall(
                    Type::synth_path(vec!["F".into()], vec![unit, bottom], Span::new(50, 71)),
                    Some(Kind::arrow_chain(3)),
                )),
                right: Box::new(forall(Type::Unit { meta: zero_meta() }, None)),
                meta: meta.clone(),
            }),
            abi_arity: 2,
            caps: (),
            meta,
        };
        assert_eq!(clone_type(&ty), ty);

        let original = EvalRef::new(forall(
            Type::Unit { meta: zero_meta() },
            Some(Kind::arrow_chain(10_000)),
        ));
        let cloned = EvalRef::new(clone_type(&original));
        let Type::Forall { param, .. } = cloned.as_ref() else {
            unreachable!()
        };
        let mut cursor = param.kind.as_ref().unwrap();
        let mut depth = 0;
        while let Kind::Arrow(left, right) = cursor {
            assert!(matches!(left.as_ref(), Kind::Star));
            depth += 1;
            cursor = right;
        }
        assert_eq!(depth, 10_000);
        let mut budget = EXACT_CALL_MEMO_KEY_BUDGET;
        assert!(charge_memo_type(&cloned, &mut budget).is_none());
    }
}
