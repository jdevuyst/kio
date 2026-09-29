use super::*;

pub(super) struct Source<'code> {
    pub builtin: ComptimeBuiltin,
    pub args: Vec<Value>,
    pub code: Option<Code<'code>>,
    pub started: Option<Instant>,
    pub record: bool,
}

impl<'code> Source<'code> {
    pub(super) fn runtime(&self) -> &[Value] {
        if self.code.is_some() {
            &self.args
        } else {
            reflection_runtime_args(self.builtin, &self.args)
        }
    }

    pub fn handler(&self, index: usize) -> Handler<'code> {
        if let Some(code) = &self.code {
            return Handler::Expr(code.child(|expr| match expr {
                EvalExpr::ReflTermLet { body, .. }
                | EvalExpr::ReflTermFn { body, .. }
                | EvalExpr::ReflTermTypeFn { body, .. } => body,
                EvalExpr::ReflIntrinsicEither {
                    on_left, on_right, ..
                } => {
                    if index == 0 {
                        on_left
                    } else {
                        on_right
                    }
                }
                EvalExpr::ReflIntrinsicIfThenElse {
                    on_true, on_false, ..
                } => {
                    if index == 0 {
                        on_true
                    } else {
                        on_false
                    }
                }
                _ => unreachable!("compiled binder callback"),
            }));
        }
        let count = if matches!(
            self.builtin,
            ComptimeBuiltin::IntrinsicEither | ComptimeBuiltin::IntrinsicIfThenElse
        ) {
            2
        } else {
            1
        };
        let args = self.runtime();
        Handler::Value(args[args.len() - count + index].clone())
    }

    #[cfg(feature = "surface")]
    pub(super) fn proof(
        &self,
    ) -> Option<&crate::pass::typecheck_core::apply::fills::MarkedComptimeProof> {
        if self.code.is_some() {
            None
        } else {
            marked_comptime_proof_from_value(self.args.first()?)
        }
    }

    pub fn prepare(&self, ctx: &EvalCtx<'_>) -> Option<(Finish, EvalArgs)> {
        let header_len = match self.builtin {
            ComptimeBuiltin::TermLet => 2,
            ComptimeBuiltin::TermFn | ComptimeBuiltin::TermTypeFn | ComptimeBuiltin::TypeForall => {
                1
            }
            _ => unreachable!("binder callback operation"),
        };
        let args = self.runtime();
        if args.len() != header_len + usize::from(self.code.is_none()) {
            return None;
        }
        #[cfg(feature = "surface")]
        if self
            .args
            .iter()
            .any(|arg| matches!(arg, Value::Projected(_)))
        {
            let proof = self.proof()?;
            match self.builtin {
                ComptimeBuiltin::TermLet => {
                    let name = fresh_checked_term_value_name("let");
                    let local = crate::pass::typecheck_core::apply::fills::projected_term_local(
                        proof,
                        (&args[0], &args[1]),
                        name.clone(),
                    )
                    .ok()?;
                    return Some((
                        Finish::ProjectedLet { name },
                        EvalArgs::One([projected_value(local)]),
                    ));
                }
                ComptimeBuiltin::TermFn => {
                    let types = crate::pass::typecheck_core::apply::fills::projected_term_fn_parameter_types(proof, &args[0]).ok()?;
                    let mut names = Vec::with_capacity(types.len());
                    let mut locals = Vec::with_capacity(types.len());
                    for (index, typ) in types.iter().enumerate() {
                        let name = fresh_checked_term_value_name(&format!("arg{index}"));
                        let typ = projected_value(typ.clone());
                        locals.push(
                            crate::pass::typecheck_core::apply::fills::projected_term_local(
                                proof,
                                (&typ, &args[0]),
                                name.clone(),
                            )
                            .ok()?,
                        );
                        names.push(name);
                    }
                    let packet = projected_checked_product_packet(proof, &locals)?;
                    return Some((Finish::ProjectedFunction { names }, EvalArgs::One([packet])));
                }
                _ => return None,
            }
        }
        match self.builtin {
            ComptimeBuiltin::TermLet => {
                let value_type = canonical_refl_type(&args[0], ctx)?;
                let value = as_checked_term(&args[1])?;
                if !checked_type_equiv(&value, &value_type, ctx) {
                    return None;
                }
                let name = fresh_checked_term_value_name("let");
                let local = checked_term_local_value(name.clone(), value_type);
                Some((Finish::Let { name, value }, EvalArgs::One([local])))
            }
            ComptimeBuiltin::TermFn => {
                let function_type = canonical_refl_type(&args[0], ctx)?;
                let (types, result_type) = split_canonical_function_params(&function_type, ctx)?;
                let mut params = Vec::with_capacity(types.len());
                for (index, typ) in types.into_iter().enumerate() {
                    let name = fresh_checked_term_value_name(&format!("arg{index}"));
                    params.push(EvalRef::new(CheckedTerm::from_node(
                        CheckedTermNode::Local { name },
                        typ,
                    )));
                }
                let packet = Value::CheckedTerm(checked_product_packet(&params));
                Some((
                    Finish::Function {
                        function_type: EvalRef::new(function_type),
                        result_type: EvalRef::new(result_type),
                        params,
                    },
                    EvalArgs::One([packet]),
                ))
            }
            ComptimeBuiltin::TermTypeFn | ComptimeBuiltin::TypeForall => {
                let arity = as_refl_type_arity(&args[0])?;
                let name =
                    fresh_checked_term_name(if self.builtin == ComptimeBuiltin::TypeForall {
                        "forall"
                    } else {
                        "type"
                    });
                let var = Value::ReflTypeVar {
                    name: name.clone(),
                    arity,
                };
                Some((Finish::TypeBinder { name, arity }, EvalArgs::One([var])))
            }
            _ => unreachable!("binder callback operation"),
        }
    }
}

