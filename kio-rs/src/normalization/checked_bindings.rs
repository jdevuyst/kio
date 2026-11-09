//! Lexical type references in checked term graphs and their typed node fields.

use super::reflection_bindings::ReflectedVariable;
use super::*;

/// Exact, bounded cache guard for captured checked graphs. Semantic grouping
/// stays with the caller; this guard may conservatively decline reuse.
#[derive(Clone, Debug)]
pub(crate) struct CheckedSnapshotKey {
    term: CheckedTerm,
    types: Vec<reflection_bindings::ReflectedSnapshotKey>,
}

impl PartialEq for CheckedSnapshotKey {
    fn eq(&self, other: &Self) -> bool {
        self.exact_match(other).0
    }
}
impl Eq for CheckedSnapshotKey {}

impl CheckedSnapshotKey {
    fn exact_match(&self, other: &Self) -> (bool, usize) {
        if self.types != other.types {
            return (false, 0);
        }
        let mut pending = vec![(&self.term, &other.term)];
        let mut left_to_right = HashMap::new();
        let mut right_to_left = HashMap::new();
        let mut work = 0;
        while let Some((left, right)) = pending.pop() {
            let left_key = std::ptr::from_ref(left);
            let right_key = std::ptr::from_ref(right);
            if let Some(previous) = left_to_right.get(&left_key) {
                if *previous != right_key {
                    return (false, work);
                }
                continue;
            }
            if right_to_left.contains_key(&right_key) {
                return (false, work);
            }
            left_to_right.insert(left_key, right_key);
            right_to_left.insert(right_key, left_key);
            work += 1;
            if !checked_traversal::node_equal(&left.node, &right.node, &mut pending) {
                return (false, work);
            }
        }
        (true, work)
    }

    pub(crate) fn new(root: &CheckedTerm, budget: &mut usize) -> Option<Self> {
        fn signature(sig: &EvalAstSignature, budget: &mut usize) -> Option<()> {
            for param in &sig.params {
                *budget = budget.checked_sub(1)?;
                if let SignatureParam::Type(param) = param {
                    let mut kinds = param.kind.as_deref().into_iter().collect::<Vec<_>>();
                    while let Some(kind) = kinds.pop() {
                        *budget = budget.checked_sub(1)?;
                        if let crate::ast::Kind::Arrow(left, right) = kind {
                            kinds.push(left);
                            kinds.push(right);
                        }
                    }
                }
            }
            Some(())
        }
        let mut work = vec![root];
        let mut seen = HashSet::new();
        let mut types = Vec::new();
        while let Some(term) = work.pop() {
            // A CheckedTerm owns its environment independently of its node.
            if !seen.insert(std::ptr::from_ref(term)) {
                continue;
            }
            *budget = budget.checked_sub(1)?;
            types.push(reflection_bindings::ReflectedSnapshotKey::new(
                &term.ty, budget,
            )?);
            match term.node.as_ref() {
                CheckedTermNode::Raw(expr) => {
                    let mut exprs = vec![expr];
                    while let Some(expr) = exprs.pop() {
                        *budget = budget.checked_sub(1)?;
                        match expr {
                            Expr::FnExpr { sig, body, .. } => {
                                signature(sig, budget)?;
                                exprs.push(body);
                            }
                            Expr::Call { callee, args, .. } => {
                                if args.len() > *budget {
                                    return None;
                                }
                                exprs.push(callee);
                                for arg in args {
                                    if let CallArg::Value(value) = arg {
                                        exprs.push(value);
                                    }
                                }
                            }
                            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                                exprs.push(value);
                                exprs.push(body);
                            }
                            _ => {}
                        }
                        if exprs.len() > *budget {
                            return None;
                        }
                    }
                }
                CheckedTermNode::TermFn { sig, .. } => signature(sig, budget)?,
                _ => {}
            }
            let mut admitted = Some(());
            local_types(&term.node, |ty, _| {
                if admitted.is_none() {
                    return;
                }
                admitted = (|| {
                    // Charge before cloning; the environment may retain owners
                    // unused by this occurrence, and the key retains them too.
                    type_traversal::charge_memo_type(ty, budget)?;
                    let carrier = ReflectedType::from_parts(
                        EvalRef::new(type_traversal::clone_type(ty)),
                        None,
                        term.ty.environment.clone(),
                    );
                    types.push(reflection_bindings::ReflectedSnapshotKey::new(
                        &carrier, budget,
                    )?);
                    Some(())
                })();
            });
            admitted?;
            checked_traversal::children(&term.node, |child| work.push(child));
            if work.len() > *budget {
                return None;
            }
        }
        Some(Self {
            term: root.clone(),
            types,
        })
    }

    pub(crate) fn presentation_matches(&self, other: &Self) -> bool {
        self.types.len() == other.types.len()
            && self
                .types
                .iter()
                .zip(&other.types)
                .all(|(left, right)| left.presentation_matches(right))
    }
}

#[derive(Default)]
pub(super) struct TypeScopes(Vec<(usize, Vec<String>)>);

impl TypeScopes {
    pub(super) fn new() -> Self {
        Self(vec![(0, Vec::new())])
    }
    pub(super) fn child(&mut self, parent: usize, node: &CheckedTermNode) -> usize {
        let CheckedTermNode::TermFn { sig, .. } = node else {
            return parent;
        };
        let names = sig
            .params
            .iter()
            .filter_map(|param| match param {
                SignatureParam::Type(param) => Some(param.name.clone()),
                SignatureParam::Value(_) => None,
            })
            .collect::<Vec<_>>();
        if names.is_empty() {
            return parent;
        }
        let index = self.0.len();
        self.0.push((parent, names));
        index
    }
    fn contains(&self, mut index: usize, name: &str) -> bool {
        loop {
            let (parent, names) = &self.0[index];
            if names.iter().any(|bound| bound == name) {
                return true;
            }
            if index == 0 {
                return false;
            }
            index = *parent;
        }
    }
}

struct TypeMask<'a> {
    scopes: &'a TypeScopes,
    scope: usize,
    local: &'a HashMap<String, usize>,
}
impl TypeMask<'_> {
    fn contains_key(&self, name: &str) -> bool {
        self.local.contains_key(name) || self.scopes.contains(self.scope, name)
    }
}

fn local_binders(node: &CheckedTermNode, mut visit: impl FnMut(&str)) {
    let signature = |sig: &Signature<EvalPhase>, visit: &mut dyn FnMut(&str)| {
        for param in &sig.params {
            if let SignatureParam::Type(param) = param {
                visit(&param.name);
            }
        }
    };
    if let CheckedTermNode::TermFn { sig, .. } = node {
        signature(sig, &mut visit);
    }
    let CheckedTermNode::Raw(root) = node else {
        return;
    };
    let mut work = vec![root];
    while let Some(expr) = work.pop() {
        match expr {
            Expr::FnExpr { sig, body, .. } => {
                signature(sig, &mut visit);
                work.push(body);
            }
            Expr::Call { callee, args, .. } => {
                work.push(callee);
                for arg in args {
                    if let CallArg::Value(value) = arg {
                        work.push(value);
                    }
                }
            }
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                work.push(body);
                work.push(value);
            }
            _ => {}
        }
    }
}

fn signature_types(
    signature: &Signature<EvalPhase>,
    bound: &mut HashMap<String, usize>,
    visit: &mut impl FnMut(&EvalAstType, &HashMap<String, usize>),
) -> Vec<String> {
    let mut entered = Vec::new();
    for param in &signature.params {
        match param {
            SignatureParam::Type(param) => {
                *bound.entry(param.name.clone()).or_default() += 1;
                entered.push(param.name.clone());
            }
            SignatureParam::Value(param) => {
                if let Some(ty) = &param.ty {
                    visit(ty, bound);
                }
            }
        }
    }
    entered
}

fn leave_names(names: Vec<String>, bound: &mut HashMap<String, usize>) {
    for name in names.into_iter().rev() {
        let count = bound.get_mut(&name).expect("entered checked type binder");
        *count -= 1;
        if *count == 0 {
            bound.remove(&name);
        }
    }
}

