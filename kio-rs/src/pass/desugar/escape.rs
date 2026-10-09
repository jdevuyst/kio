use super::*;

pub(super) fn boundary_escape_call(
    source: &Expr<Surface>,
) -> Option<std::sync::Arc<crate::ast::RecCallDiagnostic>> {
    rec_call(source, false, |modes| modes.contains(&RecCallMode::Escape)).map(call_diagnostic)
}

pub(super) fn escape_span(source: &Expr<Surface>) -> Option<Span> {
    rec_call(source, true, |modes| modes.contains(&RecCallMode::Escape)).map(Expr::span)
}

pub(super) fn required_tail_call(
    source: &Expr<Surface>,
) -> Option<std::sync::Arc<crate::ast::RecCallDiagnostic>> {
    rec_call(source, true, |modes| !modes.contains(&RecCallMode::Escape)).map(call_diagnostic)
}

fn call_diagnostic(source: &Expr<Surface>) -> std::sync::Arc<crate::ast::RecCallDiagnostic> {
    let Expr::RecCall {
        modes,
        callee,
        ext,
        meta,
        ..
    } = source
    else {
        unreachable!("the source scan selects a recursive call")
    };
    std::sync::Arc::new(crate::ast::RecCallDiagnostic {
        syntax: ext.clone(),
        modes: modes.clone(),
        callee: callee.span,
        span: meta.span,
    })
}

fn rec_call(
    source: &Expr<Surface>,
    descend_lambdas: bool,
    accepts: impl Fn(&[RecCallMode]) -> bool,
) -> Option<&Expr<Surface>> {
    let mut pending = vec![source];
    while let Some(source) = pending.pop() {
        match source {
            Expr::RecCall { modes, args, .. } => {
                if accepts(modes) {
                    return Some(source);
                }
                pending.extend(args.iter().filter_map(value_argument));
            }
            Expr::Call { callee, args, .. } => {
                pending.extend(args.iter().filter_map(value_argument));
                pending.push(callee);
            }
            Expr::Let { value, body, .. }
            | Expr::Seq { value, body, .. }
            | Expr::RowLet { value, body, .. } => {
                pending.push(body);
                pending.push(value);
            }
            Expr::UserElaborator { args, .. } => {
                pending.extend(args.iter().filter_map(value_argument))
            }
            Expr::Ufcs { receiver, args, .. } => {
                pending.extend(args.iter().filter_map(value_argument));
                pending.push(receiver);
            }
            Expr::Tuple { items, .. } => pending.extend(items),
            Expr::LabelValue { labels, .. } => {
                pending.extend(labels.iter().map(|label| &label.value))
            }
            Expr::Elaborator { call, .. } => match call {
                ElaboratorCall::FieldAccess { receiver, .. } => pending.push(receiver),
                ElaboratorCall::FieldUpdate { receiver, updates } => {
                    pending.extend(updates.iter().map(|update| &update.value));
                    pending.push(receiver);
                }
            },
            Expr::OpChain { kind, .. } => match kind {
                crate::ast::OpChainKind::Normal { slots, .. } => pending.extend(slots),
                crate::ast::OpChainKind::Variadic { elements, .. } => pending.extend(elements),
            },
            Expr::BlockCall { .. } => {
                unreachable!("trailing blocks are projected before recursion lowering")
            }
            Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => {
                if descend_lambdas {
                    pending.push(body);
                }
            }
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => {}
            Expr::RecOrder { ext, .. }
            | Expr::RecQuote { ext, .. }
            | Expr::EnrichedTuple { ext, .. }
            | Expr::EnrichedProject { ext, .. }
            | Expr::EnrichedInject { ext, .. }
            | Expr::EnrichedMatch { ext, .. }
            | Expr::EnrichedConditional { ext, .. }
            | Expr::EnrichedFieldGet { ext, .. }
            | Expr::LowHostCall { ext, .. }
            | Expr::LowModuleCall { ext, .. }
            | Expr::LowQualifiedModuleCall { ext, .. }
            | Expr::LowQualifiedNewtypeMember { ext, .. }
            | Expr::LowNewtypeCtor { ext, .. }
            | Expr::LowNewtypeProj { ext, .. }
            | Expr::LowClosureCall { ext, .. }
            | Expr::LowIndirectCall { ext, .. }
            | Expr::LowTypeApplication { ext, .. }
            | Expr::LowAbsurdCall { ext, .. }
            | Expr::LowCpsProjectorApply { ext, .. }
            | Expr::LowBoundRef { ext, .. }
            | Expr::LowHostFnValueRef { ext, .. }
            | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
        }
    }
    None
}

