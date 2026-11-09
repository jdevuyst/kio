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
                let value_type = reflected_type_carrier(&args[0])?.clone();
                let value = as_checked_term(&args[1])?;
                if !checked_reflected_type_equiv(&value, &value_type, self.builtin, ctx) {
                    return None;
                }
                let name = fresh_checked_term_value_name("let");
                let local = reflected_local_value(name.clone(), value_type);
                Some((Finish::Let { name, value }, EvalArgs::One([local])))
            }
            ComptimeBuiltin::TermFn => {
                let function_type = reflected_type_carrier(&args[0])?.clone();
                let (domains, result_type) = reflected_function_layout(&function_type, ctx)?;
                let params: Vec<_> = domains
                    .into_iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        let name = fresh_checked_term_value_name(&format!("arg{index}"));
                        EvalRef::new(CheckedTerm::from_reflected_node(
                            CheckedTermNode::Local { name },
                            ty,
                        ))
                    })
                    .collect();
                let packet = Value::CheckedTerm(checked_product_packet(&params));
                Some((
                    Finish::Function {
                        function_type,
                        result_type,
                        params,
                    },
                    EvalArgs::One([packet]),
                ))
            }
            ComptimeBuiltin::TermTypeFn | ComptimeBuiltin::TypeForall => {
                let arity = as_refl_type_arity(&args[0])?;
                let variable = ReflectedVariable::live("T".into(), arity);
                let var = Value::ReflTypeVar(variable.clone());
                Some((Finish::TypeBinder { variable }, EvalArgs::One([var])))
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
        function_type: ReflectedType,
        result_type: ReflectedType,
        params: Vec<EvalRef<CheckedTerm>>,
    },
    TypeBinder {
        variable: ReflectedVariable,
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
                let ty = body.ty.clone();
                reflected_checked_value(ProjectedTermConstruction::Let { name, value, body }, ty)
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
                if !checked_reflected_type_equiv(&body, &result_type, source.builtin, ctx) {
                    return None;
                }
                let params = params
                    .into_iter()
                    .map(|param| {
                        let CheckedTermNode::Local { name } = param.node.as_ref() else {
                            unreachable!("prepared local")
                        };
                        (name.clone(), param.ty.clone())
                    })
                    .collect();
                reflected_checked_value(
                    ProjectedTermConstruction::Fn {
                        params,
                        span: zero_span(),
                        body,
                    },
                    function_type,
                )
            }
            Finish::TypeBinder { variable } => {
                #[cfg(feature = "surface")]
                if matches!(body, Value::Projected(_)) {
                    let term = if source.builtin == ComptimeBuiltin::TypeForall {
                        crate::pass::typecheck_core::apply::fills::projected_type_forall_finish(
                            source.proof()?,
                            variable,
                            &body,
                        )
                    } else {
                        crate::pass::typecheck_core::apply::fills::projected_term_type_fn_finish(
                            source.proof()?,
                            variable,
                            &body,
                        )
                    }
                    .ok()?;
                    return Some(projected_value(term));
                }
                if source.builtin == ComptimeBuiltin::TypeForall {
                    let body = reflected_type_carrier(&body)?;
                    Some(match body.close_forall(&variable) {
                        Ok(ty) => Value::ReflType(ty),
                        Err(_) => reflection_constructor_fault(ctx),
                    })
                } else {
                    let body = as_checked_term(&body)?;
                    Some(match checked_bindings::close_forall(&body, &variable) {
                        Ok(term) => Value::CheckedTerm(EvalRef::new(term)),
                        Err(_) => reflection_constructor_fault(ctx),
                    })
                }
            }
        }
    }
}
