//! Iterative consumers of the term graphs constructed by reflection callbacks.

use super::*;

pub(super) fn children<'a>(node: &'a CheckedTermNode, mut visit: impl FnMut(&'a CheckedTerm)) {
    match node {
        CheckedTermNode::Raw(_)
        | CheckedTermNode::TemplateValue { .. }
        | CheckedTermNode::Local { .. }
        | CheckedTermNode::ElabError { .. } => {}
        CheckedTermNode::TermLet { value, body, .. } => {
            visit(value);
            visit(body);
        }
        CheckedTermNode::TermFn { body, .. } => visit(body),
        CheckedTermNode::TermCall {
            fn_value,
            arg_packet,
            ..
        } => {
            visit(fn_value);
            visit(arg_packet);
        }
        CheckedTermNode::TermTypeApp { fn_value, .. } => visit(fn_value),
        CheckedTermNode::IntrinsicPair { left, right } => {
            visit(left);
            visit(right);
        }
        CheckedTermNode::IntrinsicProjection { value, .. }
        | CheckedTermNode::IntrinsicInjection { value, .. } => visit(value),
        CheckedTermNode::IntrinsicEither {
            value,
            left_body,
            right_body,
            ..
        } => {
            visit(value);
            visit(left_body);
            visit(right_body);
        }
        CheckedTermNode::IntrinsicAbsurd { bottom_value, .. } => visit(bottom_value),
        CheckedTermNode::IntrinsicIfThenElse {
            condition,
            true_body,
            false_body,
            ..
        } => {
            visit(condition);
            visit(true_body);
            visit(false_body);
        }
    }
}

pub(super) fn template_arity(
    root: &CheckedTermNode,
    memo: &mut HashMap<*const CheckedTermNode, usize>,
) -> usize {
    let mut pending = vec![(root, false)];
    while let Some((node, finish)) = pending.pop() {
        if memo.contains_key(&(node as *const _)) {
            continue;
        }
        if finish {
            let mut arity = if let CheckedTermNode::TemplateValue { index } = node {
                index + 1
            } else {
                0
            };
            children(node, |term| {
                arity = arity.max(memo[&(&*term.node as *const _)])
            });
            memo.insert(node, arity);
        } else {
            pending.push((node, true));
            children(node, |term| pending.push((&term.node, false)));
        }
    }
    memo[&(root as *const _)]
}

pub(super) fn materialize(root: &CheckedTermNode) -> EvalAstExpr {
    enum Work<'a> {
        Enter(&'a CheckedTermNode),
        Finish(&'a CheckedTermNode, usize),
    }
    let mut pending = vec![Work::Enter(root)];
    let mut values: Vec<EvalRef<EvalAstExpr>> = Vec::new();
    while let Some(work) = pending.pop() {
        match work {
            Work::Enter(node) => {
                pending.push(Work::Finish(node, values.len()));
                let start = pending.len();
                // Value application flattens the callee's leading type applications.
                if let CheckedTermNode::TermCall {
                    fn_value,
                    arg_packet,
                    ..
                } = node
                {
                    let mut callee = fn_value.as_ref();
                    while let CheckedTermNode::TermTypeApp { fn_value, .. } = callee.node.as_ref() {
                        callee = fn_value;
                    }
                    pending.push(Work::Enter(&callee.node));
                    pending.push(Work::Enter(&arg_packet.node));
                } else {
                    children(node, |term| pending.push(Work::Enter(&term.node)));
                }
                pending[start..].reverse();
            }
            Work::Finish(node, base) => {
                let expr = {
                    let mut ready = values.drain(base..);
                    let expr = checked_term_node_expr_with(node, |_| {
                        ready
                            .next()
                            .expect("each checked child was materialized")
                            .into_inner()
                            .expect("materialized children are uniquely owned")
                    });
                    assert!(
                        ready.next().is_none(),
                        "checked construction consumed all children"
                    );
                    expr
                };
                values.push(EvalRef::new(expr));
            }
        }
    }
    values
        .pop()
        .expect("a checked root was materialized")
        .into_inner()
        .expect("the materialized root is uniquely owned")
}

fn expr_head<'a>(
    expr: &'a EvalAstExpr,
    mut child: impl FnMut(&'a EvalAstExpr) -> EvalAstExpr,
) -> EvalAstExpr {
    match expr {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Call {
            occurrence: _,
            callee,
            args,
            meta,
            ext,
        } => Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(child(callee)),
            args: args
                .iter()
                .map(|arg| match arg {
                    CallArg::Value(value) => CallArg::Value(child(value)),
                    CallArg::Type(ty) => CallArg::Type(ty.clone()),
                })
                .collect(),
            meta: meta.clone(),
            ext: *ext,
        },
        Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps,
        } => Expr::FnExpr {
            occurrence: Default::default(),
            sig: sig.clone(),
            ret_ty: ret_ty.clone(),
            body: Box::new(child(body)),
            meta: meta.clone(),
            caps: *caps,
        },
        Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern,
            value,
            body,
            meta,
        } => Expr::Let {
            occurrence: Default::default(),
            name: name.clone(),
            name_span: *name_span,
            ty: ty.clone(),
            pattern: *pattern,
            value: Box::new(child(value)),
            body: Box::new(child(body)),
            meta: meta.clone(),
        },
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(child(value)),
            body: Box::new(child(body)),
            meta: meta.clone(),
        },
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => expr.clone(),
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
    }
}

