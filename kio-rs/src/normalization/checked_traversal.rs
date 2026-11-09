//! Iterative consumers of the term graphs constructed by reflection callbacks.

use super::*;

pub(super) fn children<'a>(node: &'a CheckedTermNode, mut visit: impl FnMut(&'a CheckedTerm)) {
    match node {
        CheckedTermNode::Raw(_)
        | CheckedTermNode::TemplateValue { .. }
        | CheckedTermNode::Local { .. }
        | CheckedTermNode::MacroError { .. } => {}
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
        CheckedTermNode::IntrinsicPair { left, right }
        | CheckedTermNode::IntrinsicBoolean { left, right, .. } => {
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
            source_trivia: _,
            occurrence: _,
            diagnostic: _,
            callee,
            args,
            meta,
            ext,
        } => Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
            callee: Box::new(child(callee)),
            args: args
                .iter()
                .map(|arg| match arg {
                    CallArg::Value(value) => CallArg::Value(child(value)),
                    CallArg::Type(ty) => CallArg::Type(type_traversal::clone_type(ty)),
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
            sig: Signature::from_parts(
                sig.params
                    .iter()
                    .map(|param| match param {
                        SignatureParam::Type(param) => SignatureParam::Type(TypeParam {
                            name: param.name.clone(),
                            presentation: param.presentation.clone(),
                            kind: param
                                .kind
                                .as_deref()
                                .map(type_traversal::clone_kind)
                                .map(Box::new),
                            span: param.span,
                        }),
                        SignatureParam::Value(param) => SignatureParam::Value(Param {
                            name: param.name.clone(),
                            ty: param.ty.as_ref().map(type_traversal::clone_type),
                            pattern: param.pattern,
                            meta: param.meta.clone(),
                        }),
                    })
                    .collect(),
                sig.groups.clone(),
            ),
            ret_ty: ret_ty.as_ref().map(type_traversal::clone_type),
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
            ty: ty.as_ref().map(type_traversal::clone_type),
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
        Expr::StrLit {
            value,
            annotation,
            meta,
            ..
        } => Expr::StrLit {
            occurrence: Default::default(),
            value: value.clone(),
            annotation: annotation.as_ref().map(type_traversal::clone_type),
            meta: meta.clone(),
        },
        Expr::IntLit {
            digits,
            annotation,
            meta,
            diagnostic,
            ..
        } => Expr::IntLit {
            occurrence: Default::default(),
            diagnostic: diagnostic.clone(),
            digits: digits.clone(),
            annotation: annotation.as_ref().map(type_traversal::clone_type),
            meta: meta.clone(),
        },
        Expr::FloatLit {
            digits,
            annotation,
            meta,
            ..
        } => Expr::FloatLit {
            occurrence: Default::default(),
            digits: digits.clone(),
            annotation: annotation.as_ref().map(type_traversal::clone_type),
            meta: meta.clone(),
        },
        Expr::BoolLit {
            value,
            annotation,
            meta,
            ..
        } => Expr::BoolLit {
            occurrence: Default::default(),
            value: *value,
            annotation: annotation.as_ref().map(type_traversal::clone_type),
            meta: meta.clone(),
        },
        Expr::Path { .. } | Expr::Unit { .. } => expr.clone(),
        Expr::RecCall { ext, .. }
        | Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserMacro { ext, .. }
        | Expr::Ufcs { ext, .. }
        | Expr::OpChain { ext, .. }
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
                        source_trivia: Default::default(),
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

pub(super) fn expr_equal(left: &EvalAstExpr, right: &EvalAstExpr) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        let mut left_children = Vec::new();
        let mut right_children = Vec::new();
        let left = EvalRef::new(expr_head(left, |child| {
            left_children.push(child);
            Expr::Unit {
                source_trivia: Default::default(),
                occurrence: Default::default(),
                meta: zero_meta(),
            }
        }));
        let right = EvalRef::new(expr_head(right, |child| {
            right_children.push(child);
            Expr::Unit {
                source_trivia: Default::default(),
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
    let left = checked_bindings::align_for_materialization(left);
    let right = checked_bindings::align_for_materialization(right);
    let mut left_scopes = checked_bindings::TypeScopes::new();
    let mut right_scopes = checked_bindings::TypeScopes::new();
    let mut pending = vec![(&left, 0, &right, 0)];
    let mut templates = HashMap::new();
    while let Some((left_term, left_scope, right_term, right_scope)) = pending.pop() {
        if left_term.ty != right_term.ty {
            return false;
        }
        if template_arity(&left_term.node, &mut templates) == 0
            && template_arity(&right_term.node, &mut templates) == 0
        {
            let left_expr = EvalRef::new(materialize(&left_term.node));
            let right_expr = EvalRef::new(materialize(&right_term.node));
            if !expr_equal(&left_expr, &right_expr)
                || checked_bindings::reference_occurrences(
                    left_term,
                    &left_scopes,
                    left_scope,
                    Some((&left_expr, &left.ty.environment)),
                ) != checked_bindings::reference_occurrences(
                    right_term,
                    &right_scopes,
                    right_scope,
                    Some((&right_expr, &right.ty.environment)),
                )
            {
                return false;
            }
        } else {
            if checked_bindings::reference_occurrences(left_term, &left_scopes, left_scope, None)
                != checked_bindings::reference_occurrences(
                    right_term,
                    &right_scopes,
                    right_scope,
                    None,
                )
            {
                return false;
            }
            let mut children = Vec::new();
            if !node_equal(&left_term.node, &right_term.node, &mut children) {
                return false;
            }
            let left_scope = left_scopes.child(left_scope, &left_term.node);
            let right_scope = right_scopes.child(right_scope, &right_term.node);
            pending.extend(
                children
                    .into_iter()
                    .map(|(left, right)| (left, left_scope, right, right_scope)),
            );
        }
    }
    true
}

pub(super) fn node_equal<'a>(
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
        (TermCall { .. }, TermCall { .. }) => true,
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
            IntrinsicBoolean {
                conjunction: left, ..
            },
            IntrinsicBoolean {
                conjunction: right, ..
            },
        ) => left == right,
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
            MacroError {
                kind: lk,
                message: lm,
            },
            MacroError {
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
    fn new(term: &'a CheckedTerm, names: &mut CheckedTermNameCanonicalizer<'_>) -> Self {
        let mut types = Vec::new();
        let mut values = Vec::new();
        let node = match term.node.as_ref() {
            CheckedTermNode::Raw(expr) => CheckedTermNode::Raw(names.raw_expr(expr)),
            CheckedTermNode::Local { name } => CheckedTermNode::Local {
                name: names.value_name(name),
            },
            CheckedTermNode::TemplateValue { index } => {
                CheckedTermNode::TemplateValue { index: *index }
            }
            CheckedTermNode::TermLet { name, value, body } => CheckedTermNode::TermLet {
                name: name.clone(),
                value: value.clone(),
                body: body.clone(),
            },
            CheckedTermNode::TermFn { sig, body } => CheckedTermNode::TermFn {
                sig: names.signature(sig, &mut types, &mut values),
                body: body.clone(),
            },
            CheckedTermNode::TermCall {
                fn_value,
                arg_packet,
                ..
            } => CheckedTermNode::TermCall {
                fn_value: fn_value.clone(),
                arg_packet: arg_packet.clone(),
            },
            CheckedTermNode::TermTypeApp { fn_value, .. } => CheckedTermNode::TermTypeApp {
                fn_value: fn_value.clone(),
                arg: Type::Unit { meta: zero_meta() },
            },
            CheckedTermNode::IntrinsicPair { left, right } => CheckedTermNode::IntrinsicPair {
                left: left.clone(),
                right: right.clone(),
            },
            CheckedTermNode::IntrinsicProjection {
                intrinsic,
                left_ty,
                right_ty,
                value,
            } => CheckedTermNode::IntrinsicProjection {
                intrinsic: intrinsic.clone(),
                left_ty: names.ty(left_ty),
                right_ty: names.ty(right_ty),
                value: value.clone(),
            },
            CheckedTermNode::IntrinsicInjection {
                intrinsic,
                left_ty,
                right_ty,
                value,
            } => CheckedTermNode::IntrinsicInjection {
                intrinsic: intrinsic.clone(),
                left_ty: names.ty(left_ty),
                right_ty: names.ty(right_ty),
                value: value.clone(),
            },
            CheckedTermNode::IntrinsicEither {
                left_ty,
                right_ty,
                result_ty,
                value,
                left_name,
                left_body,
                right_name,
                right_body,
            } => CheckedTermNode::IntrinsicEither {
                left_ty: names.ty(left_ty),
                right_ty: names.ty(right_ty),
                result_ty: names.ty(result_ty),
                value: value.clone(),
                left_name: left_name.clone(),
                left_body: left_body.clone(),
                right_name: right_name.clone(),
                right_body: right_body.clone(),
            },
            CheckedTermNode::IntrinsicAbsurd {
                result_ty,
                bottom_value,
            } => CheckedTermNode::IntrinsicAbsurd {
                result_ty: names.ty(result_ty),
                bottom_value: bottom_value.clone(),
            },
            CheckedTermNode::IntrinsicBoolean {
                conjunction,
                left,
                right,
            } => CheckedTermNode::IntrinsicBoolean {
                conjunction: *conjunction,
                left: left.clone(),
                right: right.clone(),
            },
            CheckedTermNode::IntrinsicIfThenElse {
                result_ty,
                condition,
                true_body,
                false_body,
            } => CheckedTermNode::IntrinsicIfThenElse {
                result_ty: names.ty(result_ty),
                condition: condition.clone(),
                true_body: true_body.clone(),
                false_body: false_body.clone(),
            },
            CheckedTermNode::MacroError { kind, message } => CheckedTermNode::MacroError {
                kind: *kind,
                message: message.clone(),
            },
        };
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

    fn returned(
        &mut self,
        child: EvalRef<CheckedTerm>,
        names: &mut CheckedTermNameCanonicalizer<'_>,
    ) {
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
            IntrinsicPair { left, right } | IntrinsicBoolean { left, right, .. } => {
                if self.next == 0 { left } else { right }
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
            Raw(_) | TemplateValue { .. } | Local { .. } | MacroError { .. } => {
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
            TermTypeApp { arg, .. } => {
                let CheckedTermNode::TermTypeApp { arg: source, .. } = self.term.node.as_ref()
                else {
                    unreachable!("canonical type application retains its source node")
                };
                *arg = names.ty(source);
            }
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

    fn finish(self, names: &mut CheckedTermNameCanonicalizer<'_>) -> EvalRef<CheckedTerm> {
        let ty = names.ty(self.term.ty());
        names.pop_value_bindings(self.values);
        names.pop_type_bindings(self.types);
        EvalRef::new(CheckedTerm {
            node: self.node,
            ty: ReflectedType::from_parts(EvalRef::new(ty), None, self.term.ty.environment.clone()),
        })
    }
}

pub(super) fn canonicalize(
    term: &CheckedTerm,
    names: &mut CheckedTermNameCanonicalizer<'_>,
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
                source_trivia: Default::default(),
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
            CheckedTermNode::MacroError {
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