fn raw_types(root: &EvalAstExpr, visit: &mut impl FnMut(&EvalAstType, &HashMap<String, usize>)) {
    enum Work<'a> {
        Expr(&'a EvalAstExpr),
        Leave(Vec<String>),
    }
    let mut work = vec![Work::Expr(root)];
    let mut bound = HashMap::new();
    while let Some(next) = work.pop() {
        let expr = match next {
            Work::Leave(names) => {
                leave_names(names, &mut bound);
                continue;
            }
            Work::Expr(expr) => expr,
        };
        match expr {
            Expr::Call { callee, args, .. } => {
                for arg in args {
                    if let CallArg::Type(ty) = arg {
                        visit(ty, &bound);
                    }
                }
                for arg in args.iter().rev() {
                    if let CallArg::Value(value) = arg {
                        work.push(Work::Expr(value));
                    }
                }
                work.push(Work::Expr(callee));
            }
            Expr::FnExpr {
                sig, ret_ty, body, ..
            } => {
                let entered = signature_types(sig, &mut bound, visit);
                if let Some(ty) = ret_ty {
                    visit(ty, &bound);
                }
                work.push(Work::Leave(entered));
                work.push(Work::Expr(body));
            }
            Expr::Let {
                ty, value, body, ..
            } => {
                if let Some(ty) = ty {
                    visit(ty, &bound);
                }
                work.push(Work::Expr(body));
                work.push(Work::Expr(value));
            }
            Expr::Seq { value, body, .. } => {
                work.push(Work::Expr(body));
                work.push(Work::Expr(value));
            }
            Expr::StrLit { annotation, .. }
            | Expr::IntLit { annotation, .. }
            | Expr::FloatLit { annotation, .. }
            | Expr::BoolLit { annotation, .. } => {
                if let Some(ty) = annotation {
                    visit(ty, &bound);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn local_types(
    node: &CheckedTermNode,
    mut visit: impl FnMut(&EvalAstType, &HashMap<String, usize>),
) {
    let mut bound = HashMap::new();
    match node {
        CheckedTermNode::Raw(expr) => raw_types(expr, &mut visit),
        CheckedTermNode::TermFn { sig, .. } => {
            signature_types(sig, &mut bound, &mut visit);
        }
        CheckedTermNode::TermTypeApp { arg, .. } => visit(arg, &bound),
        CheckedTermNode::IntrinsicProjection {
            left_ty, right_ty, ..
        }
        | CheckedTermNode::IntrinsicInjection {
            left_ty, right_ty, ..
        } => {
            visit(left_ty, &bound);
            visit(right_ty, &bound);
        }
        CheckedTermNode::IntrinsicEither {
            left_ty,
            right_ty,
            result_ty,
            ..
        } => {
            visit(left_ty, &bound);
            visit(right_ty, &bound);
            visit(result_ty, &bound);
        }
        CheckedTermNode::IntrinsicAbsurd { result_ty, .. }
        | CheckedTermNode::IntrinsicIfThenElse { result_ty, .. } => visit(result_ty, &bound),
        CheckedTermNode::TemplateValue { .. }
        | CheckedTermNode::Local { .. }
        | CheckedTermNode::TermLet { .. }
        | CheckedTermNode::TermCall { .. }
        | CheckedTermNode::IntrinsicPair { .. }
        | CheckedTermNode::MacroError { .. } => {}
    }
}

type FreeReferences = HashSet<(String, reflection_bindings::ReflectedReferenceKey)>;

fn free_references(root: &CheckedTerm) -> HashMap<*const CheckedTerm, FreeReferences> {
    let mut work = vec![(root, false)];
    let mut found = HashMap::<*const CheckedTerm, FreeReferences>::new();
    while let Some((term, finish)) = work.pop() {
        let key = std::ptr::from_ref(term);
        if found.contains_key(&key) {
            continue;
        }
        if !finish {
            work.push((term, true));
            checked_traversal::children(&term.node, |child| work.push((child, false)));
            continue;
        }
        let mut references = FreeReferences::new();
        let mut add = |ty: &EvalAstType, bound: &HashMap<String, usize>| {
            for (name, variable) in reflection_bindings::free_bindings_in(ty, &term.ty.environment)
            {
                if !bound.contains_key(name) {
                    references.insert((
                        name.to_owned(),
                        reflection_bindings::ReflectedReferenceKey(variable.clone()),
                    ));
                }
            }
        };
        add(term.ty(), &HashMap::new());
        local_types(&term.node, &mut add);
        let mut child_binders = HashSet::new();
        if let CheckedTermNode::TermFn { sig, .. } = term.node.as_ref() {
            for param in &sig.params {
                if let SignatureParam::Type(param) = param {
                    child_binders.insert(param.name.as_str());
                }
            }
        }
        checked_traversal::children(&term.node, |child| {
            references.extend(
                found[&std::ptr::from_ref(child)]
                    .iter()
                    .filter(|(name, _)| !child_binders.contains(name.as_str()))
                    .cloned(),
            );
        });
        found.insert(key, references);
    }
    found
}

pub(super) fn has_free_opened_reference(root: &CheckedTerm) -> bool {
    free_references(root)[&std::ptr::from_ref(root)]
        .iter()
        .any(|(_, variable)| {
            matches!(
                variable.0.meaning,
                reflection_bindings::VariableMeaning::Opened(_)
            )
        })
}

#[cfg(test)]
pub(super) fn local_references(
    term: &CheckedTerm,
) -> Vec<reflection_bindings::ReflectedReferenceKey> {
    let mut references = Vec::new();
    local_types(&term.node, |ty, bound| {
        for (name, variable) in reflection_bindings::free_bindings_in(ty, &term.ty.environment) {
            if !bound.contains_key(name) {
                references.push(reflection_bindings::ReflectedReferenceKey(variable.clone()));
            }
        }
    });
    references
}

pub(super) fn reference_occurrences(
    term: &CheckedTerm,
    scopes: &TypeScopes,
    scope: usize,
    materialized: Option<(&EvalAstExpr, &ReflectionEnvironment)>,
) -> Vec<(String, reflection_bindings::ReflectedReferenceKey)> {
    let mut out = Vec::new();
    let mut append =
        |ty: &EvalAstType, environment: &ReflectionEnvironment, local: &HashMap<String, usize>| {
            let mask = TypeMask {
                scopes,
                scope,
                local,
            };
            for (head, variable) in reflection_bindings::free_bindings_in(ty, environment) {
                if !mask.contains_key(head) {
                    out.push((
                        head.to_owned(),
                        reflection_bindings::ReflectedReferenceKey(variable.clone()),
                    ));
                }
            }
        };
    append(term.ty(), &term.ty.environment, &HashMap::new());
    if let Some((expr, environment)) = materialized {
        raw_types(expr, &mut |ty, bound| append(ty, environment, bound));
    } else {
        local_types(&term.node, |ty, bound| {
            append(ty, &term.ty.environment, bound)
        });
    }
    out
}

fn map_signature(
    sig: &Signature<EvalPhase>,
    bound: &mut HashMap<String, usize>,
    transform: &mut impl FnMut(&EvalAstType, &HashMap<String, usize>) -> EvalAstType,
) -> (Signature<EvalPhase>, Vec<String>) {
    let mut entered = Vec::new();
    let params = sig
        .params
        .iter()
        .map(|param| match param {
            SignatureParam::Type(param) => {
                *bound.entry(param.name.clone()).or_default() += 1;
                entered.push(param.name.clone());
                SignatureParam::Type(TypeParam {
                    name: param.name.clone(),
                    presentation: param.presentation.clone(),
                    kind: param
                        .kind
                        .as_deref()
                        .map(type_traversal::clone_kind)
                        .map(Box::new),
                    span: param.span,
                })
            }
            SignatureParam::Value(param) => SignatureParam::Value(Param {
                name: param.name.clone(),
                ty: param.ty.as_ref().map(|ty| transform(ty, bound)),
                pattern: param.pattern,
                meta: param.meta.clone(),
            }),
        })
        .collect();
    (Signature::from_parts(params, sig.groups.clone()), entered)
}

fn map_raw_types(
    source: &EvalAstExpr,
    transform: &mut impl FnMut(&EvalAstType, &HashMap<String, usize>) -> EvalAstType,
) -> EvalAstExpr {
    enum Work<'a> {
        Expr(&'a mut EvalAstExpr),
        Leave(Vec<String>),
    }
    let mut out = ownership::Owned::new(checked_traversal::clone_expr(source));
    let mut work = vec![Work::Expr(&mut out)];
    let mut bound = HashMap::new();
    while let Some(next) = work.pop() {
        let expr = match next {
            Work::Leave(names) => {
                leave_names(names, &mut bound);
                continue;
            }
            Work::Expr(expr) => expr,
        };
        match expr {
            Expr::Call { callee, args, .. } => {
                for arg in args.iter_mut() {
                    if let CallArg::Type(ty) = arg {
                        let new = transform(ty, &bound);
                        drop(ownership::Owned::new(std::mem::replace(ty, new)));
                    }
                }
                for arg in args.iter_mut().rev() {
                    if let CallArg::Value(value) = arg {
                        work.push(Work::Expr(value));
                    }
                }
                work.push(Work::Expr(callee));
            }
            Expr::FnExpr {
                sig, ret_ty, body, ..
            } => {
                let (new, entered) = map_signature(sig, &mut bound, transform);
                drop(ownership::Owned::new(std::mem::replace(sig, new)));
                if let Some(ty) = ret_ty {
                    let new = transform(ty, &bound);
                    drop(ownership::Owned::new(std::mem::replace(ty, new)));
                }
                work.push(Work::Leave(entered));
                work.push(Work::Expr(body));
            }
            Expr::Let {
                ty, value, body, ..
            } => {
                if let Some(ty) = ty {
                    let new = transform(ty, &bound);
                    drop(ownership::Owned::new(std::mem::replace(ty, new)));
                }
                work.push(Work::Expr(body));
                work.push(Work::Expr(value));
            }
            Expr::Seq { value, body, .. } => {
                work.push(Work::Expr(body));
                work.push(Work::Expr(value));
            }
            Expr::StrLit { annotation, .. }
            | Expr::IntLit { annotation, .. }
            | Expr::FloatLit { annotation, .. }
            | Expr::BoolLit { annotation, .. } => {
                if let Some(ty) = annotation {
                    let new = transform(ty, &bound);
                    drop(ownership::Owned::new(std::mem::replace(ty, new)));
                }
            }
            _ => {}
        }
    }
    out.take()
}

fn map_node(
    node: &CheckedTermNode,
    mut transform: impl FnMut(&EvalAstType, &HashMap<String, usize>) -> EvalAstType,
    mut child: impl FnMut(&EvalRef<CheckedTerm>) -> EvalRef<CheckedTerm>,
) -> CheckedTermNode {
    use CheckedTermNode::*;
    let mut bound = HashMap::new();
    match node {
        Raw(expr) => Raw(map_raw_types(expr, &mut transform)),
        TemplateValue { index } => TemplateValue { index: *index },
        Local { name } => Local { name: name.clone() },
        TermLet { name, value, body } => TermLet {
            name: name.clone(),
            value: child(value),
            body: child(body),
        },
        TermFn { sig, body } => TermFn {
            sig: map_signature(sig, &mut bound, &mut transform).0,
            body: child(body),
        },
        TermCall {
            fn_value,
            arg_packet,
        } => TermCall {
            fn_value: child(fn_value),
            arg_packet: child(arg_packet),
        },
        TermTypeApp { fn_value, arg } => TermTypeApp {
            fn_value: child(fn_value),
            arg: transform(arg, &bound),
        },
        IntrinsicPair { left, right } => IntrinsicPair {
            left: child(left),
            right: child(right),
        },
        IntrinsicProjection {
            intrinsic,
            left_ty,
            right_ty,
            value,
        } => IntrinsicProjection {
            intrinsic: intrinsic.clone(),
            left_ty: transform(left_ty, &bound),
            right_ty: transform(right_ty, &bound),
            value: child(value),
        },
        IntrinsicInjection {
            intrinsic,
            left_ty,
            right_ty,
            value,
        } => IntrinsicInjection {
            intrinsic: intrinsic.clone(),
            left_ty: transform(left_ty, &bound),
            right_ty: transform(right_ty, &bound),
            value: child(value),
        },
        IntrinsicEither {
            left_ty,
            right_ty,
            result_ty,
            value,
            left_name,
            left_body,
            right_name,
            right_body,
        } => IntrinsicEither {
            left_ty: transform(left_ty, &bound),
            right_ty: transform(right_ty, &bound),
            result_ty: transform(result_ty, &bound),
            value: child(value),
            left_name: left_name.clone(),
            left_body: child(left_body),
            right_name: right_name.clone(),
            right_body: child(right_body),
        },
        IntrinsicAbsurd {
            result_ty,
            bottom_value,
        } => IntrinsicAbsurd {
            result_ty: transform(result_ty, &bound),
            bottom_value: child(bottom_value),
        },
        IntrinsicIfThenElse {
            result_ty,
            condition,
            true_body,
            false_body,
        } => IntrinsicIfThenElse {
            result_ty: transform(result_ty, &bound),
            condition: child(condition),
            true_body: child(true_body),
            false_body: child(false_body),
        },
        MacroError { kind, message } => MacroError {
            kind: *kind,
            message: message.clone(),
        },
    }
}

// This rewrite requires constructor hygiene: every typed occurrence is reserved
// before introducing a binder, preventing capture of a distinct free reference
// in a shared child. Distinct lexical contexts remain separate for internal
// graphs that do not establish that invariant.
fn map_graph(
    root: &CheckedTerm,
    transform: impl FnMut(
        &EvalAstType,
        &ReflectionEnvironment,
        TypeMask<'_>,
        &mut ReflectionEnvironment,
    ) -> EvalAstType,
) -> CheckedTerm {
    let references = free_references(root);
    map_graph_selective(root, &references, |_, _| false, transform)
}

fn map_graph_selective(
    root: &CheckedTerm,
    references: &HashMap<*const CheckedTerm, FreeReferences>,
    mut retain: impl FnMut(&FreeReferences, &[String]) -> bool,
    mut transform: impl FnMut(
        &EvalAstType,
        &ReflectionEnvironment,
        TypeMask<'_>,
        &mut ReflectionEnvironment,
    ) -> EvalAstType,
) -> CheckedTerm {
    type Key = (*const CheckedTerm, Vec<String>);
    enum Work<'a> {
        Enter(&'a CheckedTerm, usize),
        Finish(&'a CheckedTerm, usize, usize, Key),
    }
    let key = |term: &CheckedTerm, scope, scopes: &TypeScopes| {
        let mut masked = references[&std::ptr::from_ref(term)]
            .iter()
            .filter(|(name, _)| scopes.contains(scope, name))
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        masked.sort_unstable();
        masked.dedup();
        (std::ptr::from_ref(term), masked)
    };
    let mut scopes = TypeScopes::new();
    let mut work = vec![Work::Enter(root, 0)];
    let mut mapped = HashMap::<Key, Option<EvalRef<CheckedTerm>>>::new();
    while let Some(next) = work.pop() {
        let (term, scope, child_scope, term_key) = match next {
            Work::Enter(term, scope) => {
                let term_key = key(term, scope, &scopes);
                if mapped.contains_key(&term_key) {
                    continue;
                }
                if retain(&references[&std::ptr::from_ref(term)], &term_key.1) {
                    mapped.insert(term_key, None);
                    continue;
                }
                let child_scope = scopes.child(scope, &term.node);
                work.push(Work::Finish(term, scope, child_scope, term_key));
                checked_traversal::children(&term.node, |child| {
                    work.push(Work::Enter(child, child_scope))
                });
                continue;
            }
            Work::Finish(term, scope, child_scope, term_key) => {
                (term, scope, child_scope, term_key)
            }
        };
        let mut environment = ReflectionEnvironment::default();
        let ty = transform(
            term.ty(),
            &term.ty.environment,
            TypeMask {
                scopes: &scopes,
                scope,
                local: &HashMap::new(),
            },
            &mut environment,
        );
        let node = map_node(
            &term.node,
            |ty, bound| {
                transform(
                    ty,
                    &term.ty.environment,
                    TypeMask {
                        scopes: &scopes,
                        scope,
                        local: bound,
                    },
                    &mut environment,
                )
            },
            |child| {
                mapped[&key(child, child_scope, &scopes)]
                    .clone()
                    .unwrap_or_else(|| child.clone())
            },
        );
        mapped.insert(
            term_key,
            Some(EvalRef::new(CheckedTerm {
                node: EvalRef::new(node),
                ty: ReflectedType::from_parts(EvalRef::new(ty), None, environment),
            })),
        );
    }
    mapped
        .remove(&(std::ptr::from_ref(root), Vec::new()))
        .expect("mapped checked root")
        .map_or_else(|| root.clone(), EvalRef::unwrap_or_clone)
}

fn reserved_names(root: &CheckedTerm) -> HashSet<String> {
    let mut taken = HashSet::new();
    let mut work = vec![root];
    let mut seen = HashSet::new();
    while let Some(term) = work.pop() {
        if !seen.insert(std::ptr::from_ref(term)) {
            continue;
        }
        reflection_bindings::reserve_type_names(term.ty(), &mut taken);
        local_binders(&term.node, |name| {
            taken.insert(name.to_owned());
        });
        local_types(&term.node, |ty, bound| {
            reflection_bindings::reserve_type_names(ty, &mut taken);
            taken.extend(bound.keys().cloned());
        });
        checked_traversal::children(&term.node, |child| work.push(child));
    }
    for (_, variable) in &free_references(root)[&std::ptr::from_ref(root)] {
        if matches!(
            variable.0.meaning,
            reflection_bindings::VariableMeaning::Caller(_)
                | reflection_bindings::VariableMeaning::Live(_)
        ) {
            taken.insert(variable.0.name.clone());
        }
    }
    taken
}

fn materialization_reserved_names(root: &CheckedTerm) -> HashSet<String> {
    fn reserve(
        ty: &EvalAstType,
        environment: &ReflectionEnvironment,
        local: &HashMap<String, usize>,
        taken: &mut HashSet<String>,
    ) {
        enum Work<'a> {
            Type(&'a EvalAstType),
            Leave(&'a str),
        }
        let mut work = vec![Work::Type(ty)];
        let mut bound = HashMap::<&str, usize>::new();
        while let Some(next) = work.pop() {
            let ty = match next {
                Work::Type(ty) => ty,
                Work::Leave(name) => {
                    let count = bound.get_mut(name).unwrap();
                    *count -= 1;
                    if *count == 0 {
                        bound.remove(name);
                    }
                    continue;
                }
            };
            match ty {
                Type::Path { segments, args, .. } => {
                    if let [head] = segments.as_slice() {
                        if local.contains_key(head.as_str())
                            || bound.contains_key(head.as_str())
                            || environment.get(head.as_str()).is_none()
                        {
                            taken.insert(head.name.clone());
                        }
                    } else {
                        taken.extend(segments.iter().map(|head| head.name.clone()));
                    }
                    work.extend(args.iter().map(Work::Type));
                }
                Type::Forall { param, body, .. } => {
                    taken.insert(param.name.clone());
                    *bound.entry(&param.name).or_default() += 1;
                    work.push(Work::Leave(&param.name));
                    work.push(Work::Type(body));
                }
                Type::Function { param, ret, .. } => {
                    work.push(Work::Type(param));
                    work.push(Work::Type(ret));
                }
                Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                    work.push(Work::Type(left));
                    work.push(Work::Type(right));
                }
                Type::Unit { .. } | Type::Bottom { .. } => {}
                Type::Infer { ext, .. } | Type::Goal { ext, .. } => match *ext {},
            }
        }
    }
    let mut taken = HashSet::new();
    let mut work = vec![root];
    let mut seen = HashSet::new();
    while let Some(term) = work.pop() {
        if !seen.insert(std::ptr::from_ref(term)) {
            continue;
        }
        reserve(term.ty(), &term.ty.environment, &HashMap::new(), &mut taken);
        local_binders(&term.node, |name| {
            taken.insert(name.to_owned());
        });
        local_types(&term.node, |ty, bound| {
            reserve(ty, &term.ty.environment, bound, &mut taken)
        });
        checked_traversal::children(&term.node, |child| work.push(child));
    }
    taken
}

#[cfg(feature = "surface")]
impl CheckedTerm {
    pub(crate) fn reflection_free_bindings(&self) -> Vec<(String, ReflectedVariable)> {
        free_references(self)
            .remove(&std::ptr::from_ref(self))
            .expect("checked free references")
            .into_iter()
            .map(|(name, reference)| (name, reference.0))
            .collect()
    }

    pub(crate) fn reflection_reserved_names(&self) -> HashSet<String> {
        materialization_reserved_names(self)
    }

    pub(crate) fn rebind_reflection(
        &self,
        bindings: &[(ReflectedVariable, String, ReflectedVariable)],
    ) -> Option<Self> {
        let references = self.reflection_free_bindings();
        if references.iter().all(|(head, reference)| {
            bindings
                .iter()
                .find(|(source, _, _)| source.same_reference(reference))
                .is_none_or(|(_, name, target)| head == name && reference.same_reference(target))
        }) {
            return None;
        }
        Some(map_graph(self, |ty, environment, bound, output| {
            let mut replacements = HashMap::new();
            for (head, reference) in reflection_bindings::free_bindings_in(ty, environment) {
                if bound.contains_key(head) {
                    continue;
                }
                let (_, name, target) = bindings
                    .iter()
                    .find(|(source, _, _)| source.same_reference(reference))
                    .expect("every free checked reference belongs to the frozen operand group");
                *output = output.with(name.clone(), target.clone());
                if head != name {
                    replacements.insert(head, ReflectedType::new_trusted(type_path(name)));
                }
            }
            if replacements.is_empty() {
                type_traversal::clone_type(ty)
            } else {
                type_arguments::substitute_template(ty, &replacements)
            }
        }))
    }
}

pub(super) fn align_for_materialization(root: &CheckedTerm) -> CheckedTerm {
    align_for_export(root, &[])
}

pub(super) fn align_for_export(
    root: &CheckedTerm,
    preferred: &[(String, ReflectedVariable)],
) -> CheckedTerm {
    use reflection_bindings::ReflectedReferenceKey;
    let references = free_references(root);
    let free = &references[&std::ptr::from_ref(root)];
    if free.is_empty() {
        return root.clone();
    }
    let mut taken = materialization_reserved_names(root);
    let mut names = HashMap::<ReflectedReferenceKey, String>::new();
    let mut fixed = HashMap::<String, ReflectedVariable>::new();
    for (name, variable) in preferred.iter().cloned().chain(
        free.iter()
            .filter(|(_, variable)| {
                matches!(
                    variable.0.meaning,
                    reflection_bindings::VariableMeaning::Caller(_)
                )
            })
            .map(|(_, variable)| (variable.0.name.clone(), variable.0.clone())),
    ) {
        if !free
            .iter()
            .any(|(_, reference)| reference.0.same_reference(&variable))
        {
            continue;
        }
        if let Some(prior) = fixed.insert(name.clone(), variable.clone()) {
            assert!(
                prior.same_reference(&variable),
                "one export spelling denotes one retained binder"
            );
        }
        if let Some(prior) = names.insert(ReflectedReferenceKey(variable), name.clone()) {
            assert_eq!(prior, name, "one retained binder has one export spelling");
        }
        taken.insert(name);
    }
    let mut alias_owners = HashMap::<String, Option<ReflectedReferenceKey>>::new();
    let mut candidates = HashMap::<ReflectedReferenceKey, Vec<String>>::new();
    for (head, reference) in free {
        if names.get(reference).is_some_and(|name| name != head) {
            continue;
        }
        let owner = alias_owners
            .entry(head.clone())
            .or_insert_with(|| Some(reference.clone()));
        if owner.as_ref() != Some(reference) {
            *owner = None;
        }
        candidates
            .entry(reference.clone())
            .or_default()
            .push(head.clone());
    }
    for (reference, aliases) in &mut candidates {
        if names.contains_key(reference) {
            continue;
        }
        aliases.sort_unstable();
        if let Some(alias) = aliases.iter().find(|alias| {
            !taken.contains(*alias) && alias_owners[*alias].as_ref() == Some(reference)
        }) {
            taken.insert(alias.clone());
            names.insert(reference.clone(), alias.clone());
        }
    }
    if free
        .iter()
        .any(|(_, reference)| !names.contains_key(reference))
    {
        // Choose remaining names in graph order, never hash-table iteration order.
        let mut work = vec![root];
        let mut seen = HashSet::new();
        while let Some(term) = work.pop() {
            if !seen.insert(std::ptr::from_ref(term)) {
                continue;
            }
            let mut assign = |ty: &EvalAstType, bound: &HashMap<String, usize>| {
                for (head, variable) in
                    reflection_bindings::free_bindings_in(ty, &term.ty.environment)
                {
                    let reference = ReflectedReferenceKey(variable.clone());
                    if bound.contains_key(head)
                        || !free.contains(&(head.to_owned(), reference.clone()))
                    {
                        continue;
                    }
                    names.entry(reference).or_insert_with(|| {
                        let name = crate::pass::typecheck_core::fresh_type_var("T", &taken);
                        taken.insert(name.clone());
                        name
                    });
                }
            };
            assign(term.ty(), &HashMap::new());
            local_types(&term.node, &mut assign);
            let start = work.len();
            checked_traversal::children(&term.node, |child| work.push(child));
            work[start..].reverse();
        }
    }
    let mut aligned = map_graph_selective(
        root,
        &references,
        |references, masked| {
            references.iter().all(|(head, reference)| {
                masked.binary_search(head).is_ok() || names.get(reference) == Some(head)
            })
        },
        |ty, environment, bound, output| {
            let mut replacements = HashMap::new();
            for (head, variable) in reflection_bindings::free_bindings_in(ty, environment) {
                if bound.contains_key(head) {
                    continue;
                }
                let name = &names[&ReflectedReferenceKey(variable.clone())];
                assert!(
                    !bound.contains_key(name),
                    "retained export names cannot capture constructed binders"
                );
                *output = output.with(name.clone(), variable.clone());
                if head != name {
                    replacements.insert(head, ReflectedType::new_trusted(type_path(name)));
                }
            }
            if replacements.is_empty() {
                type_traversal::clone_type(ty)
            } else {
                type_arguments::substitute_template(ty, &replacements)
            }
        },
    );
    for (reference, name) in names {
        if !aligned
            .ty
            .environment
            .get(&name)
            .is_some_and(|old| old.same_reference(&reference.0))
        {
            aligned.ty.environment = aligned.ty.environment.with(name, reference.0);
        }
    }
    aligned
}

#[cfg(feature = "surface")]
pub(super) fn replace_templates(root: &CheckedTerm, replacements: &[CheckedTerm]) -> CheckedTerm {
    let mut work = vec![(root, false)];
    let mut ready = HashMap::<*const CheckedTerm, EvalRef<CheckedTerm>>::new();
    while let Some((term, finish)) = work.pop() {
        let key = std::ptr::from_ref(term);
        if ready.contains_key(&key) {
            continue;
        }
        if let CheckedTermNode::TemplateValue { index } = term.node.as_ref() {
            let value = replacements.get(*index).unwrap_or_else(|| {
                panic!("checked-term template refers to missing ABI slot {index}")
            });
            ready.insert(key, EvalRef::new(value.clone()));
        } else if !finish {
            work.push((term, true));
            checked_traversal::children(&term.node, |child| work.push((child, false)));
        } else {
            let node = map_node(
                &term.node,
                |ty, _| type_traversal::clone_type(ty),
                |child| ready[&std::ptr::from_ref(child.as_ref())].clone(),
            );
            ready.insert(
                key,
                EvalRef::new(CheckedTerm {
                    node: EvalRef::new(node),
                    ty: term.ty.clone(),
                }),
            );
        }
    }
    ready
        .remove(&std::ptr::from_ref(root))
        .unwrap()
        .unwrap_or_clone()
}

pub(super) fn materialized_parts(root: &CheckedTerm) -> (EvalAstExpr, EvalAstType) {
    let aligned = align_for_materialization(root);
    (
        checked_traversal::materialize(&aligned.node),
        type_traversal::clone_type(aligned.ty()),
    )
}

fn comparison_graph(root: &CheckedTerm) -> CheckedTerm {
    let aligned = align_for_materialization(root);
    let mut arities = HashMap::new();
    checked_traversal::template_arity(&aligned.node, &mut arities);
    let mut work = vec![(&aligned, false)];
    let mut out = HashMap::<*const CheckedTerm, EvalRef<CheckedTerm>>::new();
    while let Some((term, finish)) = work.pop() {
        let key = std::ptr::from_ref(term);
        if out.contains_key(&key) {
            continue;
        }
        if arities[&std::ptr::from_ref(term.node.as_ref())] == 0 {
            let mut ty = term.ty.clone();
            ty.environment = aligned.ty.environment.clone();
            out.insert(
                key,
                EvalRef::new(CheckedTerm {
                    ty,
                    node: EvalRef::new(CheckedTermNode::Raw(checked_traversal::materialize(
                        &term.node,
                    ))),
                }),
            );
        } else if !finish {
            work.push((term, true));
            checked_traversal::children(&term.node, |child| work.push((child, false)));
        } else {
            let node = map_node(
                &term.node,
                |ty, _| type_traversal::clone_type(ty),
                |child| out[&std::ptr::from_ref(child.as_ref())].clone(),
            );
            out.insert(
                key,
                EvalRef::new(CheckedTerm {
                    node: EvalRef::new(node),
                    ty: term.ty.clone(),
                }),
            );
        }
    }
    out.remove(&std::ptr::from_ref(&aligned))
        .unwrap()
        .unwrap_or_clone()
}

// Bound type spellings are compared by lexical correspondence; free names are
// reserved jointly so canonicalization cannot turn one into a bound occurrence.
fn comparison_free_heads(root: &CheckedTerm) -> HashSet<String> {
    let mut work = vec![(root, false)];
    let mut found = HashMap::<*const CheckedTerm, HashSet<String>>::new();
    while let Some((term, finish)) = work.pop() {
        let key = std::ptr::from_ref(term);
        if found.contains_key(&key) {
            continue;
        }
        if !finish {
            work.push((term, true));
            checked_traversal::children(&term.node, |child| work.push((child, false)));
            continue;
        }
        let mut names = HashSet::new();
        let mut add = |ty: &EvalAstType, bound: &HashMap<String, usize>| {
            names.extend(
                eval_refl_type_free_bare_heads(ty)
                    .into_iter()
                    .filter(|name| !bound.contains_key(name)),
            );
        };
        add(term.ty(), &HashMap::new());
        local_types(&term.node, &mut add);
        let mut bound = HashSet::new();
        if let CheckedTermNode::TermFn { sig, .. } = term.node.as_ref() {
            for param in &sig.params {
                if let SignatureParam::Type(param) = param {
                    bound.insert(param.name.as_str());
                }
            }
        }
        checked_traversal::children(&term.node, |child| {
            names.extend(
                found[&std::ptr::from_ref(child)]
                    .iter()
                    .filter(|name| !bound.contains(name.as_str()))
                    .cloned(),
            );
        });
        found.insert(key, names);
    }
    found.remove(&std::ptr::from_ref(root)).unwrap()
}

pub(super) fn semantic_comparison(
    left: &CheckedTerm,
    right: &CheckedTerm,
) -> Result<reflection_bindings::TypeComparison, reflection_bindings::IncompleteOwner> {
    use reflection_bindings::TypeComparison;
    for root in [left, right] {
        for (_, variable) in &free_references(root)[&std::ptr::from_ref(root)] {
            reflection_bindings::require_determined(&variable.0.as_type())?;
        }
    }
    let left = comparison_graph(left);
    let right = comparison_graph(right);
    let mut free_names = comparison_free_heads(&left);
    free_names.extend(comparison_free_heads(&right));
    let left = CheckedTermNameCanonicalizer::for_type_comparison(free_names.clone()).term(&left);
    let right = CheckedTermNameCanonicalizer::for_type_comparison(free_names).term(&right);
    let mut left_scopes = TypeScopes::new();
    let mut right_scopes = TypeScopes::new();
    let mut pending = vec![(&left, 0, &right, 0)];
    let mut result = TypeComparison::Equal;
    let placeholder = EvalRef::new(CheckedTerm::from_node(
        CheckedTermNode::Local {
            name: String::new(),
        },
        Type::Unit { meta: zero_meta() },
    ));
    let carriers = |term: &CheckedTerm, scopes: &TypeScopes, scope| {
        let mut types = Vec::new();
        let mut append = |ty: &EvalAstType, local: &HashMap<String, usize>| {
            let mask = TypeMask {
                scopes,
                scope,
                local,
            };
            let mut environment = ReflectionEnvironment::default();
            for (head, variable) in reflection_bindings::free_bindings_in(ty, &term.ty.environment)
            {
                if !mask.contains_key(head) {
                    environment = environment.with(head.to_owned(), variable.clone());
                }
            }
            types.push(ReflectedType::from_parts(
                EvalRef::new(type_traversal::clone_type(ty)),
                None,
                environment,
            ));
        };
        append(term.ty(), &HashMap::new());
        local_types(&term.node, |ty, local| append(ty, local));
        types
    };
    while let Some((left, left_scope, right, right_scope)) = pending.pop() {
        let left_types = carriers(left, &left_scopes, left_scope);
        let right_types = carriers(right, &right_scopes, right_scope);
        for ty in left_types.iter().chain(&right_types) {
            reflection_bindings::require_determined(ty)?;
        }
        let left_shape = map_node(
            &left.node,
            |_, _| Type::Unit { meta: zero_meta() },
            |_| placeholder.clone(),
        );
        let right_shape = map_node(
            &right.node,
            |_, _| Type::Unit { meta: zero_meta() },
            |_| placeholder.clone(),
        );
        if !checked_traversal::node_equal(&left_shape, &right_shape, &mut Vec::new())
            || left_types.len() != right_types.len()
        {
            result = TypeComparison::Different;
        } else {
            for (left, right) in left_types.iter().zip(&right_types) {
                match reflection_bindings::semantic_type_comparison(left, right)? {
                    TypeComparison::Different => result = TypeComparison::Different,
                    TypeComparison::Undetermined if result != TypeComparison::Different => {
                        result = TypeComparison::Undetermined
                    }
                    _ => {}
                }
            }
        }
        let mut left_children = Vec::new();
        let mut right_children = Vec::new();
        checked_traversal::children(&left.node, |child| left_children.push(child));
        checked_traversal::children(&right.node, |child| right_children.push(child));
        if left_children.len() != right_children.len() {
            result = TypeComparison::Different;
        }
        let left_scope = left_scopes.child(left_scope, &left.node);
        let right_scope = right_scopes.child(right_scope, &right.node);
        pending.extend(
            left_children
                .into_iter()
                .zip(right_children)
                .map(|(left, right)| (left, left_scope, right, right_scope)),
        );
    }
    Ok(result)
}

pub(super) fn close_forall(
    root: &CheckedTerm,
    variable: &ReflectedVariable,
) -> Result<CheckedTerm, reflection_bindings::FreeOpened> {
    assert!(
        matches!(
            variable.meaning,
            reflection_bindings::VariableMeaning::Live(_)
        ),
        "generic constructor closes its own live binder"
    );
    if has_free_opened_reference(root) {
        return Err(reflection_bindings::FreeOpened);
    }
    let taken = reserved_names(root);
    let name = crate::pass::typecheck_core::fresh_type_var("T", &taken);
    let body = map_graph(root, |ty, environment, bound, output| {
        let mut replacements = HashMap::new();
        for (head, found) in reflection_bindings::free_bindings_in(ty, environment) {
            if bound.contains_key(head) {
                continue;
            }
            if found.same_reference(variable) {
                replacements.insert(head, ReflectedType::new_trusted(type_path(&name)));
            } else {
                *output = output.with(head.to_owned(), found.clone());
            }
        }
        type_arguments::substitute_template(ty, &replacements)
    });
    let param = TypeParam {
        name,
        presentation: Default::default(),
        kind: Some(Box::new(crate::ast::Kind::arrow_chain(variable.arity))),
        span: zero_span(),
    };
    let ty = ReflectedType::from_parts(
        EvalRef::new(Type::Forall {
            param: param.clone(),
            body: Box::new(type_traversal::clone_type(body.ty())),
            meta: zero_meta(),
        }),
        None,
        body.ty.environment.clone(),
    );
    Ok(CheckedTerm {
        node: EvalRef::new(CheckedTermNode::TermFn {
            sig: Signature::from_parts(
                vec![SignatureParam::Type(param)],
                vec![crate::ast::SignatureGroupKind::Type { len: 1 }],
            ),
            body: EvalRef::new(body),
        }),
        ty,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> EvalAstType {
        Type::Unit { meta: zero_meta() }
    }

    fn opened(name: &str) -> ReflectedVariable {
        ReflectedType::new_trusted(Type::Forall {
            param: TypeParam {
                name: name.into(),
                presentation: Default::default(),
                kind: None,
                span: zero_span(),
            },
            body: Box::new(type_path(name)),
            meta: zero_meta(),
        })
        .open_forall()
        .unwrap()
        .0
    }

    #[test]
    fn hidden_type_application_reference_is_part_of_the_fixed_result() {
        let parameter = opened("A");
        let function = EvalRef::new(CheckedTerm::from_node(
            CheckedTermNode::Local { name: "f".into() },
            unit(),
        ));
        let mut term = CheckedTerm::from_node(
            CheckedTermNode::TermTypeApp {
                fn_value: function,
                arg: type_path("A"),
            },
            unit(),
        );
        term.ty.environment = term.ty.environment.with("A".into(), parameter);
        assert!(!reflection_bindings::has_free_opened_reference(&term.ty));
        assert!(has_free_opened_reference(&term));
        assert_eq!(local_references(&term).len(), 1);
    }

    #[test]
    fn raw_type_binders_mask_only_their_lexical_body() {
        let parameter = opened("A");
        let sig = Signature::from_parts(
            vec![SignatureParam::Type(TypeParam {
                name: "A".into(),
                presentation: Default::default(),
                kind: None,
                span: zero_span(),
            })],
            vec![crate::ast::SignatureGroupKind::Type { len: 1 }],
        );
        let raw = Expr::FnExpr {
            occurrence: Default::default(),
            sig,
            ret_ty: Some(unit()),
            body: Box::new(intrinsic_expr(
                "hidden",
                vec![CallArg::Type(type_path("A"))],
            )),
            meta: zero_meta(),
            caps: (),
        };
        let mut term = CheckedTerm::new(raw, unit());
        term.ty.environment = term.ty.environment.with("A".into(), parameter);
        assert!(!has_free_opened_reference(&term));
        assert!(local_references(&term).is_empty());
        let CheckedTermNode::Raw(expr) = term.node.as_ref() else {
            unreachable!()
        };
        let raw = Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(checked_traversal::clone_expr(expr)),
            body: Box::new(intrinsic_expr(
                "hidden",
                vec![CallArg::Type(type_path("A"))],
            )),
            meta: zero_meta(),
        };
        term.node = EvalRef::new(CheckedTermNode::Raw(raw));
        assert!(has_free_opened_reference(&term));
        assert_eq!(local_references(&term).len(), 1);
    }

    #[test]
    fn unused_node_environment_does_not_reject_the_fixed_result() {
        let mut term = CheckedTerm::from_node(
            CheckedTermNode::Local {
                name: "unit".into(),
            },
            unit(),
        );
        term.ty.environment = term.ty.environment.with("A".into(), opened("A"));
        assert!(!has_free_opened_reference(&term));
        assert!(local_references(&term).is_empty());
    }
    #[test]
    fn closing_rewrites_hidden_live_references_with_the_actual_bound_parameter() {
        let parameter = ReflectedVariable::live("A".into(), 0);
        let function = EvalRef::new(CheckedTerm::from_node(
            CheckedTermNode::Local { name: "f".into() },
            unit(),
        ));
        let mut term = CheckedTerm::from_node(
            CheckedTermNode::TermTypeApp {
                fn_value: function,
                arg: type_path("A"),
            },
            unit(),
        );
        term.ty.environment = term.ty.environment.with("A".into(), parameter.clone());
        let closed = close_forall(&term, &parameter).expect("a live parameter can be closed");
        let Type::Forall { param, .. } = closed.ty() else {
            panic!("closed forall")
        };
        let CheckedTermNode::TermFn { sig, body } = closed.node.as_ref() else {
            panic!("closed function")
        };
        let [SignatureParam::Type(written)] = sig.params.as_slice() else {
            panic!("one type binder")
        };
        assert_eq!(written.name, param.name);
        let CheckedTermNode::TermTypeApp { arg, .. } = body.node.as_ref() else {
            panic!("hidden type application")
        };
        assert_eq!(arg, &type_path(&param.name));
        assert!(local_references(body).is_empty());
    }

    #[test]
    fn shared_raw_node_is_scanned_under_each_retained_environment() {
        let function = EvalRef::new(CheckedTerm::from_node(
            CheckedTermNode::Local { name: "f".into() },
            unit(),
        ));
        let node = EvalRef::new(CheckedTermNode::TermTypeApp {
            fn_value: function,
            arg: type_path("A"),
        });
        let mut left = CheckedTerm {
            node: node.clone(),
            ty: ReflectedType::new_trusted(unit()),
        };
        left.ty.environment = left
            .ty
            .environment
            .with("A".into(), ReflectedVariable::live("A".into(), 0));
        let mut right = CheckedTerm {
            node,
            ty: ReflectedType::new_trusted(unit()),
        };
        right.ty.environment = right.ty.environment.with("A".into(), opened("A"));
        let root = CheckedTerm::from_node(
            CheckedTermNode::IntrinsicPair {
                left: EvalRef::new(left),
                right: EvalRef::new(right),
            },
            unit(),
        );
        assert!(has_free_opened_reference(&root));
        assert!(matches!(
            close_forall(&root, &ReflectedVariable::live("T".into(), 0)),
            Err(reflection_bindings::FreeOpened)
        ));
    }
    fn type_function(name: &str, body: EvalRef<CheckedTerm>) -> CheckedTerm {
        let param = TypeParam {
            name: name.into(),
            presentation: Default::default(),
            kind: None,
            span: zero_span(),
        };
        let ty = ReflectedType::from_parts(
            EvalRef::new(Type::Forall {
                param: param.clone(),
                body: Box::new(type_traversal::clone_type(body.ty())),
                meta: zero_meta(),
            }),
            None,
            body.ty.environment.clone(),
        );
        CheckedTerm {
            node: EvalRef::new(CheckedTermNode::TermFn {
                sig: Signature::from_parts(
                    vec![SignatureParam::Type(param)],
                    vec![crate::ast::SignatureGroupKind::Type { len: 1 }],
                ),
                body,
            }),
            ty,
        }
    }

    #[test]
    fn structured_nested_closing_masks_unused_same_spelled_owner_metadata() {
        let live = ReflectedVariable::live("A".into(), 0);
        let mut body = CheckedTerm::from_node(
            CheckedTermNode::Local {
                name: "value".into(),
            },
            type_path("A"),
        );
        body.ty.environment = body
            .ty
            .environment
            .with("A".into(), live.clone())
            .with("T".into(), opened("T"));
        let inner = close_forall(&body, &live).expect("first close");
        assert!(
            !has_free_opened_reference(&inner),
            "new bound T must mask unused prior T metadata in the child"
        );
        assert!(close_forall(&inner, &ReflectedVariable::live("Outer".into(), 0)).is_ok());
    }

    #[test]
    fn shared_child_is_scanned_inside_and_outside_a_structured_type_scope() {
        let mut body = CheckedTerm::from_node(
            CheckedTermNode::Local {
                name: "value".into(),
            },
            type_path("A"),
        );
        body.ty.environment = body.ty.environment.with("A".into(), opened("A"));
        let body = EvalRef::new(body);
        let inner = type_function("A", body.clone());
        assert!(!has_free_opened_reference(&inner));
        let root = CheckedTerm::from_node(
            CheckedTermNode::IntrinsicPair {
                left: body,
                right: EvalRef::new(inner),
            },
            unit(),
        );
        assert!(
            has_free_opened_reference(&root),
            "the unbound occurrence must not reuse a bound visit"
        );
    }
    #[test]
    fn free_reference_scan_shares_repeated_subgraphs_across_lexical_paths() {
        let mut leaf = CheckedTerm::from_node(
            CheckedTermNode::Local {
                name: "unit".into(),
            },
            type_path("A"),
        );
        leaf.ty.environment = leaf.ty.environment.with("A".into(), opened("A"));
        let mut root = EvalRef::new(leaf);
        let depth = 40;
        for index in 0..depth {
            let left = EvalRef::new(type_function(&format!("L{index}"), root.clone()));
            let right = EvalRef::new(type_function(&format!("R{index}"), root));
            root = EvalRef::new(CheckedTerm::from_node(
                CheckedTermNode::IntrinsicPair { left, right },
                unit(),
            ));
        }
        let summaries = free_references(&root);
        assert_eq!(summaries.len(), 1 + 3 * depth);
        let free = &summaries[&std::ptr::from_ref(root.as_ref())];
        assert_eq!(free.len(), 1);
        assert_eq!(free.iter().next().unwrap().0, "A");
        let aligned = align_for_materialization(&root);
        assert_eq!(free_references(&aligned).len(), 1 + 3 * depth);
    }
    fn hidden_argument(variable: ReflectedVariable) -> CheckedTerm {
        let function = EvalRef::new(CheckedTerm::from_node(
            CheckedTermNode::Local { name: "f".into() },
            unit(),
        ));
        let mut term = CheckedTerm::from_node(
            CheckedTermNode::TermTypeApp {
                fn_value: function,
                arg: type_path("A"),
            },
            unit(),
        );
        term.ty.environment = term.ty.environment.with("A".into(), variable);
        term
    }

    #[test]
    fn materialization_keeps_same_spelled_hidden_references_distinct() {
        let first = opened("A");
        let second = ReflectedType::new_trusted(Type::Forall {
            param: TypeParam {
                name: "A".into(),
                presentation: Default::default(),
                kind: None,
                span: zero_span(),
            },
            body: Box::new(Type::Function {
                param: Box::new(type_path("A")),
                ret: Box::new(type_path("A")),
                caps: Default::default(),
                abi_arity: 1,
                meta: zero_meta(),
            }),
            meta: zero_meta(),
        })
        .open_forall()
        .unwrap()
        .0;
        let pair = |left, right| {
            CheckedTerm::from_node(
                CheckedTermNode::IntrinsicPair {
                    left: EvalRef::new(hidden_argument(left)),
                    right: EvalRef::new(hidden_argument(right)),
                },
                unit(),
            )
        };
        let term = pair(first.clone(), second.clone());
        let aligned = align_for_materialization(&term);
        let raw = checked_traversal::materialize(&aligned.node);
        let mut arguments = Vec::new();
        raw_types(&raw, &mut |ty, bound| {
            for (name, variable) in
                reflection_bindings::free_bindings_in(ty, &aligned.ty.environment)
            {
                if !bound.contains_key(name) {
                    arguments.push((name.to_owned(), variable.clone()));
                }
            }
        });
        assert_eq!(arguments.len(), 2);
        assert_ne!(arguments[0].0, arguments[1].0);
        assert!(arguments[0].1.same_reference(&first));
        assert!(arguments[1].1.same_reference(&second));
        assert!(checked_traversal::equal(
            &term,
            &pair(first.clone(), second.clone())
        ));
        assert!(!checked_traversal::equal(&term, &pair(second, first)));
        let twice = align_for_materialization(&aligned);
        assert!(checked_traversal::expr_equal(
            &raw,
            &checked_traversal::materialize(&twice.node)
        ));
    }

    #[test]
    fn repeated_materialization_reuses_exact_nodes_and_types() {
        let stable = EvalRef::new(hidden_argument(ReflectedVariable::live("A".into(), 0)));
        let changing = EvalRef::new(hidden_argument(ReflectedVariable::live("A".into(), 0)));
        let root = CheckedTerm::from_node(
            CheckedTermNode::IntrinsicPair {
                left: stable.clone(),
                right: changing.clone(),
            },
            unit(),
        );
        let first = align_for_materialization(&root);
        let second = align_for_materialization(&first);
        assert!(EvalRef::ptr_eq(&first.node, &second.node));
        assert!(EvalRef::ptr_eq(&first.ty.ty, &second.ty.ty));
    }

    #[test]
    fn materialization_reuses_unchanged_child_handles() {
        let stable = EvalRef::new(hidden_argument(ReflectedVariable::live("A".into(), 0)));
        let distinct = hidden_argument(ReflectedVariable::live("B".into(), 0));
        let stable = EvalRef::new(align_for_materialization(&stable));
        let root = CheckedTerm::from_node(
            CheckedTermNode::IntrinsicPair {
                left: stable.clone(),
                right: EvalRef::new(distinct),
            },
            unit(),
        );
        let preferred = [(
            "B".into(),
            match root.node.as_ref() {
                CheckedTermNode::IntrinsicPair { right, .. } => {
                    right.ty.environment.get("A").unwrap().clone()
                }
                _ => unreachable!(),
            },
        )];
        let aligned = align_for_export(&root, &preferred);
        let CheckedTermNode::IntrinsicPair { left, right } = aligned.node.as_ref() else {
            unreachable!()
        };
        assert!(EvalRef::ptr_eq(left, &stable));
        let CheckedTermNode::TermTypeApp { arg, .. } = right.node.as_ref() else {
            unreachable!()
        };
        assert_eq!(arg, &type_path("B"));
    }

    #[test]
    fn export_preserves_exact_residual_live_name_without_merging_another_live() {
        let residual = ReflectedVariable::live("T".into(), 0);
        let other = ReflectedVariable::live("T".into(), 0);
        let root = CheckedTerm::from_node(
            CheckedTermNode::IntrinsicPair {
                left: EvalRef::new(hidden_argument(residual.clone())),
                right: EvalRef::new(hidden_argument(other.clone())),
            },
            unit(),
        );
        let aligned = align_for_export(&root, &[("T".into(), residual.clone())]);
        let free = free_references(&aligned);
        let references = &free[&std::ptr::from_ref(&aligned)];
        assert!(
            references
                .iter()
                .any(|(head, reference)| head == "T" && reference.0.same_reference(&residual))
        );
        assert!(
            references
                .iter()
                .any(|(head, reference)| head != "T" && reference.0.same_reference(&other))
        );
        let closed = close_forall(&hidden_argument(residual.clone()), &other).unwrap();
        let Type::Forall { param, .. } = closed.ty() else {
            unreachable!()
        };
        assert_ne!(param.name, "T");
        let exported = align_for_export(&closed, &[("T".into(), residual.clone())]);
        let twice = align_for_export(&exported, &[("T".into(), residual)]);
        assert!(EvalRef::ptr_eq(&exported.node, &twice.node));
    }

    #[test]
    fn template_bookkeeping_includes_hidden_reference_identity() {
        let first = opened("A");
        let second = ReflectedVariable::live("A".into(), 0);
        let template = |variable| {
            let mut term = hidden_argument(variable);
            let CheckedTermNode::TermTypeApp { arg, .. } = term.node.as_ref() else {
                unreachable!()
            };
            term.node = EvalRef::new(CheckedTermNode::TermTypeApp {
                fn_value: EvalRef::new(CheckedTerm::from_node(
                    CheckedTermNode::TemplateValue { index: 0 },
                    unit(),
                )),
                arg: type_traversal::clone_type(arg),
            });
            term
        };
        assert!(checked_traversal::equal(
            &template(first.clone()),
            &template(first.clone())
        ));
        assert!(!checked_traversal::equal(
            &template(first),
            &template(second)
        ));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn materialization_preserves_authenticated_caller_spelling() {
        let caller = ReflectedVariable::caller(
            "Caller".into(),
            0,
            crate::pass::typecheck_core::TypeBinderId::for_test(601),
        );
        let aligned = align_for_materialization(&hidden_argument(caller.clone()));
        let CheckedTermNode::TermTypeApp { arg, .. } = aligned.node.as_ref() else {
            unreachable!()
        };
        assert_eq!(arg, &type_path("Caller"));
        assert!(
            aligned
                .ty
                .environment
                .get("Caller")
                .unwrap()
                .same_reference(&caller)
        );
    }
    #[test]
    fn semantic_checked_comparison_ignores_reconstructed_owner_sharing() {
        let owner = opened("A");
        let pair = |left, right| {
            CheckedTerm::from_node(
                CheckedTermNode::IntrinsicPair {
                    left: EvalRef::new(hidden_argument(left)),
                    right: EvalRef::new(hidden_argument(right)),
                },
                unit(),
            )
        };
        let shared = pair(owner.clone(), owner);
        let rebuilt = pair(opened("A"), opened("A"));
        assert_eq!(
            semantic_comparison(&shared, &rebuilt).unwrap(),
            reflection_bindings::TypeComparison::Equal
        );
    }

    #[test]
    fn semantic_checked_comparison_checks_hidden_unknowns_and_preserves_neutrality() {
        let unknown = hidden_argument(ReflectedVariable {
            name: "A".into(),
            arity: 0,
            meaning: reflection_bindings::VariableMeaning::Opened(EvalRef::new(
                reflection_bindings::OpenedScheme::Incomplete,
            )),
        });
        let closed = CheckedTerm::from_node(
            CheckedTermNode::Local {
                name: "other".into(),
            },
            unit(),
        );
        assert!(semantic_comparison(&unknown, &closed).is_err());
        let neutral = hidden_argument(ReflectedVariable::neutral("A".into(), 0));
        let mut unit_arg = hidden_argument(opened("A"));
        let CheckedTermNode::TermTypeApp { fn_value, .. } = unit_arg.node.as_ref() else {
            unreachable!()
        };
        unit_arg.node = EvalRef::new(CheckedTermNode::TermTypeApp {
            fn_value: fn_value.clone(),
            arg: unit(),
        });
        assert_eq!(
            semantic_comparison(&neutral, &unit_arg).unwrap(),
            reflection_bindings::TypeComparison::Undetermined
        );
        assert_eq!(
            semantic_comparison(&neutral, &closed).unwrap(),
            reflection_bindings::TypeComparison::Different
        );
    }
    #[test]
    fn public_checked_comparison_records_hidden_missing_information() {
        let ctx = EvalCtx::new();
        let unknown = ReflectedVariable {
            name: "A".into(),
            arity: 0,
            meaning: reflection_bindings::VariableMeaning::Opened(EvalRef::new(
                reflection_bindings::OpenedScheme::Incomplete,
            )),
        };
        let left = Value::CheckedTerm(EvalRef::new(hidden_argument(unknown)));
        let right = Value::CheckedTerm(EvalRef::new(hidden_argument(opened("A"))));
        assert!(nf_eq_checked(&left, &right, &ctx).is_err());
        let neutral = Value::CheckedTerm(EvalRef::new(hidden_argument(
            ReflectedVariable::neutral("A".into(), 0),
        )));
        assert!(!nf_eq_checked(&neutral, &right, &ctx).unwrap());
        let rebuilt = Value::CheckedTerm(EvalRef::new(hidden_argument(opened("A"))));
        assert!(nf_eq_checked(&rebuilt, &right, &ctx).unwrap());
    }
    #[cfg(feature = "surface")]
    #[test]
    fn materialization_reserves_caller_export_names_before_transient_aliases() {
        let caller = ReflectedVariable::caller(
            "T".into(),
            0,
            crate::pass::typecheck_core::TypeBinderId::for_test(602),
        );
        let transient = opened("A");
        for reverse in [false, true] {
            let caller_term = EvalRef::new(hidden_argument(caller.clone()));
            let transient_term = EvalRef::new(hidden_argument(transient.clone()));
            let (left, right) = if reverse {
                (transient_term, caller_term)
            } else {
                (caller_term, transient_term)
            };
            let pair =
                CheckedTerm::from_node(CheckedTermNode::IntrinsicPair { left, right }, unit());
            let aligned = align_for_materialization(&pair);
            assert!(
                aligned
                    .ty
                    .environment
                    .get("T")
                    .unwrap()
                    .same_reference(&caller)
            );
            let refs = &free_references(&aligned)[&std::ptr::from_ref(&aligned)];
            assert_eq!(refs.len(), 2);
            assert!(
                refs.iter()
                    .any(|(name, variable)| name != "T" && variable.0.same_reference(&transient))
            );
        }
        let live = ReflectedVariable::live("X".into(), 0);
        let pair = CheckedTerm::from_node(
            CheckedTermNode::IntrinsicPair {
                left: EvalRef::new(hidden_argument(caller)),
                right: EvalRef::new(hidden_argument(live.clone())),
            },
            unit(),
        );
        let closed = close_forall(&pair, &live).unwrap();
        let Type::Forall { param, .. } = closed.ty() else {
            unreachable!()
        };
        assert_ne!(param.name, "T");
        assert!(
            align_for_materialization(&closed)
                .ty
                .environment
                .get("T")
                .is_some()
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn materialized_expression_and_type_share_the_same_caller_mapping() {
        let caller = ReflectedVariable::caller(
            "T".into(),
            0,
            crate::pass::typecheck_core::TypeBinderId::for_test(603),
        );
        let mut term = hidden_argument(caller.clone());
        term.ty = ReflectedType::from_parts(
            EvalRef::new(type_path("A")),
            None,
            ReflectionEnvironment::default().with("A".into(), caller),
        );
        let (expr, ty) = materialized_parts(&term);
        assert_eq!(term.clone_expr(), expr);
        assert_eq!(term.clone_type(), ty);
        let aligned = term.align_for_materialization();
        assert_eq!(aligned.ty(), &ty);
        assert_eq!(aligned.clone_expr(), expr);
        assert_eq!(ty, type_path("T"));
        let Expr::Call { args, .. } = expr else {
            unreachable!()
        };
        let [CallArg::Type(argument)] = args.as_slice() else {
            unreachable!()
        };
        assert_eq!(argument, &ty);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn template_export_aligns_hidden_and_result_types_without_materializing_slots() {
        let caller = ReflectedVariable::caller(
            "T".into(),
            0,
            crate::pass::typecheck_core::TypeBinderId::for_test(604),
        );
        let reflected = ReflectedType::from_parts(
            EvalRef::new(type_path("A")),
            None,
            ReflectionEnvironment::default().with("A".into(), caller),
        );
        let result_ty = ReflectedType::from_parts(
            EvalRef::new(Type::Product {
                left: Box::new(type_path("A")),
                right: Box::new(type_path("A")),
                meta: zero_meta(),
            }),
            None,
            reflected.environment.clone(),
        );
        let expected = Type::Product {
            left: Box::new(type_path("T")),
            right: Box::new(type_path("T")),
            meta: zero_meta(),
        };
        let term = CheckedTerm::from_reflected_node(
            CheckedTermNode::IntrinsicPair {
                left: EvalRef::new(CheckedTerm::template_value_reflected(0, reflected.clone())),
                right: EvalRef::new(CheckedTerm::template_value_reflected(1, reflected.clone())),
            },
            result_ty,
        );
        assert_eq!(term.clone_type(), expected);
        let aligned = term.align_for_materialization();
        assert_eq!(aligned.ty(), &expected);
        assert_eq!(aligned.template_value_arity(), 2);
        let CheckedTermNode::IntrinsicPair { left, right } = aligned.node.as_ref() else {
            unreachable!()
        };
        assert_eq!(left.ty(), &type_path("T"));
        assert_eq!(right.ty(), &type_path("T"));
        assert!(matches!(
            left.node.as_ref(),
            CheckedTermNode::TemplateValue { index: 0 }
        ));
        assert!(matches!(
            right.node.as_ref(),
            CheckedTermNode::TemplateValue { index: 1 }
        ));
    }

    #[test]
    fn independently_closed_checked_constructors_compare_with_corresponding_types() {
        let make = || {
            let live = ReflectedVariable::live("A".into(), 0);
            close_forall(&hidden_argument(live.clone()), &live).unwrap()
        };
        assert_eq!(
            semantic_comparison(&make(), &make()).unwrap(),
            reflection_bindings::TypeComparison::Equal
        );
        let make_renamed = |name: &str| {
            let live = ReflectedVariable::live("X".into(), 0);
            let argument = Type::Forall {
                param: TypeParam {
                    name: name.into(),
                    kind: None,
                    span: zero_span(),
                    presentation: Default::default(),
                },
                body: Box::new(type_path(name)),
                meta: zero_meta(),
            };
            let term = CheckedTerm::type_app_for_test(
                CheckedTerm::new(path_expr("f"), unit()),
                argument,
                unit(),
            );
            close_forall(&term, &live).unwrap()
        };
        let left = make_renamed("A");
        let right = make_renamed("T");
        let (
            CheckedTermNode::TermFn { sig: left_sig, .. },
            CheckedTermNode::TermFn { sig: right_sig, .. },
        ) = (left.node.as_ref(), right.node.as_ref())
        else {
            panic!("closing builds a type function")
        };
        assert_ne!(
            left_sig, right_sig,
            "the operands exercise different hygienic binder names"
        );
        assert_eq!(
            semantic_comparison(&left, &right).unwrap(),
            reflection_bindings::TypeComparison::Equal
        );
    }
    #[test]
    fn checked_snapshot_guards_hidden_references_and_retained_memory() {
        let left = hidden_argument(opened("A"));
        let same = left.clone();
        let different = hidden_argument(opened("A"));
        let key = |term| CheckedSnapshotKey::new(term, &mut 256).expect("small captured graph");
        assert_eq!(key(&left), key(&same));
        assert!(key(&left).presentation_matches(&key(&same)));
        assert_ne!(key(&left), key(&different));
        assert!(CheckedSnapshotKey::new(&left, &mut 0).is_none());
        let mut heavy = unit();
        for _ in 0..64 {
            heavy = Type::Product {
                left: Box::new(unit()),
                right: Box::new(heavy),
                meta: zero_meta(),
            };
        }
        let owner = ReflectedType::new_trusted(Type::Forall {
            param: TypeParam {
                name: "Unused".into(),
                kind: None,
                span: zero_span(),
                presentation: Default::default(),
            },
            body: Box::new(heavy),
            meta: zero_meta(),
        })
        .open_forall()
        .unwrap()
        .0;
        let mut retained = CheckedTerm::new(path_expr("value"), unit());
        retained.ty.environment = retained.ty.environment.with("Unused".into(), owner);
        assert!(CheckedSnapshotKey::new(&retained, &mut 32).is_none());
        assert!(
            CheckedSnapshotKey::new(&CheckedTerm::new(path_expr("value"), unit()), &mut 32)
                .is_some()
        );
    }

    #[test]
    fn checked_comparison_canonicalizes_only_bound_type_names() {
        let free = "__comparison_type_n0__";
        let value_name = "__ct_value_n91__";
        let term = CheckedTerm::from_node(
            CheckedTermNode::TermFn {
                sig: Signature::new(vec![
                    SignatureParam::Type(TypeParam {
                        name: "A".into(),
                        kind: None,
                        span: zero_span(),
                        presentation: Default::default(),
                    }),
                    SignatureParam::Value(Param {
                        name: value_name.into(),
                        ty: Some(unit()),
                        pattern: (),
                        meta: zero_meta(),
                    }),
                ]),
                body: EvalRef::new(CheckedTerm::type_app_for_test(
                    CheckedTerm::new(path_expr(value_name), unit()),
                    type_path(free),
                    unit(),
                )),
            },
            unit(),
        );
        let canonical =
            CheckedTermNameCanonicalizer::for_type_comparison(HashSet::from([free.into()]))
                .term(&term);
        let CheckedTermNode::TermFn { sig, body } = canonical.node.as_ref() else {
            unreachable!()
        };
        let SignatureParam::Type(bound) = &sig.params[0] else {
            unreachable!()
        };
        assert_ne!(bound.name, free);
        let SignatureParam::Value(value) = &sig.params[1] else {
            unreachable!()
        };
        assert_eq!(value.name, value_name);
        let CheckedTermNode::TermTypeApp { arg, .. } = body.node.as_ref() else {
            unreachable!()
        };
        assert_eq!(arg, &type_path(free));
    }

    #[test]
    fn checked_snapshot_comparison_keeps_shared_dags_shared() {
        let condition = EvalRef::new(CheckedTerm::new(path_expr("condition"), unit()));
        let mut body = EvalRef::new(CheckedTerm::new(path_expr("leaf"), unit()));
        for _ in 0..8 {
            body = EvalRef::new(CheckedTerm::from_node(
                CheckedTermNode::IntrinsicIfThenElse {
                    result_ty: unit(),
                    condition: condition.clone(),
                    true_body: body.clone(),
                    false_body: body,
                },
                unit(),
            ));
        }
        let left =
            CheckedSnapshotKey::new(&body, &mut 96).expect("small shared graph fits cache guard");
        let right =
            CheckedSnapshotKey::new(&body, &mut 96).expect("same shared graph fits cache guard");
        let (equal, work) = left.exact_match(&right);
        assert!(equal);
        assert_eq!(
            work, 10,
            "eight conditional nodes plus shared condition and leaf"
        );
    }

    #[test]
    fn checked_snapshot_does_not_reassign_hidden_refs_across_shared_nodes() {
        let condition = EvalRef::new(CheckedTerm::new(path_expr("condition"), unit()));
        let x = EvalRef::new(hidden_argument(opened("A")));
        let y = EvalRef::new(hidden_argument(opened("A")));
        let branch = |left: EvalRef<CheckedTerm>, right: EvalRef<CheckedTerm>| {
            EvalRef::new(CheckedTerm::from_node(
                CheckedTermNode::IntrinsicIfThenElse {
                    result_ty: unit(),
                    condition: condition.clone(),
                    true_body: left,
                    false_body: right,
                },
                unit(),
            ))
        };
        let tail = branch(y.clone(), x.clone());
        let left = branch(x, tail.clone());
        let right = branch(y, tail);
        let left = CheckedSnapshotKey::new(&left, &mut 256).unwrap();
        let right = CheckedSnapshotKey::new(&right, &mut 256).unwrap();
        assert_eq!(
            left.types, right.types,
            "the shared tail fixes the same admission visit order"
        );
        assert_ne!(
            left, right,
            "paired nodes must retain the same hidden references"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn template_adapter_preserves_hidden_and_split_slot_references() {
        let first = ReflectedVariable::live("T".into(), 0);
        let second = ReflectedVariable::live("T".into(), 0);
        let left = first.as_type();
        let right = second.as_type();
        let mut builder = reflection_bindings::ReflectionTypeBuilder::new([&left, &right]);
        let a = builder.import(&left).take();
        let b = builder.import(&right).take();
        let pair = builder.finish(Type::Product {
            left: Box::new(a),
            right: Box::new(b),
            meta: zero_meta(),
        });
        let template = CheckedTerm::template_value_reflected(0, pair.clone());
        let adapted = template.adapt_template_values(&CheckedTermTemplateValueAdapter {
            sources: vec![CheckedTermTemplateSource {
                ty: pair.clone(),
                force_if_unused: false,
            }],
            slots: vec![CheckedTermTemplateSlot {
                pieces: vec![
                    CheckedTermTemplatePiece {
                        source_index: 0,
                        source_slot: 0,
                        source_slot_tys: vec![left.clone(), right.clone()],
                    },
                    CheckedTermTemplatePiece {
                        source_index: 0,
                        source_slot: 1,
                        source_slot_tys: vec![left.clone(), right.clone()],
                    },
                ],
            }],
        });
        assert!(reflection_bindings::semantic_type_equal(&adapted.ty, &pair).unwrap());
        let free = free_references(&adapted);
        let free = &free[&std::ptr::from_ref(&adapted)];
        assert!(free.iter().any(|(_, v)| v.0.same_reference(&first)));
        assert!(free.iter().any(|(_, v)| v.0.same_reference(&second)));
        let hidden = hidden_argument(first.clone());
        let hidden_replaced = replace_templates(&hidden, &[]);
        assert_eq!(
            semantic_comparison(&hidden, &hidden_replaced).unwrap(),
            reflection_bindings::TypeComparison::Equal
        );
        assert!(
            free_references(&hidden_replaced)[&std::ptr::from_ref(&hidden_replaced)]
                .iter()
                .any(|(_, v)| v.0.same_reference(&first))
        );
    }
}