fn replace_children(expr: &mut EvalAstExpr, mut child: impl FnMut() -> EvalAstExpr) {
    match expr {
        Expr::Call { callee, args, .. } => {
            **callee = child();
            for arg in args {
                if let CallArg::Value(value) = arg {
                    *value = child();
                }
            }
        }
        Expr::FnExpr { body, .. } => **body = child(),
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            **value = child();
            **body = child();
        }
        _ => {}
    }
}

pub(super) fn clone_expr(root: &EvalAstExpr) -> EvalAstExpr {
    enum Work<'a> {
        Enter(&'a EvalAstExpr),
        Finish(EvalRef<EvalAstExpr>, usize),
    }
    let mut pending = vec![Work::Enter(root)];
    let mut values: Vec<EvalRef<EvalAstExpr>> = Vec::new();
    while let Some(work) = pending.pop() {
        match work {
            Work::Enter(expr) => {
                let mut children = Vec::new();
                let head = expr_head(expr, |child| {
                    children.push(child);
                    Expr::Unit {
                        occurrence: Default::default(),
                        meta: zero_meta(),
                    }
                });
                pending.push(Work::Finish(EvalRef::new(head), values.len()));
                pending.extend(children.into_iter().rev().map(Work::Enter));
            }
            Work::Finish(mut head, base) => {
                let mut ready = values.drain(base..);
                replace_children(EvalRef::make_mut(&mut head), || {
                    ready
                        .next()
                        .expect("each source child was cloned")
                        .into_inner()
                        .expect("cloned source children are uniquely owned")
                });
                assert!(
                    ready.next().is_none(),
                    "source construction consumed all children"
                );
                drop(ready);
                values.push(head);
            }
        }
    }
    values
        .pop()
        .expect("a source root was cloned")
        .into_inner()
        .expect("the cloned source root is uniquely owned")
}

fn expr_equal(left: &EvalAstExpr, right: &EvalAstExpr) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        let mut left_children = Vec::new();
        let mut right_children = Vec::new();
        let left = EvalRef::new(expr_head(left, |child| {
            left_children.push(child);
            Expr::Unit {
                occurrence: Default::default(),
                meta: zero_meta(),
            }
        }));
        let right = EvalRef::new(expr_head(right, |child| {
            right_children.push(child);
            Expr::Unit {
                occurrence: Default::default(),
                meta: zero_meta(),
            }
        }));
        if left != right {
            return false;
        }
        assert_eq!(
            left_children.len(),
            right_children.len(),
            "equal source heads have equal child counts"
        );
        pending.extend(left_children.into_iter().zip(right_children).rev());
    }
    true
}

pub(super) fn equal(left: &CheckedTerm, right: &CheckedTerm) -> bool {
    let mut pending = vec![(left, right)];
    let mut templates = HashMap::new();
    while let Some((left, right)) = pending.pop() {
        if left.ty != right.ty {
            return false;
        }
        if template_arity(&left.node, &mut templates) == 0
            && template_arity(&right.node, &mut templates) == 0
        {
            let left = EvalRef::new(materialize(&left.node));
            let right = EvalRef::new(materialize(&right.node));
            if !expr_equal(&left, &right) {
                return false;
            }
        } else if !node_equal(&left.node, &right.node, &mut pending) {
            return false;
        }
    }
    true
}