pub(super) struct Callback<'code> {
    pub source: Source<'code>,
    pub finish: Finish,
}

pub(super) enum Finish {
    Let {
        name: String,
        value: EvalRef<CheckedTerm>,
    },
    Function {
        function_type: EvalRef<EvalAstType>,
        result_type: EvalRef<EvalAstType>,
        params: Vec<EvalRef<CheckedTerm>>,
    },
    TypeBinder {
        name: String,
        arity: usize,
    },
    #[cfg(feature = "surface")]
    ProjectedLet {
        name: String,
    },
    #[cfg(feature = "surface")]
    ProjectedFunction {
        names: Vec<String>,
    },
}

impl Finish {
    pub fn finish(self, source: &Source<'_>, body: Value, ctx: &EvalCtx<'_>) -> Option<Value> {
        let args = source.runtime();
        match self {
            #[cfg(feature = "surface")]
            Finish::ProjectedLet { name } => {
                let term = crate::pass::typecheck_core::apply::fills::projected_term_let(
                    source.proof()?,
                    &args[0],
                    &args[1],
                    name,
                    &body,
                )
                .ok()?;
                Some(projected_value(term))
            }
            #[cfg(feature = "surface")]
            Finish::ProjectedFunction { names } => {
                let term = crate::pass::typecheck_core::apply::fills::projected_term_fn(
                    source.proof()?,
                    &args[0],
                    names,
                    &body,
                )
                .ok()?;
                Some(projected_value(term))
            }
            Finish::Let { name, value } => {
                #[cfg(feature = "surface")]
                if matches!(body, Value::Projected(_)) {
                    let term = crate::pass::typecheck_core::apply::fills::projected_term_let(
                        source.proof()?,
                        &args[0],
                        &args[1],
                        name,
                        &body,
                    )
                    .ok()?;
                    return Some(projected_value(term));
                }
                let body = as_checked_term(&body)?;
                let ty = body.clone_type();
                Some(checked_term_node_value(
                    CheckedTermNode::TermLet { name, value, body },
                    ty,
                ))
            }
            Finish::Function {
                function_type,
                result_type,
                params,
            } => {
                #[cfg(feature = "surface")]
                if matches!(body, Value::Projected(_)) {
                    let names = params
                        .iter()
                        .map(|param| {
                            let CheckedTermNode::Local { name } = param.node.as_ref() else {
                                unreachable!("prepared local")
                            };
                            name.clone()
                        })
                        .collect();
                    let term = crate::pass::typecheck_core::apply::fills::projected_term_fn(
                        source.proof()?,
                        &args[0],
                        names,
                        &body,
                    )
                    .ok()?;
                    return Some(projected_value(term));
                }
                let body = as_checked_term(&body)?;
                if !checked_type_equiv(&body, &result_type, ctx) {
                    return None;
                }
                let count = params.len();
                let params = params
                    .into_iter()
                    .map(|param| {
                        let CheckedTermNode::Local { name } = param.node.as_ref() else {
                            unreachable!("prepared local")
                        };
                        SignatureParam::Value(Param {
                            name: name.clone(),
                            ty: Some(param.clone_type()),
                            pattern: Default::default(),
                            meta: zero_meta(),
                        })
                    })
                    .collect();
                Some(checked_term_node_value(
                    CheckedTermNode::TermFn {
                        sig: Signature::from_parts(
                            params,
                            vec![crate::ast::SignatureGroupKind::Value { len: count }],
                        ),
                        body,
                    },
                    function_type.unwrap_or_clone(),
                ))
            }
            Finish::TypeBinder { name, arity } => {
                #[cfg(feature = "surface")]
                if matches!(body, Value::Projected(_)) {
                    let term = if source.builtin == ComptimeBuiltin::TypeForall {
                        crate::pass::typecheck_core::apply::fills::projected_type_forall_finish(
                            source.proof()?,
                            name,
                            arity,
                            &body,
                        )
                    } else {
                        crate::pass::typecheck_core::apply::fills::projected_term_type_fn_finish(
                            source.proof()?,
                            name,
                            arity,
                            &body,
                        )
                    }
                    .ok()?;
                    return Some(projected_value(term));
                }
                let param = TypeParam {
                    name,
                    span: zero_span(),
                    kind: Some(crate::ast::Kind::arrow_chain(arity)),
                };
                if source.builtin == ComptimeBuiltin::TypeForall {
                    let body = canonical_refl_type(&body, ctx)?;
                    Some(refl_type_value(Type::Forall {
                        param,
                        body: Box::new(body),
                        meta: zero_meta(),
                    }))
                } else {
                    Some(Value::CheckedTerm(EvalRef::new(checked_term_type_fn(
                        param,
                        as_checked_term(&body)?,
                    ))))
                }
            }
        }
    }
}
