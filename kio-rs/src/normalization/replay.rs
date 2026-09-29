//! Checked recipes replay into the existing transient AST through one worklist.

use super::ownership::Owned;
use super::*;

#[cfg(test)]
mod tests;

enum Slot {
    Expr(Owned<Expr<PrePrime>>),
    Type(Owned<Type<PrePrime>>),
    Signature(Owned<Signature<PrePrime>>),
}

struct Reader<'a>(std::vec::Drain<'a, Slot>);

impl Reader<'_> {
    fn expr(&mut self) -> Expr<PrePrime> {
        let Some(Slot::Expr(value)) = self.0.next() else {
            unreachable!("replay expression result")
        };
        value.take()
    }

    fn ty(&mut self) -> Type<PrePrime> {
        let Some(Slot::Type(value)) = self.0.next() else {
            unreachable!("replay type result")
        };
        value.take()
    }

    fn signature(&mut self) -> Signature<PrePrime> {
        let Some(Slot::Signature(value)) = self.0.next() else {
            unreachable!("replay signature result")
        };
        value.take()
    }
}

enum Work<'a> {
    Node(&'a CheckedTermNode),
    Type(&'a EvalAstType),
    Signature(&'a EvalAstSignature, Span),
    Finish(&'a CheckedTermNode, Span, usize),
    Callee(&'a CheckedTerm),
    AfterCallee {
        packet: &'a CheckedTermNode,
        params: &'a [EvalAstType],
        span: Span,
        base: usize,
    },
    Packet {
        node: &'a CheckedTermNode,
        params: &'a [EvalAstType],
        root: &'a CheckedTermNode,
        all_params: &'a [EvalAstType],
        base: usize,
    },
    Project(&'a [EvalAstType]),
    FinishCall {
        span: Span,
        base: usize,
        types_end: usize,
    },
}

pub(super) fn instantiate<FR, FT>(
    node: &CheckedTermNode,
    values: &[Expr<PrePrime>],
    call_span: Span,
    next: &mut u32,
    raw_to_pre_prime: &mut FR,
    type_to_pre_prime: &mut FT,
) -> Expr<PrePrime>
where
    FR: FnMut(&Expr<EvalPhase>, Span) -> Expr<PrePrime>,
    FT: FnMut(&Type<EvalPhase>) -> Type<PrePrime>,
{
    let mut pending = vec![Work::Node(node)];
    let mut out = Vec::new();
    while let Some(work) = pending.pop() {
        match work {
            Work::Node(node) => {
                let node_span = checked_template_span(call_span, next);
                let base = out.len();
                if let CheckedTermNode::TermCall {
                    fn_value,
                    params,
                    arg_packet,
                } = node
                {
                    pending.push(Work::AfterCallee {
                        packet: &arg_packet.node,
                        params,
                        span: node_span,
                        base,
                    });
                    pending.push(Work::Callee(fn_value));
                    continue;
                }
                pending.push(Work::Finish(node, node_span, base));
                let start = pending.len();
                match node {
                    CheckedTermNode::TermLet { value, body, .. } => {
                        pending.push(Work::Node(&value.node));
                        pending.push(Work::Node(&body.node));
                    }
                    CheckedTermNode::TermFn { sig, body } => {
                        pending.push(Work::Signature(sig, node_span));
                        pending.push(Work::Node(&body.node));
                    }
                    CheckedTermNode::TermTypeApp { fn_value, arg } => {
                        pending.push(Work::Node(&fn_value.node));
                        pending.push(Work::Type(arg));
                    }
                    CheckedTermNode::IntrinsicPair { left, right } => {
                        pending.push(Work::Type(left.ty()));
                        pending.push(Work::Type(right.ty()));
                        pending.push(Work::Node(&left.node));
                        pending.push(Work::Node(&right.node));
                    }
                    CheckedTermNode::IntrinsicProjection {
                        left_ty,
                        right_ty,
                        value,
                        ..
                    }
                    | CheckedTermNode::IntrinsicInjection {
                        left_ty,
                        right_ty,
                        value,
                        ..
                    } => {
                        pending.push(Work::Type(left_ty));
                        pending.push(Work::Type(right_ty));
                        pending.push(Work::Node(&value.node));
                    }
                    CheckedTermNode::IntrinsicEither {
                        left_ty,
                        right_ty,
                        result_ty,
                        value,
                        left_body,
                        right_body,
                        ..
                    } => {
                        pending.push(Work::Type(left_ty));
                        pending.push(Work::Type(right_ty));
                        pending.push(Work::Type(result_ty));
                        pending.push(Work::Node(&value.node));
                        pending.push(Work::Node(&left_body.node));
                        pending.push(Work::Node(&right_body.node));
                    }
                    CheckedTermNode::IntrinsicAbsurd {
                        result_ty,
                        bottom_value,
                    } => {
                        pending.push(Work::Type(result_ty));
                        pending.push(Work::Node(&bottom_value.node));
                    }
                    CheckedTermNode::IntrinsicIfThenElse {
                        result_ty,
                        condition,
                        true_body,
                        false_body,
                    } => {
                        pending.push(Work::Type(result_ty));
                        pending.push(Work::Node(&condition.node));
                        pending.push(Work::Node(&true_body.node));
                        pending.push(Work::Node(&false_body.node));
                    }
                    CheckedTermNode::Raw(_)
                    | CheckedTermNode::TemplateValue { .. }
                    | CheckedTermNode::Local { .. }
                    | CheckedTermNode::ElabError { .. } => {}
                    CheckedTermNode::TermCall { .. } => unreachable!("call scheduled separately"),
                }
                pending[start..].reverse();
            }
            Work::Type(ty) => out.push(Slot::Type(Owned::new(type_to_pre_prime(ty)))),
            Work::Signature(sig, span) => out.push(Slot::Signature(Owned::new(
                instantiate_checked_signature(sig, span, type_to_pre_prime),
            ))),
            Work::Callee(term) => match term.node.as_ref() {
                CheckedTermNode::TermTypeApp { fn_value, arg } => {
                    pending.push(Work::Type(arg));
                    pending.push(Work::Callee(fn_value));
                }
                node => pending.push(Work::Node(node)),
            },
            Work::AfterCallee {
                packet,
                params,
                span,
                base,
            } => {
                let types_end = out.len();
                pending.push(Work::FinishCall {
                    span,
                    base,
                    types_end,
                });
                pending.push(Work::Packet {
                    node: packet,
                    params,
                    root: packet,
                    all_params: params,
                    base: types_end,
                });
            }
            Work::Packet {
                node,
                params,
                root,
                all_params,
                base,
            } => match params {
                [] | [_] => pending.push(Work::Node(node)),
                [_, rest @ ..] => {
                    if let CheckedTermNode::IntrinsicPair { left, right } = node {
                        pending.push(Work::Packet {
                            node: &right.node,
                            params: rest,
                            root,
                            all_params,
                            base,
                        });
                        pending.push(Work::Node(&left.node));
                    } else {
                        // The consumed prefix has already allocated spans and run
                        // converters. Fallback replays the whole packet after it.
                        out.truncate(base);
                        pending.push(Work::Project(all_params));
                        pending.push(Work::Node(root));
                    }
                }
            },
            Work::Project(params) => {
                let Some(Slot::Expr(packet)) = out.pop() else {
                    unreachable!("fallback packet result")
                };
                out.extend(
                    project_packet(packet, params, type_to_pre_prime)
                        .into_iter()
                        .map(Slot::Expr),
                );
            }
            Work::FinishCall {
                span,
                base,
                types_end,
            } => {
                let mut read = Reader(out.drain(base..));
                let callee = read.expr();
                let mut args = Vec::new();
                for _ in base + 1..types_end {
                    args.push(CallArg::Type(read.ty()));
                }
                while read.0.len() != 0 {
                    args.push(CallArg::Value(read.expr()));
                }
                drop(read);
                out.push(Slot::Expr(Owned::new(Expr::synth_call(callee, args, span))));
            }
            Work::Finish(node, node_span, base) => {
                let mut read = Reader(out.drain(base..));
                let value = build(
                    node,
                    values,
                    call_span,
                    node_span,
                    raw_to_pre_prime,
                    &mut read,
                );
                assert_eq!(
                    read.0.len(),
                    0,
                    "replay node consumes every prepared result"
                );
                drop(read);
                out.push(Slot::Expr(Owned::new(value)));
            }
        }
    }
    let mut read = Reader(out.drain(..));
    let value = read.expr();
    assert_eq!(read.0.len(), 0, "replay produces one root");
    value
}

fn build<FR>(
    node: &CheckedTermNode,
    values: &[Expr<PrePrime>],
    call_span: Span,
    node_span: Span,
    raw_to_pre_prime: &mut FR,
    read: &mut Reader<'_>,
) -> Expr<PrePrime>
where
    FR: FnMut(&Expr<EvalPhase>, Span) -> Expr<PrePrime>,
{
    match node {
        CheckedTermNode::Raw(expr) => raw_to_pre_prime(expr, call_span),
        CheckedTermNode::TemplateValue { index } => values
            .get(*index)
            .map(clone_expr)
            .unwrap_or_else(|| {
                unreachable!(
                    "checked elaborator template value index {index} is outside the current {}-value input",
                    values.len()
                )
            }),
        CheckedTermNode::Local { name } => path_expr_pre_prime(name, node_span),
        CheckedTermNode::TermLet { name, .. } => Expr::Let { occurrence: Default::default(),
            name: name.clone(),
            name_span: node_span,
            ty: None,
            pattern: (),
            value: Box::new(read.expr()),
            body: Box::new(read.expr()),
            meta: Meta::new(node_span),
        },
        CheckedTermNode::TermFn { .. } => Expr::FnExpr { occurrence: Default::default(),
            sig: read.signature(),
            ret_ty: None,
            body: Box::new(read.expr()),
            meta: Meta::new(node_span),
            caps: (),
        },
        CheckedTermNode::TermCall { .. } => unreachable!("calls have their own replay continuation"),
        CheckedTermNode::TermTypeApp { .. } => Expr::synth_call(
            read.expr(),
            vec![CallArg::Type(read.ty())],
            node_span,
        ),
        CheckedTermNode::IntrinsicPair { .. } => {
            let left_ty = read.ty();
            let right_ty = read.ty();
            let left_value = read.expr();
            let right_value = read.expr();
            checked_template_intrinsic_expr_at(
                "__pair__",
                vec![
                    CallArg::Type(left_ty),
                    CallArg::Type(right_ty),
                    CallArg::Value(left_value),
                    CallArg::Value(right_value),
                ],
                node_span,
            )
        }
        CheckedTermNode::IntrinsicProjection {
            intrinsic,
            ..
        } => {
            let left_ty = read.ty();
            let right_ty = read.ty();
            let value = read.expr();
            checked_template_intrinsic_expr_at(
                intrinsic,
                vec![
                    CallArg::Type(left_ty),
                    CallArg::Type(right_ty),
                    CallArg::Value(value),
                ],
                node_span,
            )
        }
        CheckedTermNode::IntrinsicInjection {
            intrinsic,
            ..
        } => {
            let left_ty = read.ty();
            let right_ty = read.ty();
            let value = read.expr();
            checked_template_intrinsic_expr_at(
                intrinsic,
                vec![
                    CallArg::Type(left_ty),
                    CallArg::Type(right_ty),
                    CallArg::Value(value),
                ],
                node_span,
            )
        }
        CheckedTermNode::IntrinsicEither { left_name, right_name, .. } => {
            let left_ty = read.ty();
            let right_ty = read.ty();
            let result_ty = read.ty();
            let value = read.expr();
            let left_body = read.expr();
            let right_body = read.expr();
            checked_template_intrinsic_expr_at(
                "__either__",
                vec![
                    CallArg::Type(type_traversal::clone_pre_prime_type(&left_ty)),
                    CallArg::Type(type_traversal::clone_pre_prime_type(&right_ty)),
                    CallArg::Type(result_ty),
                    CallArg::Value(value),
                    CallArg::Value(Expr::FnExpr { occurrence: Default::default(),
                        sig: Signature::new(vec![SignatureParam::Value(Param {
                            name: left_name.clone(),
                            ty: Some(left_ty),
                            pattern: Default::default(),
                            meta: Meta::new(node_span),
                        })]),
                        ret_ty: None,
                        body: Box::new(left_body),
                        meta: Meta::new(node_span),
                        caps: (),
                    }),
                    CallArg::Value(Expr::FnExpr { occurrence: Default::default(),
                        sig: Signature::new(vec![SignatureParam::Value(Param {
                            name: right_name.clone(),
                            ty: Some(right_ty),
                            pattern: Default::default(),
                            meta: Meta::new(node_span),
                        })]),
                        ret_ty: None,
                        body: Box::new(right_body),
                        meta: Meta::new(node_span),
                        caps: (),
                    }),
                ],
                node_span,
            )
        }
        CheckedTermNode::IntrinsicAbsurd { .. } => {
            let result_ty = read.ty();
            let bottom_value = read.expr();
            checked_template_intrinsic_expr_at(
                "__absurd__",
                vec![CallArg::Type(result_ty), CallArg::Value(bottom_value)],
                node_span,
            )
        }
        CheckedTermNode::IntrinsicIfThenElse { .. } => {
            let result_ty = read.ty();
            let condition = read.expr();
            let true_body = read.expr();
            let false_body = read.expr();
            checked_template_intrinsic_expr_at(
                "__if_then_else__",
                vec![
                    CallArg::Type(result_ty),
                    CallArg::Value(condition),
                    CallArg::Value(Expr::FnExpr { occurrence: Default::default(),
                        sig: Signature::from_groups(vec![crate::ast::SignatureGroup::Value(
                            Vec::new(),
                        )]),
                        ret_ty: None,
                        body: Box::new(true_body),
                        meta: Meta::new(node_span),
                        caps: (),
                    }),
                    CallArg::Value(Expr::FnExpr { occurrence: Default::default(),
                        sig: Signature::from_groups(vec![crate::ast::SignatureGroup::Value(
                            Vec::new(),
                        )]),
                        ret_ty: None,
                        body: Box::new(false_body),
                        meta: Meta::new(node_span),
                        caps: (),
                    }),
                ],
                node_span,
            )
        }
        CheckedTermNode::ElabError { .. } => Expr::Unit { occurrence: Default::default(),
            meta: Meta::new(node_span),
        },
    }
}

fn clone_expr(root: &Expr<PrePrime>) -> Expr<PrePrime> {
    let mut pending = vec![(root, None)];
    let mut out: Vec<Owned<Expr<PrePrime>>> = Vec::new();
    while let Some((expr, base)) = pending.pop() {
        let Some(base) = base else {
            pending.push((expr, Some(out.len())));
            let start = pending.len();
            match expr {
                Expr::Call { callee, args, .. } => {
                    pending.push((callee, None));
                    for arg in args {
                        if let CallArg::Value(value) = arg {
                            pending.push((value, None));
                        }
                    }
                }
                Expr::FnExpr { body, .. } => pending.push((body, None)),
                Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                    pending.push((value, None));
                    pending.push((body, None));
                }
                _ => {}
            }
            pending[start..].reverse();
            continue;
        };
        let mut ready = out.drain(base..);
        let mut child = || ready.next().expect("copied expression child").take();
        let ty = type_traversal::clone_pre_prime_type;
        let rebuilt = match expr {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Path { .. } | Expr::Unit { .. } => expr.clone(),
            Expr::Call {
                args, meta, ext, ..
            } => Expr::Call {
                occurrence: Default::default(),
                callee: Box::new(child()),
                args: args
                    .iter()
                    .map(|arg| match arg {
                        CallArg::Value(_) => CallArg::Value(child()),
                        CallArg::Type(arg) => CallArg::Type(ty(arg)),
                    })
                    .collect(),
                meta: meta.clone(),
                ext: *ext,
            },
            Expr::FnExpr {
                sig,
                ret_ty,
                meta,
                caps,
                ..
            } => Expr::FnExpr {
                occurrence: Default::default(),
                sig: Signature::from_parts(
                    sig.params
                        .iter()
                        .map(|param| match param {
                            SignatureParam::Type(param) => SignatureParam::Type(TypeParam {
                                name: param.name.clone(),
                                span: param.span,
                                kind: param.kind.as_ref().map(type_traversal::clone_kind),
                            }),
                            SignatureParam::Value(param) => SignatureParam::Value(Param {
                                name: param.name.clone(),
                                ty: param.ty.as_ref().map(ty),
                                pattern: param.pattern,
                                meta: param.meta.clone(),
                            }),
                        })
                        .collect(),
                    sig.groups.clone(),
                ),
                ret_ty: ret_ty.as_ref().map(ty),
                body: Box::new(child()),
                meta: meta.clone(),
                caps: *caps,
            },
            Expr::Let {
                name,
                name_span,
                ty: annotation,
                pattern,
                meta,
                ..
            } => Expr::Let {
                occurrence: Default::default(),
                name: name.clone(),
                name_span: *name_span,
                ty: annotation.as_ref().map(ty),
                pattern: *pattern,
                value: Box::new(child()),
                body: Box::new(child()),
                meta: meta.clone(),
            },
            Expr::Seq { meta, .. } => Expr::Seq {
                occurrence: Default::default(),
                value: Box::new(child()),
                body: Box::new(child()),
                meta: meta.clone(),
            },
            Expr::StrLit {
                occurrence: _,
                value,
                annotation,
                meta,
            } => Expr::StrLit {
                occurrence: Default::default(),
                value: value.clone(),
                annotation: ty(annotation),
                meta: meta.clone(),
            },
            Expr::IntLit {
                occurrence: _,
                digits,
                annotation,
                meta,
            } => Expr::IntLit {
                occurrence: Default::default(),
                digits: digits.clone(),
                annotation: ty(annotation),
                meta: meta.clone(),
            },
            Expr::FloatLit {
                occurrence: _,
                digits,
                annotation,
                meta,
            } => Expr::FloatLit {
                occurrence: Default::default(),
                digits: digits.clone(),
                annotation: ty(annotation),
                meta: meta.clone(),
            },
            Expr::BoolLit {
                occurrence: _,
                value,
                annotation,
                meta,
            } => Expr::BoolLit {
                occurrence: Default::default(),
                value: *value,
                annotation: ty(annotation),
                meta: meta.clone(),
            },
            Expr::RecCall { ext, .. }
            | Expr::RowLet { ext, .. }
            | Expr::Tuple { ext, .. }
            | Expr::FnPlaceholder { ext, .. }
            | Expr::LabelValue { ext, .. }
            | Expr::Elaborator { ext, .. }
            | Expr::RecOrder { ext, .. }
            | Expr::RecQuote { ext, .. }
            | Expr::UserElaborator { ext, .. }
            | Expr::Ufcs { ext, .. }
            | Expr::OpChain { ext, .. }
            | Expr::EnrichedTuple { ext, .. }
            | Expr::EnrichedProject { ext, .. }
            | Expr::EnrichedInject { ext, .. }
            | Expr::EnrichedMatch { ext, .. }
            | Expr::EnrichedConditional { ext, .. }
            | Expr::EnrichedRecord { ext, .. }
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
        };
        assert_eq!(ready.len(), 0, "copied expression consumes every child");
        drop(ready);
        out.push(Owned::new(rebuilt));
    }
    out.pop().expect("copied expression root").take()
}

fn project_packet<FT>(
    packet: Owned<Expr<PrePrime>>,
    params: &[EvalAstType],
    type_to_pre_prime: &mut FT,
) -> Vec<Owned<Expr<PrePrime>>>
where
    FT: FnMut(&EvalAstType) -> Type<PrePrime>,
{
    let mut packet = packet;
    let mut out = Vec::new();
    for (index, first) in params.iter().enumerate() {
        let rest = &params[index + 1..];
        let Some((last, prefix)) = rest.split_last() else {
            out.push(packet);
            return out;
        };
        let mut rest_ty = Owned::new(type_traversal::clone_type(last));
        for component in prefix.iter().rev() {
            rest_ty = Owned::new(Type::Product {
                left: Box::new(type_traversal::clone_type(component)),
                right: Box::new(rest_ty.take()),
                meta: zero_meta(),
            });
        }
        let first_ty = Owned::new(type_to_pre_prime(first));
        let rest_ty = Owned::new(type_to_pre_prime(&rest_ty));
        let fst = checked_template_intrinsic_expr(
            "__fst__",
            vec![
                CallArg::Type(type_traversal::clone_pre_prime_type(&first_ty)),
                CallArg::Type(type_traversal::clone_pre_prime_type(&rest_ty)),
                CallArg::Value(clone_expr(&packet)),
            ],
        );
        let snd = checked_template_intrinsic_expr(
            "__snd__",
            vec![
                CallArg::Type(first_ty.take()),
                CallArg::Type(rest_ty.take()),
                CallArg::Value(packet.take()),
            ],
        );
        out.push(Owned::new(fst));
        packet = Owned::new(snd);
    }
    out
}
