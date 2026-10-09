//! Compositional value topology shared by public facades and typed stages.
//!
//! Named-call parameter partitions belong to `CallableExecutionLayout`. A
//! first-class function instead has one semantic domain, including Unit.
//! Rebuilding binary tails here makes that domain and structural values commute
//! with substitution even when a former abstract tail becomes a product or sum.

use super::boundary_facade::{
    BoundaryFacadePlan, CallableSourceParamAdapter, CallableSourceParamLayout, FacadeBinder,
    FacadeBinderId, FacadeUse, FacadeUseId, PreparedSchemeError, QualifiedTypeName,
};
use crate::ast::{FnTypeCapabilities, Kind, Module, Routed, Type, TypeParam};
use crate::pass::resolve::Package;
use std::collections::BTreeSet;
use std::fmt::Write as _;

#[cfg(test)]
thread_local! {
    static STAGE_DEPENDENCY_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) enum ValueDomainStep<T> {
    Value(ValueUseId),
    Product(T, T),
}

/// An injective native identifier for an alpha-stable stage skeleton. Names
/// cannot depend on which unrelated skeletons happen to share a package.
pub(crate) fn canonical_stage_symbol(identity: &str) -> String {
    let mut name = String::with_capacity("KioForall_".len() + identity.len() * 2);
    name.push_str("KioForall_");
    for byte in identity.bytes() {
        write!(name, "{byte:02x}").expect("writing an identifier to String cannot fail");
    }
    name
}

#[derive(Clone, Copy, PartialEq)]
enum IdentityPresentation {
    Exact,
    ReadableStage,
}