fn node_equal<'a>(
    left: &'a CheckedTermNode,
    right: &'a CheckedTermNode,
    pending: &mut Vec<(&'a CheckedTerm, &'a CheckedTerm)>,
) -> bool {
    use CheckedTermNode::*;
    let same = match (left, right) {
        (Raw(left), Raw(right)) => expr_equal(left, right),
        (TemplateValue { index: left }, TemplateValue { index: right }) => left == right,
        (Local { name: left }, Local { name: right }) => left == right,
        (TermLet { name: left, .. }, TermLet { name: right, .. }) => left == right,
        (TermFn { sig: left, .. }, TermFn { sig: right, .. }) => left == right,
        (TermCall { params: left, .. }, TermCall { params: right, .. }) => left == right,
        (TermTypeApp { arg: left, .. }, TermTypeApp { arg: right, .. }) => left == right,
        (IntrinsicPair { .. }, IntrinsicPair { .. }) => true,
        (
            IntrinsicProjection {
                intrinsic: li,
                left_ty: ll,
                right_ty: lr,
                ..
            },
            IntrinsicProjection {
                intrinsic: ri,
                left_ty: rl,
                right_ty: rr,
                ..
            },
        )
        | (
            IntrinsicInjection {
                intrinsic: li,
                left_ty: ll,
                right_ty: lr,
                ..
            },
            IntrinsicInjection {
                intrinsic: ri,
                left_ty: rl,
                right_ty: rr,
                ..
            },
        ) => li == ri && ll == rl && lr == rr,
        (
            IntrinsicEither {
                left_ty: ll,
                right_ty: lr,
                result_ty: lt,
                left_name: ln,
                right_name: lm,
                ..
            },
            IntrinsicEither {
                left_ty: rl,
                right_ty: rr,
                result_ty: rt,
                left_name: rn,
                right_name: rm,
                ..
            },
        ) => ll == rl && lr == rr && lt == rt && ln == rn && lm == rm,
        (
            IntrinsicAbsurd {
                result_ty: left, ..
            },
            IntrinsicAbsurd {
                result_ty: right, ..
            },
        )
        | (
            IntrinsicIfThenElse {
                result_ty: left, ..
            },
            IntrinsicIfThenElse {
                result_ty: right, ..
            },
        ) => left == right,
        (
            ElabError {
                kind: lk,
                message: lm,
            },
            ElabError {
                kind: rk,
                message: rm,
            },
        ) => lk == rk && lm == rm,
        _ => false,
    };
    if same {
        let mut left_children = Vec::new();
        let mut right_children = Vec::new();
        children(left, |term| left_children.push(term));
        children(right, |term| right_children.push(term));
        assert_eq!(
            left_children.len(),
            right_children.len(),
            "equal checked heads have equal child counts"
        );
        pending.extend(left_children.into_iter().zip(right_children).rev());
    }
    same
}

struct CanonicalFrame<'a> {
    term: &'a CheckedTerm,
    node: EvalRef<CheckedTermNode>,
    next: usize,
    types: Vec<CanonicalBinding>,
    values: Vec<CanonicalBinding>,
}

impl<'a> CanonicalFrame<'a> {
    fn new(term: &'a CheckedTerm, names: &mut CheckedTermNameCanonicalizer) -> Self {
        let mut types = Vec::new();
        let mut values = Vec::new();
        let mut node = match term.node.as_ref() {
            CheckedTermNode::Raw(expr) => CheckedTermNode::Raw(clone_expr(expr)),
            node => node.clone(),
        };
        match &mut node {
            CheckedTermNode::Local { name } => *name = names.value_name(name),
            CheckedTermNode::TermFn { sig, .. } => {
                *sig = names.signature(sig, &mut types, &mut values)
            }
            CheckedTermNode::IntrinsicProjection {
                left_ty, right_ty, ..
            }
            | CheckedTermNode::IntrinsicInjection {
                left_ty, right_ty, ..
            } => {
                *left_ty = names.ty(left_ty);
                *right_ty = names.ty(right_ty);
            }
            CheckedTermNode::IntrinsicEither {
                left_ty,
                right_ty,
                result_ty,
                ..
            } => {
                *left_ty = names.ty(left_ty);
                *right_ty = names.ty(right_ty);
                *result_ty = names.ty(result_ty);
            }
            CheckedTermNode::IntrinsicAbsurd { result_ty, .. }
            | CheckedTermNode::IntrinsicIfThenElse { result_ty, .. } => {
                *result_ty = names.ty(result_ty)
            }
            _ => {}
        }
        Self {
            term,
            node: EvalRef::new(node),
            next: 0,
            types,
            values,
        }
    }

