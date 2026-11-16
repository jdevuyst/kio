//! Written evaluator type operands with lexical substitutions in ordinary slots.

use super::*;

#[derive(Debug, Clone)]
pub(super) struct EvalTypeTemplate {
    ty: ReflectedType,
    bindings: Vec<(String, usize)>,
}

impl EvalTypeTemplate {
    pub(super) fn is_closed(&self) -> bool {
        self.bindings.is_empty()
    }

    pub(super) fn expression(self: &Arc<Self>) -> EvalExpr {
        if self.is_closed() {
            EvalExpr::Const(Value::ReflType(self.ty.clone()))
        } else {
            EvalExpr::TypeValue(self.clone())
        }
    }

    pub(super) fn instantiate(&self, mut local: impl FnMut(usize) -> Option<Value>) -> Value {
        if self.bindings.is_empty() {
            return Value::ReflType(self.ty.clone());
        }
        let actuals: Vec<(&str, ReflectedType)> = self
            .bindings
            .iter()
            .map(|(name, slot)| {
                let Value::ReflType(ty) =
                    local(*slot).expect("compiled type slot exists in its lexical layout")
                else {
                    panic!("compiled type stage must bind an actual type carrier");
                };
                (name.as_str(), ty)
            })
            .collect();
        Value::ReflType(substitute_reflected(&self.ty, &actuals))
    }

    #[cfg(test)]
    pub(super) fn unit() -> Arc<Self> {
        Arc::new(Self {
            ty: ReflectedType::new_trusted(Type::Unit { meta: zero_meta() }),
            bindings: Vec::new(),
        })
    }
}

pub(super) fn substitute_reflected(
    template: &ReflectedType,
    actuals: &[(&str, ReflectedType)],
) -> ReflectedType {
    if let Type::Path { segments, args, .. } = template.as_type()
        && args.is_empty()
        && let [head] = segments.as_slice()
        && let Some((_, actual)) = actuals.iter().find(|(name, _)| *name == head.as_str())
    {
        return actual.clone();
    }
    let mut builder = reflection_bindings::ReflectionTypeBuilder::with_environment(
        std::iter::once(template).chain(actuals.iter().map(|(_, ty)| ty)),
        template.environment.clone(),
    );
    let substitutions = actuals
        .iter()
        .map(|(name, ty)| (*name, ReflectedType::new_trusted(builder.import(ty).take())))
        .collect();
    builder.finish(substitute_template(template.as_type(), &substitutions))
}

pub(super) fn compile_type_template(
    ty: &EvalAstType,
    state: &EvalCompileState<'_, '_>,
) -> Option<Arc<EvalTypeTemplate>> {
    let ty = state
        .module
        .map(|owner| qualify_explicit_reflected_paths(ty, owner, &state.type_binders))
        .unwrap_or_else(|| type_traversal::clone_type(ty));
    let mut free = eval_refl_type_free_bare_heads(&ty);
    let mut bindings = Vec::new();
    for (slot, local) in state.locals.iter().enumerate().rev() {
        if local.is_type && free.remove(&local.name) {
            bindings.push((local.name.clone(), slot));
        }
    }
    bindings.reverse();
    Some(Arc::new(EvalTypeTemplate {
        ty: ReflectedType::new(ty, "`_` cannot be reflected at compile time").ok()?,
        bindings,
    }))
}

pub(super) fn collect_type_capture_slots(
    ty: &EvalAstType,
    outer_locals: &[EvalLocal],
    bound: &[EvalLocal],
    captures: &mut Vec<usize>,
) {
    let mut free = eval_refl_type_free_bare_heads(ty);
    for (slot, local) in outer_locals.iter().enumerate().rev() {
        if local.is_type
            && free.remove(&local.name)
            && !bound
                .iter()
                .rev()
                .any(|bound| bound.is_type && bound.name == local.name)
            && !captures.contains(&slot)
        {
            captures.push(slot);
        }
    }
}