impl IdentityPresentation {
    fn token(self, out: &mut String, exact: char, readable: &str) {
        if self == Self::Exact {
            out.push(exact);
        } else {
            out.push_str(readable);
            out.push('_');
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ValueCapture {
    Use(ValueUseId),
    ApplicationPrefix {
        constructor: ValueUseId,
        args: Vec<ValueUseId>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValueStageShape {
    pub(crate) plan: ValuePlan,
    /// Entries correspond to `plan.ambient_binders()`, in occurrence order.
    /// Equal concrete types remain separate captures under substitution.
    pub(crate) captures: Vec<ValueCapture>,
}

pub(crate) enum ValueNominalDeclaration<'a> {
    Host(&'a crate::ast::HostType<Routed>),
    Newtype(&'a crate::ast::Newtype<Routed>),
}

impl ValueNominalDeclaration<'_> {
    pub(crate) fn kind(&self) -> Kind {
        let params = match self {
            Self::Host(declaration) => &declaration.type_params,
            Self::Newtype(declaration) => &declaration.type_params,
        };
        params.iter().rev().fold(Kind::Star, |result, param| {
            Kind::Arrow(Box::new(param.effective_kind()), Box::new(result))
        })
    }
}

/// Exact lookup only: preparation has already resolved imports and aliases.
/// This exposes a body-owned declaration, not a host-visible payload operation.
pub(crate) fn nominal_declaration<'a>(
    package: &'a Package<Routed>,
    name: &QualifiedTypeName,
) -> Option<ValueNominalDeclaration<'a>> {
    let entry = package.module(&name.module_segments().join("/"))?;
    let id = entry.scope.lookup(name.name())?;
    let declaration = crate::pass::resolve::declaration_by_id(&entry.module, id)?;
    if let Some(host) = declaration.host_type() {
        Some(ValueNominalDeclaration::Host(host))
    } else {
        declaration.newtype().map(ValueNominalDeclaration::Newtype)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ValueUseId(usize);

impl ValueUseId {
    pub(crate) fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ValueUse {
    Unit,
    Bottom,
    Bound(FacadeBinderId),
    Nominal(QualifiedTypeName),
    Apply {
        constructor: ValueUseId,
        args: Vec<ValueUseId>,
    },
    Product {
        left: ValueUseId,
        right: ValueUseId,
    },
    Sum {
        left: ValueUseId,
        right: ValueUseId,
    },
    Function {
        domain: ValueUseId,
        result: ValueUseId,
        caps: FnTypeCapabilities,
    },
    Forall {
        binder: FacadeBinderId,
        result: ValueUseId,
    },
}

/// Child indices precede parents, including synthesized binary tail nodes.
/// Binder identities are those of the input facade, not names rediscovered by
/// a renderer. Nominal applications retain their exact constructor identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValuePlan {
    uses: Vec<ValueUse>,
    source_uses: Vec<ValueUseId>,
    binders: Vec<FacadeBinder>,
    ambient: Vec<FacadeBinderId>,
    root: ValueUseId,
}

impl ValuePlan {
    /// Body storage preserves every plan field except diagnostic binder spans.
    /// It is consumed after validation; runtime projection and stage rendering
    /// depend on binder IDs and kinds, not their source locations.
    pub(crate) fn body_storage_identity(&self) -> impl Eq + std::hash::Hash + '_ {
        let Self {
            uses,
            source_uses,
            binders,
            ambient,
            root,
        } = self;
        let binders = binders
            .iter()
            .map(
                |FacadeBinder {
                     name,
                     kind,
                     span: _,
                 }| (name, kind),
            )
            .collect::<Vec<_>>();
        (uses, source_uses, binders, ambient, root)
    }

    pub(crate) fn from_facade(facade: &BoundaryFacadePlan) -> Self {
        let mut plan = Self {
            uses: Vec::with_capacity(facade.uses().len()),
            source_uses: Vec::with_capacity(facade.uses().len()),
            binders: facade.binders().to_vec(),
            ambient: Vec::new(),
            root: ValueUseId(0),
        };
        for source in facade.uses() {
            let use_ = match source {
                FacadeUse::Unit { .. } => ValueUse::Unit,
                FacadeUse::Bottom { .. } => ValueUse::Bottom,
                FacadeUse::Bound { binder, .. } => ValueUse::Bound(*binder),
                FacadeUse::Nominal { name, .. } => ValueUse::Nominal(name.clone()),
                FacadeUse::Apply {
                    constructor, args, ..
                } => {
                    let mut constructor = plan.source(*constructor);
                    let mut prefix = Vec::new();
                    if let ValueUse::Apply {
                        constructor: head,
                        args,
                    } = plan.use_at(constructor)
                    {
                        prefix.extend_from_slice(args);
                        constructor = *head;
                    }
                    prefix.extend(args.iter().map(|arg| plan.source(*arg)));
                    ValueUse::Apply {
                        constructor,
                        args: prefix,
                    }
                }
                FacadeUse::Product { args, .. } | FacadeUse::Sum { args, .. } => {
                    let sum = matches!(source, FacadeUse::Sum { .. });
                    let args = args.iter().map(|arg| plan.source(*arg)).collect::<Vec<_>>();
                    let root = plan.binary_spine(&args, sum);
                    plan.source_uses.push(root);
                    continue;
                }
                FacadeUse::Function {
                    slots,
                    result,
                    caps,
                    ..
                } => {
                    let slots = slots
                        .iter()
                        .map(|arg| plan.source(*arg))
                        .collect::<Vec<_>>();
                    let domain = if slots.is_empty() {
                        plan.push(ValueUse::Unit)
                    } else {
                        plan.binary_spine(&slots, false)
                    };
                    ValueUse::Function {
                        domain,
                        result: plan.source(*result),
                        caps: caps.clone(),
                    }
                }
                FacadeUse::Forall { binder, result, .. } => ValueUse::Forall {
                    binder: *binder,
                    result: plan.source(*result),
                },
            };
            let id = plan.push(use_);
            plan.source_uses.push(id);
        }
        plan.root = plan.source(facade.root());
        plan
    }

    pub(crate) fn root(&self) -> ValueUseId {
        self.root
    }

    pub(crate) fn from_routed_type(
        package: &Package<Routed>,
        module: &Module<Routed>,
        ambient: &[TypeParam],
        ty: &Type<Routed>,
    ) -> Result<Self, PreparedSchemeError> {
        let (facade, binders) = BoundaryFacadePlan::from_routed_type(package, module, ambient, ty)?;
        let mut plan = Self::from_facade(&facade);
        plan.ambient = binders;
        Ok(plan)
    }

    pub(crate) fn ambient_binders(&self) -> &[FacadeBinderId] {
        &self.ambient
    }

    /// A callable carrier's nominal identity cannot depend on the concrete
    /// types substituted for its surrounding binders. Capture those types as
    /// generic parameters, retaining only the active stage-dependent shape.
    pub(crate) fn stage_shape<E>(
        &self,
        root: ValueUseId,
        mut nominal_kind: impl FnMut(&QualifiedTypeName) -> Result<Kind, E>,
    ) -> Result<ValueStageShape, E> {
        assert!(matches!(self.use_at(root), ValueUse::Forall { .. }));
        // Only binders introduced beneath this root affect its carrier shape.
        // Preorder puts every enclosing binder before the binders it encloses.
        // Therefore the least free order is sufficient: leaving a forall can
        // remove its own least order only when no enclosing binder is used.
        let mut order = vec![None; self.binders.len()];
        let mut seen = vec![false; self.uses.len()];
        let mut pending = vec![root];
        let mut next_order = 0;
        while let Some(id) = pending.pop() {
            #[cfg(test)]
            STAGE_DEPENDENCY_STEPS.with(|steps| steps.set(steps.get() + 1));
            if std::mem::replace(&mut seen[id.index()], true) {
                continue;
            }
            match self.use_at(id) {
                ValueUse::Forall { binder, result } => {
                    order[binder.index()] = Some(next_order);
                    next_order += 1;
                    pending.push(*result);
                }
                ValueUse::Apply { constructor, args } => {
                    pending.extend(args.iter().rev());
                    pending.push(*constructor);
                }
                ValueUse::Product { left, right } | ValueUse::Sum { left, right } => {
                    pending.extend([*right, *left])
                }
                ValueUse::Function { domain, result, .. } => pending.extend([*result, *domain]),
                _ => {}
            }
        }
        let least = |left: Option<usize>, right: Option<usize>| match (left, right) {
            (Some(left), Some(right)) => Some(left.min(right)),
            _ => left.or(right),
        };
        let mut free = Vec::<Option<usize>>::with_capacity(self.uses.len());
        for node in &self.uses {
            #[cfg(test)]
            STAGE_DEPENDENCY_STEPS.with(|steps| steps.set(steps.get() + 1));
            let dependency = match node {
                ValueUse::Bound(binder) => order[binder.index()],
                ValueUse::Forall { binder, result } => {
                    let dependency = free[result.index()];
                    if dependency == order[binder.index()] {
                        None
                    } else {
                        dependency
                    }
                }
                ValueUse::Apply { constructor, args } => {
                    args.iter().fold(free[constructor.index()], |minimum, arg| {
                        least(minimum, free[arg.index()])
                    })
                }
                ValueUse::Product { left, right } | ValueUse::Sum { left, right } => {
                    least(free[left.index()], free[right.index()])
                }
                ValueUse::Function { domain, result, .. } => {
                    least(free[domain.index()], free[result.index()])
                }
                ValueUse::Unit | ValueUse::Bottom | ValueUse::Nominal(_) => None,
            };
            free.push(dependency);
        }
        enum Task {
            Enter(ValueUseId),
            Capture(ValueCapture),
            Binary(bool),
            Function(FnTypeCapabilities),
            Forall(FacadeBinderId),
            Apply,
        }
        let mut shaped = ValueStageShape {
            plan: Self {
                uses: Vec::new(),
                source_uses: Vec::new(),
                binders: self.binders.clone(),
                ambient: Vec::new(),
                root: ValueUseId(0),
            },
            captures: Vec::new(),
        };
        let mut values = Vec::new();
        let mut tasks = vec![Task::Enter(root)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Enter(id) => {
                    let independent = free[id.index()].is_none();
                    if id != root && independent {
                        tasks.push(Task::Capture(ValueCapture::Use(id)));
                        continue;
                    }
                    match self.use_at(id) {
                        ValueUse::Bound(binder) => {
                            values.push(shaped.plan.push(ValueUse::Bound(*binder)))
                        }
                        ValueUse::Forall { binder, result } => {
                            tasks.push(Task::Forall(*binder));
                            tasks.push(Task::Enter(*result));
                        }
                        ValueUse::Function {
                            domain,
                            result,
                            caps,
                        } => {
                            tasks.push(Task::Function(caps.clone()));
                            tasks.push(Task::Enter(*result));
                            tasks.push(Task::Enter(*domain));
                        }
                        ValueUse::Product { left, right } | ValueUse::Sum { left, right } => {
                            tasks.push(Task::Binary(matches!(
                                self.use_at(id),
                                ValueUse::Sum { .. }
                            )));
                            tasks.push(Task::Enter(*right));
                            tasks.push(Task::Enter(*left));
                        }
                        ValueUse::Apply { constructor, args } => {
                            let prefix = if free[constructor.index()].is_none() {
                                args.iter()
                                    .take_while(|arg| free[arg.index()].is_none())
                                    .count()
                            } else {
                                0
                            };
                            for arg in args[prefix..].iter().rev() {
                                tasks.push(Task::Apply);
                                tasks.push(Task::Enter(*arg));
                            }
                            tasks.push(if prefix > 0 {
                                Task::Capture(ValueCapture::ApplicationPrefix {
                                    constructor: *constructor,
                                    args: args[..prefix].to_vec(),
                                })
                            } else {
                                Task::Enter(*constructor)
                            });
                        }
                        ValueUse::Unit | ValueUse::Bottom | ValueUse::Nominal(_) => {
                            unreachable!("binder-independent leaves are captured")
                        }
                    }
                }
                Task::Capture(capture) => {
                    let kind = match &capture {
                        ValueCapture::Use(id) => self.kind(*id, &mut nominal_kind)?,
                        ValueCapture::ApplicationPrefix { constructor, args } => {
                            let mut kind = self.kind(*constructor, &mut nominal_kind)?;
                            for _ in args {
                                let Kind::Arrow(_, result) = kind else {
                                    unreachable!(
                                        "a checked partial application stays within its kind"
                                    )
                                };
                                kind = *result;
                            }
                            kind
                        }
                    };
                    let binder = FacadeBinderId::from_index(shaped.plan.binders.len());
                    shaped.plan.binders.push(FacadeBinder {
                        name: format!("Capture{}", shaped.captures.len()),
                        kind,
                        span: crate::span::Span::new(0, 0),
                    });
                    shaped.plan.ambient.push(binder);
                    shaped.captures.push(capture);
                    values.push(shaped.plan.push(ValueUse::Bound(binder)));
                }
                Task::Binary(sum) => {
                    let right = values
                        .pop()
                        .expect("a structural shape has its right child");
                    let left = values.pop().expect("a structural shape has its left child");
                    values.push(shaped.plan.push(if sum {
                        ValueUse::Sum { left, right }
                    } else {
                        ValueUse::Product { left, right }
                    }));
                }
                Task::Function(caps) => {
                    let result = values.pop().expect("a function shape has its result");
                    let domain = values.pop().expect("a function shape has its domain");
                    values.push(shaped.plan.push(ValueUse::Function {
                        domain,
                        result,
                        caps,
                    }));
                }
                Task::Forall(binder) => {
                    let result = values.pop().expect("a forall shape has its result");
                    values.push(shaped.plan.push(ValueUse::Forall { binder, result }));
                }
                Task::Apply => {
                    let arg = values.pop().expect("an application shape has its argument");
                    let constructor = values
                        .pop()
                        .expect("an application shape has its constructor");
                    values.push(shaped.plan.push(ValueUse::Apply {
                        constructor,
                        args: vec![arg],
                    }));
                }
            }
        }
        assert_eq!(values.len(), 1, "a staged shape has one root");
        shaped.plan.root = values.pop().unwrap();
        Ok(shaped)
    }

    /// The nominal resolver must use this plan's declaration snapshot: a
    /// retained public type need not have a declaration in the live package.
    pub(crate) fn kind<E>(
        &self,
        mut root: ValueUseId,
        mut nominal_kind: impl FnMut(&QualifiedTypeName) -> Result<Kind, E>,
    ) -> Result<Kind, E> {
        let mut applied = 0;
        let mut kind = loop {
            match self.use_at(root) {
                ValueUse::Bound(binder) => break self.binder(*binder).kind.clone(),
                ValueUse::Nominal(name) => break nominal_kind(name)?,
                ValueUse::Apply { constructor, args } => {
                    applied += args.len();
                    root = *constructor;
                }
                _ => break Kind::Star,
            }
        };
        for _ in 0..applied {
            let Kind::Arrow(_, result) = kind else {
                unreachable!("a checked type application stays within its constructor kind")
            };
            kind = *result;
        }
        Ok(kind)
    }

    /// Used free binders in lexical order. Unrelated enclosing binders never
    /// affect a generated carrier's identity or generic parameter list.
    pub(crate) fn free_binders(
        &self,
        root: ValueUseId,
        ambient: &[FacadeBinderId],
    ) -> Vec<FacadeBinderId> {
        let mut used = BTreeSet::new();
        let mut introduced = BTreeSet::new();
        let mut seen = vec![false; self.uses.len()];
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if std::mem::replace(&mut seen[id.index()], true) {
                continue;
            }
            match self.use_at(id) {
                ValueUse::Bound(binder) => {
                    used.insert(*binder);
                }
                ValueUse::Forall { binder, result } => {
                    introduced.insert(*binder);
                    pending.push(*result);
                }
                ValueUse::Apply { constructor, args } => {
                    pending.push(*constructor);
                    pending.extend(args);
                }
                ValueUse::Product { left, right } | ValueUse::Sum { left, right } => {
                    pending.extend([*left, *right]);
                }
                ValueUse::Function { domain, result, .. } => {
                    pending.extend([*domain, *result]);
                }
                ValueUse::Unit | ValueUse::Bottom | ValueUse::Nominal(_) => {}
            }
        }
        used.retain(|binder| !introduced.contains(binder));
        let result = ambient
            .iter()
            .copied()
            .filter(|binder| used.contains(binder))
            .collect::<Vec<_>>();
        assert_eq!(
            result.len(),
            used.len(),
            "a value plan has its complete ambient scope"
        );
        result
    }

    /// Fold the prepared source partition, rather than deriving native arity
    /// from the instantiated domain. A named Unit parameter remains nullary.
    pub(crate) fn native_parameter_domain<T, E>(
        &self,
        source: &CallableSourceParamLayout,
        slots: &[FacadeUseId],
        mut emit: impl FnMut(ValueDomainStep<T>) -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        let slots = slots
            .get(source.facade_slots())
            .expect("a prepared source parameter owns an in-range slot partition");
        match source.adapter() {
            CallableSourceParamAdapter::UnitValue => Ok(None),
            CallableSourceParamAdapter::Identity => {
                let [slot] = slots else {
                    unreachable!("an identity source parameter owns exactly one slot")
                };
                emit(ValueDomainStep::Value(self.source(*slot))).map(Some)
            }
            CallableSourceParamAdapter::RightNest => {
                let (last, prefix) = slots
                    .split_last()
                    .expect("a product source parameter owns at least one slot");
                let mut result = emit(ValueDomainStep::Value(self.source(*last)))?;
                for slot in prefix.iter().rev() {
                    let left = emit(ValueDomainStep::Value(self.source(*slot)))?;
                    result = emit(ValueDomainStep::Product(left, result))?;
                }
                Ok(Some(result))
            }
        }
    }

    /// Alpha-stable, injectively framed identity of a value type. Callers
    /// selecting a nested root supply its enclosing binders in lexical order.
    /// Source grouping, body ABI arity, names and spans are not type identity.
    pub(crate) fn identity(&self, root: ValueUseId, ambient: &[FacadeBinderId]) -> String {
        self.render_identity(root, ambient, IdentityPresentation::Exact)
    }

    /// Readable, injective presentation of an already-prepared stage shape.
    /// Exact identity, lexical indexing and this spelling share one traversal;
    /// native length budgets remain the consuming backend's responsibility.
    pub(crate) fn readable_stage_symbol(&self) -> String {
        assert!(
            matches!(self.use_at(self.root), ValueUse::Forall { .. }),
            "a stage symbol starts with its prepared Forall binder"
        );
        self.render_identity(
            self.root,
            &self.ambient,
            IdentityPresentation::ReadableStage,
        )
    }

    fn render_identity(
        &self,
        root: ValueUseId,
        ambient: &[FacadeBinderId],
        presentation: IdentityPresentation,
    ) -> String {
        enum Task {
            Use(ValueUseId),
            Leave,
        }
        let readable = presentation == IdentityPresentation::ReadableStage;
        let mut out = String::from(if readable { "KioForall_" } else { "V1;" });
        let mut scope = self.free_binders(root, ambient);
        if !readable {
            write!(out, "e{};", scope.len()).expect("writing to String cannot fail");
        } else if !scope.is_empty() {
            write!(out, "Captures{}_", scope.len()).expect("writing to String cannot fail");
        }
        for binder in &scope {
            write_kind_identity(&self.binder(*binder).kind, &mut out, presentation);
        }
        let mut tasks = vec![Task::Use(root)];
        while let Some(task) = tasks.pop() {
            let Task::Use(id) = task else {
                scope.pop();
                continue;
            };
            match self.use_at(id) {
                ValueUse::Unit => presentation.token(&mut out, 'u', "Unit"),
                ValueUse::Bottom => presentation.token(&mut out, 'z', "Never"),
                ValueUse::Bound(binder) => {
                    let depth = scope
                        .iter()
                        .rev()
                        .position(|candidate| candidate == binder)
                        .expect("a value identity has its complete lexical binder scope");
                    if readable {
                        write!(out, "T{depth}_").expect("writing to String cannot fail");
                    } else {
                        write!(out, "b{depth};").expect("writing to String cannot fail");
                    }
                }
                ValueUse::Nominal(name) => {
                    if readable {
                        write!(out, "Nominal{}_", name.module_segments().len())
                            .expect("writing to String cannot fail");
                    } else {
                        write!(out, "n{};", name.module_segments().len())
                            .expect("writing to String cannot fail");
                    }
                    for segment in name
                        .module_segments()
                        .iter()
                        .map(String::as_str)
                        .chain(std::iter::once(name.name()))
                    {
                        if readable {
                            let component = super::public_names::encode_source_identity(segment);
                            write!(out, "S{}_{component}_", component.len())
                                .expect("writing to String cannot fail");
                        } else {
                            write!(out, "{}:{segment}", segment.len())
                                .expect("writing to String cannot fail");
                        }
                    }
                }
                ValueUse::Apply { constructor, args } => {
                    if readable {
                        write!(out, "Apply{}_", args.len()).expect("writing to String cannot fail");
                    } else {
                        write!(out, "a{};", args.len()).expect("writing to String cannot fail");
                    }
                    tasks.extend(args.iter().rev().copied().map(Task::Use));
                    tasks.push(Task::Use(*constructor));
                }
                ValueUse::Product { left, right } | ValueUse::Sum { left, right } => {
                    if matches!(self.use_at(id), ValueUse::Product { .. }) {
                        presentation.token(&mut out, 'p', "Pair");
                    } else {
                        presentation.token(&mut out, 's', "Sum");
                    }
                    tasks.push(Task::Use(*right));
                    tasks.push(Task::Use(*left));
                }
                ValueUse::Function { domain, result, .. } => {
                    presentation.token(&mut out, 'f', "Fn");
                    tasks.push(Task::Use(*result));
                    tasks.push(Task::Use(*domain));
                }
                ValueUse::Forall { binder, result } => {
                    if !readable || id != root {
                        presentation.token(&mut out, 'q', "Forall");
                    }
                    write_kind_identity(&self.binder(*binder).kind, &mut out, presentation);
                    scope.push(*binder);
                    tasks.push(Task::Leave);
                    tasks.push(Task::Use(*result));
                }
            }
        }
        if readable {
            assert_eq!(out.pop(), Some('_'), "a readable stage ends after a token");
        }
        out
    }

    pub(crate) fn source(&self, id: FacadeUseId) -> ValueUseId {
        self.source_uses[id.index()]
    }

    pub(crate) fn use_at(&self, id: ValueUseId) -> &ValueUse {
        &self.uses[id.0]
    }

    pub(crate) fn use_id(&self, index: usize) -> ValueUseId {
        assert!(
            index < self.uses.len(),
            "value use index belongs to this plan"
        );
        ValueUseId(index)
    }

    pub(crate) fn uses(&self) -> &[ValueUse] {
        &self.uses
    }

    /// Split only the already-resolved arity; a substituted final slot stays
    /// whole even when its concrete value is itself a product or sum.
    pub(crate) fn right_spine_parts(
        &self,
        root: ValueUseId,
        arity: usize,
        sum: bool,
    ) -> Vec<ValueUseId> {
        if arity == 0 {
            return Vec::new();
        }
        let mut parts = Vec::with_capacity(arity);
        let mut current = root;
        for _ in 1..arity {
            let (left, right) = match (self.use_at(current), sum) {
                (ValueUse::Product { left, right }, false)
                | (ValueUse::Sum { left, right }, true) => (*left, *right),
                _ => unreachable!("a resolved structural arity fits its semantic value domain"),
            };
            parts.push(left);
            current = right;
        }
        parts.push(current);
        parts
    }

    pub(crate) fn binder(&self, id: FacadeBinderId) -> &FacadeBinder {
        &self.binders[id.index()]
    }

    fn push(&mut self, use_: ValueUse) -> ValueUseId {
        let id = ValueUseId(self.uses.len());
        self.uses.push(use_);
        id
    }

    fn binary_spine(&mut self, items: &[ValueUseId], sum: bool) -> ValueUseId {
        let (last, prefix) = items.split_last().expect("a structural spine is nonempty");
        prefix.iter().rev().fold(*last, |right, left| {
            self.push(if sum {
                ValueUse::Sum { left: *left, right }
            } else {
                ValueUse::Product { left: *left, right }
            })
        })
    }
}

fn write_kind_identity(kind: &Kind, out: &mut String, presentation: IdentityPresentation) {
    let mut pending = vec![kind];
    while let Some(kind) = pending.pop() {
        match kind {
            Kind::Star => presentation.token(out, '*', "Type"),
            Kind::Arrow(domain, result) => {
                presentation.token(out, '>', "Arrow");
                pending.push(result);
                pending.push(domain);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::boundary_facade::PreparedBoundaryScheme;
    use super::*;
    use crate::ast::Meta;
    use crate::span::Span;

    #[test]
    fn body_storage_identity_ignores_only_binder_spans() {
        let plan =
            ValuePlan::from_facade(&facade(&forall("A", function(path("A"), path("api.N")))));
        let mut moved = plan.clone();
        moved.binders[0].span = Span::new(10, 20);
        assert_ne!(plan, moved);
        assert!(plan.body_storage_identity() == moved.body_storage_identity());

        let changes: [fn(&mut ValuePlan); 8] = [
            |plan| plan.binders[0].name = "B".to_owned(),
            |plan| plan.binders[0].kind = Kind::arrow_chain(1),
            |plan| {
                let node = plan
                    .uses
                    .iter_mut()
                    .find(|node| matches!(node, ValueUse::Function { .. }))
                    .unwrap();
                let ValueUse::Function { caps, .. } = node else {
                    unreachable!()
                };
                caps.lifetime = crate::ast::Lifetime::Stack;
            },
            |plan| {
                let node = plan
                    .uses
                    .iter_mut()
                    .find(|node| matches!(node, ValueUse::Nominal(_)))
                    .unwrap();
                *node =
                    ValueUse::Nominal(QualifiedTypeName::new(vec!["api".to_owned()], "M").unwrap());
            },
            |plan| plan.source_uses.reverse(),
            |plan| plan.ambient.push(FacadeBinderId::from_index(0)),
            |plan| plan.root = ValueUseId(0),
            |plan| plan.uses.push(ValueUse::Unit),
        ];
        for change in changes {
            let mut changed = plan.clone();
            change(&mut changed);
            assert!(plan.body_storage_identity() != changed.body_storage_identity());
        }
    }

    #[test]
    fn stage_symbols_preserve_the_full_identity_without_registration() {
        for identity in ["", "a_b", "a/b", "α\0β", "forall(bound(0),capture(1))"] {
            let symbol = canonical_stage_symbol(identity);
            let hex = symbol.strip_prefix("KioForall_").unwrap();
            let bytes = hex
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(bytes, identity.as_bytes());
        }
        assert_ne!(canonical_stage_symbol("a_b"), canonical_stage_symbol("a/b"));
    }

    fn path(name: &str) -> Type<Routed> {
        Type::synth_path(
            name.split('.').map(str::to_owned).collect(),
            Vec::new(),
            Span::new(0, 0),
        )
    }

    fn binary(left: Type<Routed>, right: Type<Routed>, sum: bool) -> Type<Routed> {
        let meta = Meta::new(Span::new(0, 0));
        if sum {
            Type::Sum {
                left: Box::new(left),
                right: Box::new(right),
                meta,
            }
        } else {
            Type::Product {
                left: Box::new(left),
                right: Box::new(right),
                meta,
            }
        }
    }

    fn forall(name: &str, body: Type<Routed>) -> Type<Routed> {
        Type::Forall {
            param: TypeParam {
                name: name.to_owned(),
                span: Span::new(0, 0),
                kind: None,
            },
            body: Box::new(body),
            meta: Meta::new(Span::new(0, 0)),
        }
    }

    fn forall_kind(name: &str, kind: Kind, body: Type<Routed>) -> Type<Routed> {
        let mut ty = forall(name, body);
        let Type::Forall { param, .. } = &mut ty else {
            unreachable!()
        };
        param.kind = Some(kind);
        ty
    }

    fn function(domain: Type<Routed>, result: Type<Routed>) -> Type<Routed> {
        Type::Function {
            param: Box::new(domain),
            ret: Box::new(result),
            abi_arity: 1,
            caps: Default::default(),
            meta: Meta::new(Span::new(0, 0)),
        }
    }

    fn application(head: &str, arguments: Vec<Type<Routed>>) -> Type<Routed> {
        Type::synth_path(
            head.split('.').map(str::to_owned).collect(),
            arguments,
            Span::new(0, 0),
        )
    }

    fn shape(plan: &ValuePlan, root: ValueUseId) -> ValueStageShape {
        plan.stage_shape(root, |name| {
            Ok::<_, std::convert::Infallible>(if name.name() == "Either" {
                Kind::arrow_chain(2)
            } else {
                Kind::Star
            })
        })
        .unwrap()
    }

    fn shape_identity(shape: &ValueStageShape) -> String {
        shape
            .plan
            .identity(shape.plan.root(), shape.plan.ambient_binders())
    }

    #[test]
    fn readable_stage_names_share_exact_scope_kind_and_application_identity() {
        for (ty, exact, readable) in [
            (
                forall("A", function(path("A"), path("A"))),
                "V1;e0;q*fb0;b0;",
                "KioForall_Type_Fn_T0_T0",
            ),
            (
                forall("A", forall("B", function(path("A"), path("B")))),
                "V1;e0;q*q*fb1;b0;",
                "KioForall_Type_Forall_Type_Fn_T1_T0",
            ),
            (
                forall("A", forall("A", function(path("A"), path("A")))),
                "V1;e0;q*q*fb0;b0;",
                "KioForall_Type_Forall_Type_Fn_T0_T0",
            ),
            (
                forall_kind(
                    "F",
                    Kind::arrow_chain(1),
                    forall("A", application("F", vec![path("A")])),
                ),
                "V1;e0;q>**q*a1;b1;b0;",
                "KioForall_Arrow_Type_Type_Forall_Type_Apply1_T1_T0",
            ),
            (
                forall_kind(
                    "F",
                    Kind::arrow_chain(2),
                    forall(
                        "A",
                        forall("B", application("F", vec![path("A"), path("B")])),
                    ),
                ),
                "V1;e0;q>*>**q*q*a2;b2;b1;b0;",
                "KioForall_Arrow_Type_Arrow_Type_Type_Forall_Type_Forall_Type_Apply2_T2_T1_T0",
            ),
        ] {
            let plan = ValuePlan::from_facade(&facade(&ty));
            assert_eq!(plan.identity(plan.root(), &[]), exact);
            assert_eq!(plan.readable_stage_symbol(), readable);
        }
        let alpha = ValuePlan::from_facade(&facade(&forall(
            "Renamed",
            function(path("Renamed"), path("Renamed")),
        )));
        assert_eq!(alpha.readable_stage_symbol(), "KioForall_Type_Fn_T0_T0");
    }

    #[test]
    fn readable_stage_names_preserve_occurrence_captures_and_constructor_kinds() {
        let plan = ValuePlan::from_facade(&facade(&forall(
            "B",
            function(path("api.Captured"), path("B")),
        )));
        let shaped = shape(&plan, plan.root());
        assert_eq!(shape_identity(&shaped), "V1;e1;*q*fb1;b0;");
        assert_eq!(
            shaped.plan.readable_stage_symbol(),
            "KioForall_Captures1_Type_Type_Fn_T1_T0"
        );

        let plan = ValuePlan::from_facade(&facade(&forall(
            "B",
            application("api.Either", vec![path("api.A"), path("B")]),
        )));
        let shaped = shape(&plan, plan.root());
        assert_eq!(shape_identity(&shaped), "V1;e1;>**q*a1;b1;b0;");
        assert_eq!(
            shaped.plan.readable_stage_symbol(),
            "KioForall_Captures1_Arrow_Type_Type_Type_Apply1_T1_T0"
        );

        let plan = ValuePlan::from_facade(&facade(&forall(
            "B",
            function(
                binary(path("B"), path("api.A"), false),
                binary(path("B"), path("api.A"), false),
            ),
        )));
        let shaped = shape(&plan, plan.root());
        assert_eq!(shape_identity(&shaped), "V1;e2;**q*fpb0;b2;pb0;b1;");
        assert_eq!(
            shaped.plan.readable_stage_symbol(),
            "KioForall_Captures2_Type_Type_Type_Fn_Pair_T0_T2_Pair_T0_T1"
        );
    }

    #[test]
    fn stage_shape_captures_concrete_substitution_without_changing_carrier() {
        let generic = ValuePlan::from_facade(&facade(&forall(
            "A",
            forall("B", function(path("B"), path("A"))),
        )));
        let ValueUse::Forall { result, .. } = generic.use_at(generic.root()) else {
            panic!("outer binder")
        };
        let concrete = ValuePlan::from_facade(&facade(&forall(
            "B",
            function(path("B"), binary(path("api.X"), path("api.Y"), false)),
        )));
        assert_eq!(
            shape_identity(&shape(&generic, *result)),
            shape_identity(&shape(&concrete, concrete.root()))
        );
    }

    #[test]
    fn stage_shape_factors_partial_constructor_prefixes() {
        let generic = ValuePlan::from_facade(&facade(&forall_kind(
            "F",
            Kind::arrow_chain(1),
            forall("B", application("F", vec![path("B")])),
        )));
        let ValueUse::Forall { result, .. } = generic.use_at(generic.root()) else {
            panic!("constructor binder")
        };
        let concrete = ValuePlan::from_facade(&facade(&forall(
            "B",
            application("api.Either", vec![path("api.A"), path("B")]),
        )));
        let concrete_shape = shape(&concrete, concrete.root());
        assert_eq!(
            shape_identity(&shape(&generic, *result)),
            shape_identity(&concrete_shape)
        );
        let [ValueCapture::ApplicationPrefix { constructor, args }] = &concrete_shape.captures[..]
        else {
            panic!("partial constructor capture")
        };
        assert!(
            matches!(concrete.use_at(*constructor), ValueUse::Nominal(name) if name.name() == "Either")
        );
        assert!(
            matches!(&args[..], [arg] if matches!(concrete.use_at(*arg), ValueUse::Nominal(name) if name.name() == "A"))
        );
        assert_eq!(
            concrete_shape
                .plan
                .binder(concrete_shape.plan.ambient_binders()[0])
                .kind,
            Kind::arrow_chain(1)
        );
    }

    #[test]
    fn stage_shape_keeps_nested_scope_and_captures_independent_stages() {
        let shadow = ValuePlan::from_facade(&facade(&forall("B", forall("B", path("B")))));
        let shadow_shape = shape(&shadow, shadow.root());
        let ValueUse::Forall { result, .. } = shadow_shape.plan.use_at(shadow_shape.plan.root())
        else {
            panic!("stage")
        };
        assert!(matches!(
            shadow_shape.plan.use_at(*result),
            ValueUse::Bound(_)
        ));
        let [ValueCapture::Use(inner)] = &shadow_shape.captures[..] else {
            panic!("captured inner stage")
        };
        assert!(matches!(shadow.use_at(*inner), ValueUse::Forall { .. }));

        let dependent = ValuePlan::from_facade(&facade(&forall(
            "B",
            forall("C", function(path("C"), path("B"))),
        )));
        let dependent_shape = shape(&dependent, dependent.root());
        assert!(dependent_shape.captures.is_empty());
        let renamed = ValuePlan::from_facade(&facade(&forall(
            "X",
            forall("Y", function(path("Y"), path("X"))),
        )));
        assert_eq!(
            shape_identity(&dependent_shape),
            shape_identity(&shape(&renamed, renamed.root()))
        );
    }

    #[test]
    fn stage_shape_keeps_distinct_capture_occurrences_after_unification() {
        let make = |right| {
            forall(
                "B",
                function(
                    binary(path("B"), path("api.A"), false),
                    binary(path("B"), path(right), false),
                ),
            )
        };
        let distinct = ValuePlan::from_facade(&facade(&make("api.C")));
        let same = ValuePlan::from_facade(&facade(&make("api.A")));
        let distinct = shape(&distinct, distinct.root());
        let same = shape(&same, same.root());
        assert_eq!(distinct.captures.len(), 2);
        assert_eq!(same.captures.len(), 2);
        assert_eq!(shape_identity(&distinct), shape_identity(&same));
    }

    #[test]
    fn partial_bound_application_retains_its_residual_kind() {
        let plan = ValuePlan::from_facade(&facade(&forall_kind(
            "F",
            Kind::arrow_chain(2),
            application("F", vec![path("api.A")]),
        )));
        let ValueUse::Forall { result, .. } = plan.use_at(plan.root()) else {
            panic!("constructor binder")
        };
        assert_eq!(
            plan.kind(*result, |_| Ok::<_, ()>(Kind::Star)).unwrap(),
            Kind::arrow_chain(1)
        );
    }

    #[test]
    fn stage_dependency_summary_visits_wide_distinct_binders_linearly() {
        for width in [128, 512, 2048] {
            let mut plan = ValuePlan {
                uses: Vec::new(),
                source_uses: Vec::new(),
                ambient: Vec::new(),
                binders: (0..=width)
                    .map(|index| FacadeBinder {
                        name: format!("T{index}"),
                        kind: Kind::Star,
                        span: Span::new(0, 0),
                    })
                    .collect(),
                root: ValueUseId(0),
            };
            let children = (0..width)
                .map(|index| plan.push(ValueUse::Bound(FacadeBinderId::from_index(index))))
                .collect::<Vec<_>>();
            let product = plan.binary_spine(&children, false);
            plan.root = plan.push(ValueUse::Forall {
                binder: FacadeBinderId::from_index(width),
                result: product,
            });
            STAGE_DEPENDENCY_STEPS.with(|steps| steps.set(0));
            let result = shape(&plan, plan.root());
            assert_eq!(result.captures, vec![ValueCapture::Use(product)]);
            assert_eq!(result.plan.uses().len(), 2);
            assert_eq!(
                STAGE_DEPENDENCY_STEPS.with(std::cell::Cell::get),
                2 * plan.uses().len()
            );
        }
    }

    fn facade(ty: &Type<Routed>) -> BoundaryFacadePlan {
        BoundaryFacadePlan::from_prepared_scheme(
            PreparedBoundaryScheme::try_new(ty, &BTreeSet::new()).unwrap(),
        )
    }

    fn identity(ty: &Type<Routed>) -> String {
        let plan = ValuePlan::from_facade(&facade(ty));
        plan.identity(plan.root(), &[])
    }

    #[test]
    fn binary_values_commute_with_structural_tail_substitution() {
        for sum in [false, true] {
            let generic = facade(&forall("T", binary(path("api.A"), path("T"), sum)));
            let tail = binary(path("api.B"), path("api.C"), sum);
            let substituted = generic.apply_leading_type_arg(&facade(&tail)).unwrap();
            let plan = ValuePlan::from_facade(&substituted);
            assert_eq!(
                plan.identity(plan.root(), &[]),
                identity(&binary(path("api.A"), tail, sum))
            );
            assert_ne!(
                plan.identity(plan.root(), &[]),
                identity(&binary(
                    binary(path("api.A"), path("api.B"), sum),
                    path("api.C"),
                    sum
                )),
            );
        }
    }

    #[test]
    fn canonical_domain_keeps_unit_without_changing_facade_slots() {
        let unit = Type::Unit {
            meta: Meta::new(Span::new(0, 0)),
        };
        let ty = Type::Function {
            param: Box::new(unit),
            ret: Box::new(path("api.A")),
            abi_arity: 0,
            caps: Default::default(),
            meta: Meta::new(Span::new(0, 0)),
        };
        let facade = facade(&ty);
        assert!(
            matches!(facade.use_at(facade.root()), FacadeUse::Function { slots, .. } if slots.is_empty())
        );
        let plan = ValuePlan::from_facade(&facade);
        let ValueUse::Function { domain, .. } = plan.use_at(plan.root()) else {
            panic!("function")
        };
        assert_eq!(plan.use_at(*domain), &ValueUse::Unit);
    }

    #[test]
    fn identity_is_alpha_stable_and_preserves_shadowing() {
        assert_eq!(
            identity(&forall("A", forall("B", path("A")))),
            identity(&forall("X", forall("Y", path("X"))))
        );
        assert_ne!(
            identity(&forall("A", forall("B", path("A")))),
            identity(&forall("A", forall("A", path("A"))))
        );
    }

    #[test]
    fn nested_identity_ignores_unused_ambient_binders() {
        let plan = ValuePlan::from_facade(&facade(&forall(
            "Unused",
            forall("A", forall("T", path("A"))),
        )));
        let ValueUse::Forall {
            binder: unused,
            result,
        } = *plan.use_at(plan.root())
        else {
            panic!("outer")
        };
        let ValueUse::Forall {
            binder: used,
            result: nested,
        } = *plan.use_at(result)
        else {
            panic!("inner")
        };
        assert_eq!(plan.free_binders(nested, &[unused, used]), vec![used]);
        assert_eq!(
            plan.identity(nested, &[unused, used]),
            plan.identity(nested, &[used])
        );
    }

    #[test]
    fn applied_constructor_substitution_keeps_one_exact_head() {
        let mut applied = path("F");
        let Type::Path { args, .. } = &mut applied else {
            unreachable!()
        };
        args.push(path("api.B"));
        let generic = facade(&forall_kind("F", Kind::arrow_chain(1), applied));
        let mut partial = path("api.Pair");
        let Type::Path { args, .. } = &mut partial else {
            unreachable!()
        };
        args.push(path("api.A"));
        let substituted = generic.apply_leading_type_arg(&facade(&partial)).unwrap();
        let plan = ValuePlan::from_facade(&substituted);
        let mut direct = path("api.Pair");
        let Type::Path { args, .. } = &mut direct else {
            unreachable!()
        };
        args.extend([path("api.A"), path("api.B")]);
        assert_eq!(plan.identity(plan.root(), &[]), identity(&direct));
    }
}