    fn child(&self) -> Option<&'a CheckedTerm> {
        let mut found = None;
        let mut index = 0;
        children(&self.term.node, |term| {
            if index == self.next {
                found = Some(term);
            }
            index += 1;
        });
        found
    }

    fn returned(&mut self, child: EvalRef<CheckedTerm>, names: &mut CheckedTermNameCanonicalizer) {
        use CheckedTermNode::*;
        let node = EvalRef::make_mut(&mut self.node);
        let slot = match node {
            TermLet { value, body, .. } => {
                if self.next == 0 {
                    value
                } else {
                    body
                }
            }
            TermFn { body, .. } => body,
            TermCall {
                fn_value,
                arg_packet,
                ..
            } => {
                if self.next == 0 {
                    fn_value
                } else {
                    arg_packet
                }
            }
            TermTypeApp { fn_value, .. } => fn_value,
            IntrinsicPair { left, right } => {
                if self.next == 0 {
                    left
                } else {
                    right
                }
            }
            IntrinsicProjection { value, .. } | IntrinsicInjection { value, .. } => value,
            IntrinsicEither {
                value,
                left_body,
                right_body,
                ..
            } => match self.next {
                0 => value,
                1 => left_body,
                _ => right_body,
            },
            IntrinsicAbsurd { bottom_value, .. } => bottom_value,
            IntrinsicIfThenElse {
                condition,
                true_body,
                false_body,
                ..
            } => match self.next {
                0 => condition,
                1 => true_body,
                _ => false_body,
            },
            Raw(_) | TemplateValue { .. } | Local { .. } | ElabError { .. } => {
                unreachable!("leaf terms have no child continuation")
            }
        };
        *slot = child;
        self.next += 1;
        match node {
            TermLet { name, .. } if self.next == 1 => {
                if let Some(binding) = names.push_value_name(name) {
                    *name = binding.canonical.clone();
                    self.values.push(binding);
                }
            }
            TermCall { params, .. } if self.next == 1 => {
                for ty in params {
                    *ty = names.ty(ty);
                }
            }
            TermTypeApp { arg, .. } => *arg = names.ty(arg),
            IntrinsicEither {
                left_name,
                right_name,
                ..
            } => {
                names.pop_value_bindings(std::mem::take(&mut self.values));
                let name = match self.next {
                    1 => Some(left_name),
                    2 => Some(right_name),
                    _ => None,
                };
                if let Some(name) = name
                    && let Some(binding) = names.push_value_name(name)
                {
                    *name = binding.canonical.clone();
                    self.values.push(binding);
                }
            }
            _ => {}
        }
    }

    fn finish(self, names: &mut CheckedTermNameCanonicalizer) -> EvalRef<CheckedTerm> {
        let ty = names.ty(self.term.ty());
        names.pop_value_bindings(self.values);
        names.pop_type_bindings(self.types);
        EvalRef::new(CheckedTerm {
            node: self.node,
            ty: ReflectedType::new_trusted(ty),
        })
    }
}

pub(super) fn canonicalize(
    term: &CheckedTerm,
    names: &mut CheckedTermNameCanonicalizer,
) -> CheckedTerm {
    let mut frames = vec![CanonicalFrame::new(term, names)];
    loop {
        if let Some(child) = frames.last().expect("an active canonical term").child() {
            frames.push(CanonicalFrame::new(child, names));
        } else {
            let result = frames
                .pop()
                .expect("an active canonical term")
                .finish(names);
            if let Some(parent) = frames.last_mut() {
                parent.returned(result, names);
            } else {
                return result
                    .into_inner()
                    .expect("a canonical root is uniquely owned");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> EvalRef<CheckedTerm> {
        EvalRef::new(CheckedTerm::new(
            Expr::Unit {
                occurrence: Default::default(),
                meta: zero_meta(),
            },
            Type::Unit { meta: zero_meta() },
        ))
    }

    #[test]
    fn constructed_and_raw_terms_compare_their_exact_serialized_shape() {
        let constructed = CheckedTerm::from_node(
            CheckedTermNode::TermLet {
                name: "item".into(),
                value: unit(),
                body: unit(),
            },
            Type::Unit { meta: zero_meta() },
        );
        let raw = CheckedTerm::new(constructed.clone_expr(), constructed.clone_type());
        assert!(constructed == raw);
        let mut changed = raw.clone_expr();
        let Expr::Let { name, .. } = &mut changed else {
            unreachable!()
        };
        *name = "different".into();
        assert!(constructed != CheckedTerm::new(changed, constructed.clone_type()));

        let error = CheckedTerm::from_node(
            CheckedTermNode::ElabError {
                kind: CheckedTermErrorKind::Type,
                message: "diagnostic".into(),
            },
            Type::Unit { meta: zero_meta() },
        );
        assert!(
            error == *unit(),
            "non-template equality observes the serialized unit, not diagnostic metadata"
        );
    }

    #[test]
    fn template_graph_equality_preserves_the_deep_template_index() {
        let build = |index| {
            let mut term = EvalRef::new(CheckedTerm::from_node(
                CheckedTermNode::TemplateValue { index },
                Type::Unit { meta: zero_meta() },
            ));
            let value = unit();
            for _ in 0..10_000 {
                term = EvalRef::new(CheckedTerm::from_node(
                    CheckedTermNode::TermLet {
                        name: "item".into(),
                        value: value.clone(),
                        body: term,
                    },
                    Type::Unit { meta: zero_meta() },
                ));
            }
            term
        };
        let left = build(1);
        assert!(left == build(1));
        assert!(left != build(2));
        assert_eq!(left.template_value_arity(), 2);
    }
}