fn value_argument(arg: &CallArg<Surface>) -> Option<&Expr<Surface>> {
    match arg {
        CallArg::Value(value) => Some(value),
        CallArg::Type(_) => None,
    }
}

#[derive(Clone)]
pub(super) struct RecEscapePlan {
    pub(super) answer: TypeParam,
    pub(super) driver: String,
    pub(super) source_call: Span,
}

pub(super) struct RecEscapeContext {
    pub(super) group: std::sync::Arc<RecLoweringCtx>,
    pub(super) member: usize,
}

impl RecEscapePlan {
    pub(super) fn answer_type(&self) -> Type<Desugared> {
        Type::synth_path(vec![self.answer.name.clone()], Vec::new(), self.answer.span)
    }
}

impl RecLoweringCtx {
    pub(super) fn with_answer(&self, answer: Type<Desugared>) -> Self {
        let plan = self
            .escape
            .as_ref()
            .expect("answer-indexed recursion has an escape plan");
        let subst = std::collections::HashMap::from([(plan.answer.name.clone(), answer.clone())]);
        let mut ctx = self.clone();
        ctx.state_ty = subst_rec_type(&self.state_ty, &subst);
        ctx.ret_ty = answer;
        ctx.step_result_ty = subst_rec_type(&self.step_result_ty, &subst);
        ctx.state_type_args = self
            .state_type_args
            .iter()
            .map(|ty| subst_rec_type(ty, &subst))
            .collect();
        if let Some(resume) = &mut ctx.resume {
            resume.ty = subst_rec_type(&resume.ty, &subst);
        }
        ctx
    }

    pub(super) fn rec_driver_type(&self) -> Type<Desugared> {
        let escape = self.escape.as_ref().expect("re-entry has an answer binder");
        Type::Forall {
            param: escape.answer.clone(),
            body: Box::new(Type::synth_function(
                vec![self.state_ty.clone()],
                self.ret_ty.clone(),
                self.span,
            )),
            meta: Meta::new(self.span),
        }
    }
}

fn lambda(
    params: Vec<SignatureParam<Desugared>>,
    ret: Type<Desugared>,
    body: Expr<Desugared>,
    span: Span,
) -> Expr<Desugared> {
    Expr::FnExpr {
        occurrence: Default::default(),
        sig: Signature::new(params),
        ret_ty: Some(ret),
        body: Box::new(body),
        meta: Meta::new(span),
        caps: (),
    }
}

fn parameter(name: &str, ty: Type<Desugared>, span: Span) -> SignatureParam<Desugared> {
    SignatureParam::Value(Param {
        name: name.to_owned(),
        ty: Some(ty),
        pattern: (),
        meta: Meta::new(span),
    })
}

impl DesugarVisitor {
    pub(super) fn bind_rec_type_param(&mut self, param: TypeParam) -> TypeParam {
        let source = param.name.clone();
        let shadows_member = self.state.rec_group.as_ref().is_some_and(|group| {
            group.members[0]
                .type_params
                .iter()
                .any(|member| member.name == source)
        });
        let param = if shadows_member {
            let fresh =
                crate::pass::typecheck_core::fresh_type_var(&source, &self.state.module_names);
            self.state.module_names.insert(fresh.clone());
            param.renamed(fresh)
        } else {
            param
        };
        self.state
            .rec_type_renames
            .push((source, param.name.clone()));
        param
    }

    pub(super) fn bind_rec_lambda_types(&mut self, params: &mut [SignatureParam<Surface>]) {
        if self.state.rec_group.is_none() {
            return;
        }
        for param in params {
            if let SignatureParam::Type(ty) = param {
                *ty = self.bind_rec_type_param(ty.clone());
            }
        }
    }