pub(super) fn substitute_template(
    root: &EvalAstType,
    substitutions: &HashMap<&str, ReflectedType>,
) -> EvalAstType {
    use super::ownership::Owned;
    use super::type_traversal::{clone_kind, clone_type};
    if substitutions.is_empty() {
        return clone_type(root);
    }
    enum Work<'a> {
        Enter(&'a EvalAstType),
        Finish(&'a EvalAstType, usize),
        Forall(&'a TypeParam, &'a Meta<EvalPhase>, String),
    }
    let mut free = HashSet::new();
    for actual in substitutions.values() {
        free.extend(eval_refl_type_free_bare_heads(actual.as_type()));
    }
    let mut taken = free.clone();
    reflection_bindings::reserve_type_names(root, &mut taken);
    let mut bindings = HashMap::<&str, Vec<String>>::new();
    let mut work = vec![Work::Enter(root)];
    let mut values: Vec<Owned<EvalAstType>> = Vec::new();
    while let Some(next) = work.pop() {
        match next {
            Work::Enter(ty) => {
                if let Type::Forall { param, body, meta } = ty {
                    let name = if free.contains(&param.name) {
                        crate::pass::typecheck_core::fresh_type_var(&param.name, &taken)
                    } else {
                        param.name.clone()
                    };
                    taken.insert(name.clone());
                    bindings.entry(&param.name).or_default().push(name.clone());
                    work.push(Work::Forall(param, meta, name));
                    work.push(Work::Enter(body));
                    continue;
                }
                work.push(Work::Finish(ty, values.len()));
                match ty {
                    Type::Path { args, .. } => work.extend(args.iter().rev().map(Work::Enter)),
                    Type::Function { param, ret, .. } => {
                        work.push(Work::Enter(ret));
                        work.push(Work::Enter(param));
                    }
                    Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                        work.push(Work::Enter(right));
                        work.push(Work::Enter(left));
                    }
                    Type::Unit { .. } | Type::Bottom { .. } => {}
                    Type::Forall { .. } => unreachable!(),
                    Type::Infer { ext, .. } | Type::Goal { ext, .. } => match *ext {},
                }
            }
            Work::Forall(param, meta, name) => {
                let stack = bindings
                    .get_mut(param.name.as_str())
                    .expect("entered binder");
                stack.pop();
                if stack.is_empty() {
                    bindings.remove(param.name.as_str());
                }
                let body = values.pop().expect("completed binder body");
                values.push(Owned::new(Type::Forall {
                    param: TypeParam {
                        name,
                        presentation: param.presentation.clone(),
                        kind: param.kind.as_deref().map(clone_kind).map(Box::new),
                        span: param.span,
                    },
                    body: Box::new(body.take()),
                    meta: meta.clone(),
                }));
            }
            Work::Finish(ty, base) => {
                let mut ready = values.drain(base..);
                let mut child = || ready.next().expect("completed type child").take();
                let out = match ty {
                    Type::Path {
                        segments,
                        args,
                        meta,
                    } => {
                        let bound = if let [head] = segments.as_slice() {
                            bindings.get(head.as_str()).and_then(|names| names.last())
                        } else {
                            None
                        };
                        let replacement = if bound.is_none()
                            && let [head] = segments.as_slice()
                        {
                            substitutions.get(head.as_str())
                        } else {
                            None
                        };
                        if let Some(actual) = replacement {
                            if args.is_empty() {
                                clone_type(actual.as_type())
                            } else {
                                let Type::Path {
                                    segments,
                                    args: prefix,
                                    meta: actual_meta,
                                } = actual.as_type()
                                else {
                                    unreachable!(
                                        "checked higher-kind substitution has a path head"
                                    );
                                };
                                let mut arguments: Vec<Owned<EvalAstType>> =
                                    prefix.iter().map(|ty| Owned::new(clone_type(ty))).collect();
                                arguments.extend(args.iter().map(|_| Owned::new(child())));
                                Type::Path {
                                    segments: segments.clone(),
                                    args: arguments.into_iter().map(Owned::take).collect(),
                                    meta: actual_meta.clone(),
                                }
                            }
                        } else {
                            let mut segments = segments.clone();
                            if let Some(name) = bound {
                                segments[0].name = name.clone();
                            }
                            Type::Path {
                                segments,
                                args: args.iter().map(|_| child()).collect(),
                                meta: meta.clone(),
                            }
                        }
                    }
                    Type::Unit { meta } => Type::Unit { meta: meta.clone() },
                    Type::Bottom { meta } => Type::Bottom { meta: meta.clone() },
                    Type::Function {
                        abi_arity,
                        caps,
                        meta,
                        ..
                    } => Type::Function {
                        param: Box::new(child()),
                        ret: Box::new(child()),
                        abi_arity: *abi_arity,
                        caps: *caps,
                        meta: meta.clone(),
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
                    Type::Forall { .. } => unreachable!(),
                    Type::Infer { ext, .. } | Type::Goal { ext, .. } => match *ext {},
                };
                assert!(
                    ready.next().is_none(),
                    "type reconstruction consumes its children"
                );
                drop(ready);
                values.push(Owned::new(out));
            }
        }
    }
    values.pop().expect("template root").take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalization::artifact::tests::lowered_package;

    #[test]
    fn compound_type_arguments_preserve_distinct_same_spelled_lexical_references() {
        use super::super::reflection_bindings::{
            ReflectedVariable, TypeChild, semantic_type_equal,
        };
        let left = ReflectedVariable::live("A".into(), 0).as_type();
        let right = ReflectedVariable::live("A".into(), 0).as_type();
        let template = EvalTypeTemplate {
            ty: ReflectedType::new_trusted(Type::Product {
                left: Box::new(type_path("L")),
                right: Box::new(type_path("R")),
                meta: zero_meta(),
            }),
            bindings: vec![("L".into(), 0), ("R".into(), 1)],
        };
        let instantiate = |right: &ReflectedType| {
            let Value::ReflType(result) = template.instantiate(|slot| {
                Some(Value::ReflType(if slot == 0 {
                    left.clone()
                } else {
                    right.clone()
                }))
            }) else {
                panic!("type template result");
            };
            result
        };
        let distinct = instantiate(&right);
        assert!(semantic_type_equal(&distinct.child(TypeChild::Left), &left).unwrap());
        assert!(semantic_type_equal(&distinct.child(TypeChild::Right), &right).unwrap());
        assert!(
            !semantic_type_equal(
                &distinct.child(TypeChild::Left),
                &distinct.child(TypeChild::Right),
            )
            .unwrap()
        );
        let shared = instantiate(&left);
        assert!(
            semantic_type_equal(
                &shared.child(TypeChild::Left),
                &shared.child(TypeChild::Right),
            )
            .unwrap()
        );
    }

    #[test]
    fn higher_kind_template_keeps_lexical_head_and_incomplete_opened_argument() {
        use super::super::reflection_bindings::{
            IncompleteOwner, OpenedScheme, ReflectedVariable, TypeChild, VariableMeaning,
            require_determined, semantic_type_equal,
        };
        let head = ReflectedVariable::live("A".into(), 1).as_type();
        let argument = ReflectedVariable {
            name: "A".into(),
            arity: 0,
            meaning: VariableMeaning::Opened(EvalRef::new(OpenedScheme::Incomplete)),
        }
        .as_type();
        let template = EvalTypeTemplate {
            ty: ReflectedType::new_trusted(Type::synth_path(
                vec!["F".into()],
                vec![type_path("T")],
                zero_span(),
            )),
            bindings: vec![("F".into(), 0), ("T".into(), 1)],
        };
        let Value::ReflType(result) = template.instantiate(|slot| {
            Some(Value::ReflType(if slot == 0 {
                head.clone()
            } else {
                argument.clone()
            }))
        }) else {
            panic!("type template result");
        };
        assert_eq!(require_determined(&result), Err(IncompleteOwner));
        let child = result.child(TypeChild::Argument(0));
        assert!(child.free_variables()[0].same_reference(argument.free_variables()[0]));
        let mut result_head = type_traversal::clone_type(result.as_type());
        if let Type::Path { args, .. } = &mut result_head {
            for argument in std::mem::take(args) {
                drop(ownership::Owned::new(argument));
            }
        }
        let result_head =
            ReflectedType::from_parts(EvalRef::new(result_head), None, result.environment.clone());
        assert!(semantic_type_equal(&result_head, &head).unwrap());
    }

    #[test]
    fn actual_type_arguments_partition_shared_memo_results() {
        let artifact = lowered_package(&[(
            "x.kio",
            r#"
            mod x;
            import __comptime__;
            pure fn reflected[A](ct: __Comptime__) -> __Type__ {
                __reflect_type__(ct)[A]
            }
        "#,
        )]);
        let functions = SharedEvalFunctionCache::default();
        let memo = SharedExactCallMemoCache::default();
        let metrics = Arc::new(EvalMetrics::default());
        for bottom in [false, true, false, true] {
            let ctx =
                AuthenticatedEvalCtx::for_module(artifact.prepare_module("x").expect("module"))
                    .with_eval_function_cache(functions.clone())
                    .with_exact_call_memo_cache(memo.clone())
                    .with_metrics(metrics.clone());
            let ty = if bottom {
                Type::Bottom { meta: zero_meta() }
            } else {
                Type::Unit { meta: zero_meta() }
            };
            let call = Expr::synth_call(
                path_expr("reflected"),
                vec![CallArg::Type(ty.clone())],
                zero_span(),
            );
            let closure = eval_authenticated(&call, &Env::new(), &ctx);
            let Value::EvalClosure { captured, .. } = &closure else {
                panic!("type-only application must retain the value stage");
            };
            assert!(
                matches!(captured.as_slice(), [Value::ReflType(actual)] if actual.as_type() == &ty)
            );
            let result = apply(
                closure,
                vec![Value::Primitive(ComptimeBuiltin::ComptimeProof)],
                &ctx,
            );
            assert!(matches!(result, Value::ReflType(actual) if actual.as_type() == &ty));
        }
        let counts = metrics.snapshot();
        assert_eq!(counts.exact_call_memo_misses, 2, "{counts:?}");
        assert_eq!(counts.exact_call_memo_stores, 2, "{counts:?}");
        assert_eq!(counts.exact_call_memo_hits, 2, "{counts:?}");
    }

    #[test]
    fn private_type_stage_preserves_zero_value_stage_and_shared_body() {
        let artifact = lowered_package(&[(
            "x.kio",
            r#"
            mod x;
            pure fn keep[A](value: .) -> . { value }
        "#,
        )]);
        let ctx = AuthenticatedEvalCtx::for_module(artifact.prepare_module("x").expect("module"));
        let original = eval_authenticated(&path_expr("keep"), &Env::new(), &ctx);
        let Value::EvalClosure {
            function: original, ..
        } = original
        else {
            panic!("function");
        };
        assert_eq!(original.abi_arity(), 0, "the value domain is still Unit");
        assert_eq!(original.slot_arity(), 1, "one private type slot");
        let call = Expr::synth_call(
            path_expr("keep"),
            vec![CallArg::Type(Type::Unit { meta: zero_meta() })],
            zero_span(),
        );
        let specialized = eval_authenticated(&call, &Env::new(), &ctx);
        let Value::EvalClosure { function, captured } = &specialized else {
            panic!("value stage");
        };
        assert!(
            EvalRef::ptr_eq(&original.body, &function.body),
            "specialization shares compiled code"
        );
        assert_eq!(function.value_groups.len(), 1);
        assert!(!function.value_groups[0].type_arity.is_some());
        assert_eq!(function.abi_arity(), 0);
        assert!(matches!(captured.as_slice(), [Value::ReflType(_)]));
        let opaque = Value::Atom("unit-action".to_owned());
        assert!(
            matches!(apply(specialized, vec![opaque], &ctx), Value::Atom(name) if name == "unit-action")
        );
    }

    #[test]
    fn lexical_type_template_substitution_avoids_nested_capture() {
        let ty = Type::Forall {
            param: TypeParam {
                presentation: Default::default(),
                name: "B".to_owned(),
                kind: None,
                span: zero_span(),
            },
            body: Box::new(Type::Product {
                left: Box::new(type_path("A")),
                right: Box::new(type_path("B")),
                meta: zero_meta(),
            }),
            meta: zero_meta(),
        };
        let template = EvalTypeTemplate {
            ty: ReflectedType::new_trusted(ty),
            bindings: vec![("A".to_owned(), 0)],
        };
        let result = template.instantiate(|slot| {
            assert_eq!(slot, 0);
            Some(refl_type_value(type_path("B")))
        });
        let Value::ReflType(result) = result else {
            panic!("reflected type");
        };
        let Type::Forall { param, body, .. } = result.as_type() else {
            panic!("forall retained");
        };
        assert_ne!(param.name, "B", "the actual free argument stays free");
        let Type::Product { left, right, .. } = body.as_ref() else {
            panic!("product retained");
        };
        assert_eq!(left.as_ref(), &type_path("B"));
        assert_eq!(right.as_ref(), &type_path(&param.name));
    }

    #[test]
    fn generated_deep_actual_types_forward_and_substitute_on_the_default_stack() {
        let mut deep = Type::Unit { meta: zero_meta() };
        for _ in 0..10_000 {
            deep = Type::Forall {
                param: TypeParam {
                    presentation: Default::default(),
                    name: "B".into(),
                    kind: None,
                    span: zero_span(),
                },
                body: Box::new(deep),
                meta: zero_meta(),
            };
        }
        let actual = ReflectedType::new_trusted(deep);
        let forwarded = EvalTypeTemplate {
            ty: ReflectedType::new_trusted(type_path("A")),
            bindings: vec![("A".into(), 0)],
        }
        .instantiate(|_| Some(Value::ReflType(actual.clone())));
        let Value::ReflType(forwarded) = forwarded else {
            panic!("type");
        };
        assert!(
            EvalRef::ptr_eq(&actual.ty, &forwarded.ty),
            "bare forwarding shares the actual carrier"
        );
        let result = EvalTypeTemplate {
            ty: ReflectedType::new_trusted(Type::Product {
                left: Box::new(type_path("A")),
                right: Box::new(Type::Unit { meta: zero_meta() }),
                meta: zero_meta(),
            }),
            bindings: vec![("A".into(), 0)],
        }
        .instantiate(|_| Some(Value::ReflType(actual.clone())));
        let Value::ReflType(result) = result else {
            panic!("type");
        };
        let Type::Product { left, .. } = result.as_type() else {
            panic!("product");
        };
        let mut cursor = left.as_ref();
        let mut depth = 0;
        while let Type::Forall { body, .. } = cursor {
            depth += 1;
            cursor = body;
        }
        assert_eq!(depth, 10_000);
        assert!(matches!(cursor, Type::Unit { .. }));
        drop(result);
        drop(forwarded);
        drop(actual);
    }

    #[test]
    fn generated_deep_written_operand_compiles_and_substitutes_on_the_default_stack() {
        let mut deep = type_path("A");
        for _ in 0..10_000 {
            deep = Type::Forall {
                param: TypeParam {
                    presentation: Default::default(),
                    name: "B".into(),
                    kind: None,
                    span: zero_span(),
                },
                body: Box::new(deep),
                meta: zero_meta(),
            };
        }
        let source = ReflectedType::new_trusted(deep);
        let artifact = lowered_package(&[("x.kio", "mod x; pure fn keep(value: .) { value }")]);
        let ctx = AuthenticatedEvalCtx::for_module(artifact.prepare_module("x").expect("module"));
        for module in [None, ctx.root_module] {
            let state = EvalCompileState {
                ctx: &ctx,
                module,
                module_path: "x".into(),
                label_scope: None,
                locals: vec![EvalLocal::type_parameter("A".into())],
                type_binders: HashMap::from([("A".into(), crate::ast::Kind::Star)]),
                fn_stack: Vec::new(),
                source_generation: EvalSourceGeneration::fresh(),
                local_eval_function_cache: None,
                share_fn_closures: false,
            };
            let compiled = compile_type_template(source.as_type(), &state).expect("deep template");
            let result = compiled.instantiate(|slot| {
                assert_eq!(slot, 0);
                Some(refl_type_value(Type::Unit { meta: zero_meta() }))
            });
            let Value::ReflType(result) = result else {
                panic!("type");
            };
            let mut cursor = result.as_type();
            let mut depth = 0;
            while let Type::Forall { body, .. } = cursor {
                depth += 1;
                cursor = body;
            }
            assert_eq!(depth, 10_000);
            assert!(matches!(cursor, Type::Unit { .. }));
        }
    }
}