    pub(super) fn rename_rec_type_path(&self, segments: &mut [PathSegment]) {
        if let [segment] = segments
            && let Some((_, target)) = self
                .state
                .rec_type_renames
                .iter()
                .rev()
                .find(|(source, _)| *source == segment.name)
        {
            segment.name = target.clone();
        }
    }

    pub(super) fn rec_escape_wrapper_fn(
        &mut self,
        member: &RecMemberPlan,
        ctx: &RecLoweringCtx,
        loop_path: Vec<PathSegment>,
        shared_step: &str,
    ) -> Result<FnDef<Desugared>, Error> {
        let span = ctx.span;
        let escape = ctx.escape.as_ref().expect("escaping wrapper has a plan");
        let driver_ty = ctx.rec_driver_type();
        let unit_ty = Type::Unit {
            meta: Meta::new(span),
        };
        let unit = || Expr::Unit {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            meta: Meta::new(span),
        };
        let again = format!("__rec_again{}__", span.start);
        let initial = format!("__rec_initial{}__", span.start);
        let current = format!("__rec_current{}__", span.start);
        let next = format!("__rec_next{}__", span.start);
        let ignored = format!("__rec_same_answer{}__", span.start);
        let ignored_unit = format!("__rec_unit{}__", span.start);
        let selected_path = Span::new(
            loop_path.first().unwrap().span.start,
            loop_path.last().unwrap().span.end,
        );
        let diagnostic = crate::ast::RecLoopDiagnostic::Reentry {
            selected_path,
            escape_call: escape.source_call,
        };

        // The bootstrap returns before the returned driver starts a run. Its
        // callback captures the bootstrap step, never a predecessor driver.
        let fresh_driver = Expr::synth_call(
            path_expr(again.clone(), span),
            vec![CallArg::Value(unit())],
            rec_generated_span(span, 30_000),
        );
        let fresh_run = Expr::synth_call(
            fresh_driver,
            vec![
                CallArg::Type(ctx.ret_ty.clone()),
                CallArg::Value(path_expr(next.clone(), span)),
            ],
            span,
        );
        let fresh_driver = lambda(
            vec![
                SignatureParam::Type(escape.answer.clone()),
                parameter(&next, ctx.state_ty.clone(), span),
            ],
            ctx.ret_ty.clone(),
            fresh_run,
            span,
        );
        let transition = call_path(
            vec![PathSegment::new(shared_step, span)],
            vec![
                CallArg::Type(ctx.ret_ty.clone()),
                CallArg::Value(fresh_driver),
                CallArg::Value(path_expr(current.clone(), span)),
            ],
            span,
        );
        let real_step = lambda(
            vec![
                parameter(
                    &ignored,
                    Type::synth_function(vec![ctx.state_ty.clone()], ctx.ret_ty.clone(), span),
                    span,
                ),
                parameter(&current, ctx.state_ty.clone(), span),
            ],
            ctx.step_result_ty.clone(),
            transition,
            span,
        );
        let real_run = call_path(
            loop_path.clone(),
            vec![
                CallArg::Type(ctx.state_ty.clone()),
                CallArg::Type(ctx.ret_ty.clone()),
                CallArg::Value(real_step),
                CallArg::Value(path_expr(initial.clone(), span)),
            ],
            span,
        )
        .with_call_diagnostic(diagnostic);
        let driver = lambda(
            vec![
                SignatureParam::Type(escape.answer.clone()),
                parameter(&initial, ctx.state_ty.clone(), span),
            ],
            ctx.ret_ty.clone(),
            real_run,
            span,
        );
        let bootstrap_step = lambda(
            vec![
                parameter(
                    &again,
                    Type::synth_function(vec![unit_ty.clone()], driver_ty.clone(), span),
                    span,
                ),
                parameter(&ignored_unit, unit_ty.clone(), span),
            ],
            sum_type(unit_ty.clone(), driver_ty.clone(), span),
            inject_step_return(driver, &unit_ty, &driver_ty, span),
            span,
        );
        let bootstrap = call_path(
            loop_path,
            vec![
                CallArg::Type(unit_ty),
                CallArg::Type(driver_ty),
                CallArg::Value(bootstrap_step),
                CallArg::Value(unit()),
            ],
            span,
        )
        .with_call_diagnostic(diagnostic);
        let entry_ctx = ctx.with_answer(member.wrapper_ret.clone());
        let initial_state = self.rec_initial_state(member, &entry_ctx)?;
        let body = Expr::synth_call(
            bootstrap,
            vec![
                CallArg::Type(member.wrapper_ret.clone()),
                CallArg::Value(initial_state),
            ],
            span,
        );
        Ok(FnDef {
            vis: member.vis.clone(),
            purity: crate::ast::Purity::Impure,
            name: member.name.clone(),
            sig: member.wrapper_sig.clone(),
            ret: member.wrapper_ret.clone(),
            ret_elided: member.ret_elided,
            body,
            meta: member.meta.clone(),
            doc: member.doc.clone(),
        })
    }

    pub(super) fn walk_escaped_rec_expr(
        &mut self,
        source: Expr<Surface>,
        member: &RecMemberPlan,
    ) -> Result<Expr<Desugared>, Error> {
        let group = self
            .state
            .rec_group
            .as_ref()
            .expect("recursive source is lowered inside its group")
            .clone();
        let previous = self.state.escaped_rec.replace(Box::new(RecEscapeContext {
            group,
            member: member.index,
        }));
        let result = self.walk_expr(source);
        self.state.escaped_rec = previous;
        result
    }

    pub(super) fn transform_rec_escape_call(
        &mut self,
        modes: Vec<RecCallMode>,
        callee: PathSegment,
        args: Vec<CallArg<Surface>>,
        syntax: RecCallSyntax,
        span: Span,
    ) -> Result<Expr<Desugared>, Error> {
        let context = self
            .state
            .escaped_rec
            .as_ref()
            .expect("escaping call has a lexical group");
        let ctx = context.group.clone();
        let current_member = context.member;
        let site = recursion_fixes::CallSite {
            syntax: &syntax,
            callee: callee.span,
        };
        if !modes.contains(&RecCallMode::Escape) {
            return Err(recursion_fixes::mode(Error::parse(span, "recursive call crosses a closure boundary").with_help("write `recur(escape) name(...)` to re-enter this recursive group from the closure"), site, &modes, RecCallMode::Escape, true));
        }
        let Some(member) = ctx.members.iter().find(|member| member.name == callee.name) else {
            return Err(Error::name_res(
                callee.span,
                format!(
                    "`recur {}` is not a member of this `rec(loop)` group",
                    callee.name
                ),
            ));
        };
        let (type_args, values) = self.rec_call_parts(member, args, span, &ctx)?;
        self.validate_rec_poly_mode(
            member,
            &ctx.members[current_member],
            &type_args,
            &modes,
            site,
            span,
        )?;
        let answer = instantiate_rec_ret_ty(member, &type_args);
        let entry_ctx = ctx.with_answer(answer.clone());
        let mut value_tys = instantiate_rec_value_tys(member, &type_args);
        let mut values = values
            .into_iter()
            .map(|value| self.walk_expr(value))
            .collect::<Result<Vec<_>, _>>()?;
        values.extend(rec_phantom_witness_values(
            &type_args,
            &member.phantom_witness_indices,
            span,
        ));
        value_tys.extend(rec_phantom_witness_tys(
            &type_args,
            &member.phantom_witness_indices,
            span,
        ));
        let result = format!("__rec_escape_result{}__", span.start);
        values.push(lambda(
            vec![parameter(&result, answer.clone(), span)],
            entry_ctx.step_result_ty.clone(),
            inject_step_return(path_expr(result, span), &entry_ctx.state_ty, &answer, span),
            span,
        ));
        value_tys.push(rec_cont_fn_type(&answer, &entry_ctx.step_result_ty, span));
        let payload = product_value_from_values(values, value_tys, span);
        let state = self.rec_wrap_state(
            member,
            entry_ctx.state_type_args.clone(),
            type_args,
            payload,
        );
        let state = inject_sum_arm(state, &rec_state_arm_types(&entry_ctx), member.index, span);
        let driver = &ctx
            .escape
            .as_ref()
            .expect("written escape selects a re-entry group")
            .driver;
        Ok(Expr::synth_call(
            path_expr(driver.clone(), span),
            vec![CallArg::Type(answer), CallArg::Value(state)],
            rec_generated_span(span, 30_001),
        ))
    }
}
