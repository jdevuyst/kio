//! Executable stages and lexical environments of checked routed bodies.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Expr, Newtype, Routed, Signature, SignatureGroupRef, SignatureParam, Type, TypeParam,
};

use super::boundary_facade::{
    BoundaryFacadeExecutionUse, BoundaryFacadePlan, CallableExecutionLayout,
    CallableExecutionStage, FacadeBinderId, FacadeUseId,
};
use super::tail_plan::{TailContext, TailStage};
use super::value_plan::{ValuePlan, ValueUse, ValueUseId};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct BindingId(usize);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct EntryId(usize);

impl EntryId {
    pub(crate) fn index(self) -> usize {
        self.0
    }
}

impl BindingId {
    pub(crate) fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TypeBindingId(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BindingOrigin {
    Parameter { entry: EntryId, stage: usize },
    Local { entry: EntryId },
}

#[derive(Debug)]
pub(crate) struct ValueBinding<'a> {
    pub(crate) name: &'a str,
    pub(crate) ty: Option<&'a Type<Routed>>,
    pub(crate) origin: BindingOrigin,
}

#[derive(Debug)]
pub(crate) struct TypeBinding<'a> {
    pub(crate) param: &'a TypeParam,
    pub(crate) entry: EntryId,
    pub(crate) stage: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum StageInput<'a> {
    Type(&'a TypeParam),
    Value(&'a [SignatureParam<Routed>]),
}

impl<'a> StageInput<'a> {
    pub(crate) fn param_names(self) -> Vec<&'a str> {
        match self {
            Self::Type(_) => Vec::new(),
            Self::Value(params) => params
                .iter()
                .map(|param| {
                    let SignatureParam::Value(param) = param else {
                        unreachable!("a checked value stage contains only value binders")
                    };
                    param.name.as_str()
                })
                .collect(),
        }
    }

    pub(crate) fn is_value(self) -> bool {
        matches!(self, Self::Value(_))
    }
}

pub(crate) fn signature_stages(sig: &Signature<Routed>) -> Vec<StageInput<'_>> {
    let mut stages = Vec::new();
    for group in sig.canonical_group_refs() {
        match group {
            SignatureGroupRef::Type(params) => stages.extend(params.iter().map(|param| {
                let SignatureParam::Type(param) = param else {
                    unreachable!("a checked type stage contains a type binder")
                };
                StageInput::Type(param)
            })),
            SignatureGroupRef::Value(params) => stages.push(StageInput::Value(params)),
        }
    }
    if stages.is_empty() {
        stages.push(StageInput::Value(&[]));
    }
    stages
}

#[derive(Debug)]
pub(crate) struct ExecutableStage<'a> {
    pub(crate) input: StageInput<'a>,
    pub(crate) bindings: Vec<BindingId>,
    pub(crate) type_binding: Option<TypeBindingId>,
    pub(crate) captures: Vec<BindingId>,
}

pub(crate) struct ExecutableEntry<'a> {
    pub(crate) id: EntryId,
    pub(crate) stages: Vec<ExecutableStage<'a>>,
    pub(crate) type_scope: Vec<TypeBindingId>,
    pub(crate) parent: Option<EntryId>,
    pub(crate) requires_owner: bool,
    body: &'a Expr<Routed>,
}

impl<'a> ExecutableEntry<'a> {
    pub(crate) fn body(&self) -> TailContext<'a> {
        TailContext::body(self.body)
    }

    pub(crate) fn captures(&self) -> &[BindingId] {
        &self.stages[0].captures
    }

    pub(crate) fn typed_stages(&self, plan: &ValuePlan) -> Vec<TypedStage> {
        typed_stages(self.stages.iter().map(|stage| stage.input), plan)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedStage {
    pub(crate) use_id: ValueUseId,
    pub(crate) binder: Option<FacadeBinderId>,
    previous_inputs: usize,
}

impl TypedStage {
    /// A prepared host invocation consumes every preceding input at its final stage.
    pub(crate) fn captured_stages(self) -> std::ops::Range<usize> {
        0..self.previous_inputs
    }
}

pub(crate) struct CallableSpine {
    pub(crate) type_stages: Vec<TypedStage>,
    pub(crate) function: ValueUseId,
    pub(crate) domain: ValueUseId,
    pub(crate) result: ValueUseId,
}

impl CallableSpine {
    pub(crate) fn outer_witness_captures(
        &self,
        plan: &ValuePlan,
        ambient: &[FacadeBinderId],
    ) -> Vec<FacadeBinderId> {
        let root = self
            .type_stages
            .first()
            .map_or(self.function, |stage| stage.use_id);
        plan.free_binders(root, ambient)
    }

    pub(crate) fn invocations(&self) -> impl Iterator<Item = CpsInvocationStage> {
        continuation_invocations(self.type_stages.len())
    }
}

pub(crate) fn callable_spine(plan: &ValuePlan, root: ValueUseId) -> CallableSpine {
    let mut current = root;
    let mut type_stages = Vec::new();
    while let ValueUse::Forall { binder, result } = plan.use_at(current) {
        type_stages.push(TypedStage {
            use_id: current,
            binder: Some(*binder),
            previous_inputs: type_stages.len(),
        });
        current = *result;
    }
    let ValueUse::Function { domain, result, .. } = plan.use_at(current) else {
        unreachable!("a checked callable spine terminates in its value stage")
    };
    CallableSpine {
        type_stages,
        function: current,
        domain: *domain,
        result: *result,
    }
}

pub(crate) struct CpsEntryPlan {
    pub(crate) entry: CallableSpine,
    pub(crate) continuation: CallableSpine,
}

pub(crate) fn cps_entry_plan(plan: &ValuePlan, root: ValueUseId) -> CpsEntryPlan {
    let entry = callable_spine(plan, root);
    let continuation = callable_spine(plan, entry.domain);
    CpsEntryPlan {
        entry,
        continuation,
    }
}

/// Synthetic binders have recipe-local identities, independent of source spellings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CpsBinding {
    Payload,
    Continuation,
}

impl CpsBinding {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Payload => "payload",
            Self::Continuation => "continuation",
        }
    }
}

pub(crate) struct CpsRuntimePlan {
    pub(crate) type_stages: usize,
    pub(crate) continuation_type_stages: usize,
    pub(crate) payload_arity: usize,
}

pub(crate) struct CpsInvocationStage {
    pub(crate) stage: TailStage,
    pub(crate) type_index: Option<usize>,
    pub(crate) resume: Option<usize>,
}

impl CpsRuntimePlan {
    pub(crate) fn invocations(&self) -> impl Iterator<Item = CpsInvocationStage> + '_ {
        continuation_invocations(self.continuation_type_stages)
    }

    pub(crate) fn captures(&self) -> &'static [CpsBinding] {
        match self.payload_arity {
            0 => &[],
            1 => &[CpsBinding::Payload],
            _ => unreachable!("a checked CPS continuation receives at most one payload"),
        }
    }
}

fn continuation_invocations(count: usize) -> impl Iterator<Item = CpsInvocationStage> {
    (0..=count).map(move |index| {
        let is_type = index < count;
        CpsInvocationStage {
            stage: if is_type {
                TailStage::Type
            } else {
                TailStage::Value
            },
            type_index: is_type.then_some(index),
            resume: is_type.then_some(index),
        }
    })
}

pub(crate) fn cps_newtype_runtime_plan(declaration: &Newtype<Routed>) -> CpsRuntimePlan {
    assert!(
        !declaration.existential_params.is_empty(),
        "a CPS recipe belongs to an existential newtype"
    );
    CpsRuntimePlan {
        type_stages: 1,
        continuation_type_stages: declaration.existential_params.len(),
        payload_arity: 1,
    }
}

impl CpsEntryPlan {
    pub(crate) fn stage_witness_captures(
        &self,
        plan: &ValuePlan,
        ambient: &[FacadeBinderId],
    ) -> Vec<Vec<FacadeBinderId>> {
        let mut scope = ambient.to_vec();
        let mut captures = Vec::new();
        for stage in &self.entry.type_stages {
            captures.push(plan.free_binders(stage.use_id, &scope));
            scope.push(
                stage
                    .binder
                    .expect("a planned type stage binds its witness"),
            );
        }
        captures.push(plan.free_binders(self.entry.domain, &scope));
        captures
    }

    pub(crate) fn runtime(&self) -> CpsRuntimePlan {
        CpsRuntimePlan {
            type_stages: self.entry.type_stages.len(),
            continuation_type_stages: self.continuation.type_stages.len(),
            payload_arity: 1,
        }
    }
}

pub(crate) fn cps_boundary_runtime_plan(
    facade: &BoundaryFacadePlan,
    execution: &CallableExecutionLayout,
    root: FacadeUseId,
) -> CpsRuntimePlan {
    let values = ValuePlan::from_facade(facade);
    let recipe = cps_entry_plan(&values, values.source(root));
    let source = facade
        .uses_with_ids()
        .map(|(id, _)| (values.source(id), id))
        .collect::<HashMap<_, _>>();
    let actions = execution.root_uses();
    let type_count = |stages: &[TypedStage]| {
        stages
            .iter()
            .filter(|stage| match actions.use_at(source[&stage.use_id]) {
                BoundaryFacadeExecutionUse::InvokeForall => true,
                BoundaryFacadeExecutionUse::DeclarationBinder => false,
                _ => unreachable!("a checked forall has its prepared execution action"),
            })
            .count()
    };
    let BoundaryFacadeExecutionUse::Function(entry) =
        actions.use_at(source[&recipe.entry.function])
    else {
        unreachable!("a checked CPS entry has its prepared value stage")
    };
    assert_eq!(
        entry.body_abi_arity(),
        1,
        "a CPS entry binds one continuation"
    );
    let BoundaryFacadeExecutionUse::Function(continuation) =
        actions.use_at(source[&recipe.continuation.function])
    else {
        unreachable!("a checked CPS continuation has its prepared value stage")
    };
    let payload_arity = continuation.body_abi_arity();
    assert!(
        payload_arity <= 1,
        "a CPS continuation receives at most one payload"
    );
    CpsRuntimePlan {
        type_stages: type_count(&recipe.entry.type_stages),
        continuation_type_stages: type_count(&recipe.continuation.type_stages),
        payload_arity,
    }
}

pub(crate) fn cps_continuation_runtime_shape(ty: &Type<Routed>) -> (usize, usize) {
    let (type_stages, callable) = ty.peel_leading_foralls();
    let Type::Function { abi_arity, .. } = callable else {
        unreachable!("a routed CPS continuation has a function value stage")
    };
    assert!(
        *abi_arity <= 1,
        "a CPS continuation receives at most one payload"
    );
    (type_stages, *abi_arity)
}

pub(crate) fn typed_stages<'a>(
    stages: impl IntoIterator<Item = StageInput<'a>>,
    plan: &ValuePlan,
) -> Vec<TypedStage> {
    pair_typed_stages(stages.into_iter().map(StageInput::is_value), plan)
}

pub(crate) fn typed_execution_stages(
    execution: &CallableExecutionLayout,
    plan: &ValuePlan,
) -> Vec<TypedStage> {
    pair_typed_stages(
        execution
            .head_stages()
            .iter()
            .map(|stage| matches!(stage, CallableExecutionStage::Value(_))),
        plan,
    )
}

fn pair_typed_stages(stages: impl IntoIterator<Item = bool>, plan: &ValuePlan) -> Vec<TypedStage> {
    let mut current = plan.root();
    stages
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let (result, binder) = match (value, plan.use_at(current)) {
                (false, ValueUse::Forall { binder, result }) => (*result, Some(*binder)),
                (true, ValueUse::Function { result, .. }) => (*result, None),
                _ => unreachable!("checked abstraction stages agree with their value plan"),
            };
            let stage = TypedStage {
                use_id: current,
                binder,
                previous_inputs: index,
            };
            current = result;
            stage
        })
        .collect()
}

#[derive(Clone, Copy)]
pub(crate) enum WitnessContext {
    Execute { entry: EntryId, stage: usize },
    Construct { entry: EntryId, stage: usize },
}

#[derive(Default)]
pub(crate) struct WitnessCaptures {
    stages: HashMap<(EntryId, usize), Vec<TypeBindingId>>,
}

impl WitnessCaptures {
    pub(crate) fn dependencies(
        bodies: &ExecutableBodyPlan<'_>,
        context: WitnessContext,
        plan: &ValuePlan,
        use_id: ValueUseId,
        scope: &[(FacadeBinderId, TypeBindingId)],
    ) -> Vec<TypeBindingId> {
        let (entry, stage, include_input) = match context {
            WitnessContext::Execute { entry, stage } => (entry, stage, true),
            WitnessContext::Construct { entry, stage } => (entry, stage, false),
        };
        let available = bodies
            .entry(entry)
            .type_scope
            .iter()
            .copied()
            .filter(|id| {
                let binding = bodies.type_binding(*id);
                binding.entry != entry
                    || binding.stage < stage
                    || (include_input && binding.stage == stage)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            scope.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
            available,
            "witness mappings preserve the complete ordered lexical type scope"
        );
        let ambient = plan
            .uses()
            .iter()
            .filter_map(|use_| match use_ {
                ValueUse::Bound(binder) => Some(*binder),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        plan.free_binders(use_id, &ambient)
            .into_iter()
            .map(|binder| {
                let binding = scope
                    .iter()
                    .find(|(candidate, _)| *candidate == binder)
                    .expect("every free witness binder has an exact lexical mapping")
                    .1;
                let source = bodies.type_binding(binding).param;
                let selected = plan.binder(binder);
                assert_eq!(
                    source.name, selected.name,
                    "witness mapping preserves its binder"
                );
                assert_eq!(
                    source.effective_kind(),
                    selected.kind,
                    "witness mapping preserves its binder kind"
                );
                binding
            })
            .collect()
    }

    pub(crate) fn record(
        &mut self,
        bodies: &ExecutableBodyPlan<'_>,
        context: WitnessContext,
        plan: &ValuePlan,
        use_id: ValueUseId,
        scope: &[(FacadeBinderId, TypeBindingId)],
    ) {
        for binding in Self::dependencies(bodies, context, plan, use_id, scope) {
            self.require(bodies, context, binding);
        }
    }

    pub(crate) fn captures(&self, entry: EntryId, stage: usize) -> &[TypeBindingId] {
        self.stages.get(&(entry, stage)).map_or(&[], Vec::as_slice)
    }

    fn require(
        &mut self,
        bodies: &ExecutableBodyPlan<'_>,
        mut context: WitnessContext,
        binding: TypeBindingId,
    ) {
        let source = bodies.type_binding(binding);
        loop {
            let (entry, stage) = match context {
                WitnessContext::Execute { entry, stage } => (entry, stage),
                WitnessContext::Construct { entry, stage } if stage > 0 => (entry, stage - 1),
                WitnessContext::Construct { entry, .. } => {
                    let parent = bodies
                        .entry(entry)
                        .parent
                        .expect("a named entry's construction has no lexical type dependencies");
                    (parent, bodies.entry(parent).stages.len() - 1)
                }
            };
            assert!(stage < bodies.entry(entry).stages.len());
            let start = if source.entry == entry {
                assert!(
                    source.stage <= stage,
                    "a witness cannot depend on an unexecuted type stage"
                );
                source.stage + 1
            } else {
                assert!(
                    bodies.entry(entry).type_scope.contains(&binding),
                    "a witness dependency belongs to the entry's lexical type scope"
                );
                0
            };
            for stage in start..=stage {
                let captures = self.stages.entry((entry, stage)).or_default();
                if !captures.contains(&binding) {
                    captures.push(binding);
                }
            }
            if source.entry == entry {
                return;
            }
            context = WitnessContext::Construct { entry, stage: 0 };
        }
    }
}

pub(crate) struct ExecutableBodyPlan<'a> {
    entries: Vec<ExecutableEntry<'a>>,
    bindings: Vec<ValueBinding<'a>>,
    type_bindings: Vec<TypeBinding<'a>>,
    entry_by_body: HashMap<usize, EntryId>,
    references: HashMap<usize, BindingId>,
    locals: HashMap<usize, BindingId>,
    arms: HashMap<(usize, usize), BindingId>,
    scopes: HashMap<usize, Option<usize>>,
    scope_nodes: Vec<(BindingId, Option<usize>)>,
    definitions: HashMap<BindingId, BindingDefinition<'a>>,
}

#[derive(Clone, Copy)]
pub(crate) enum BindingDefinition<'a> {
    Local {
        value: &'a Expr<Routed>,
    },
    Arm {
        source: &'a Expr<Routed>,
        index: usize,
    },
}

impl<'a> ExecutableBodyPlan<'a> {
    pub(crate) fn new(sig: &'a Signature<Routed>, body: &'a Expr<Routed>) -> Self {
        let mut builder = Builder {
            plan: Self {
                entries: Vec::new(),
                bindings: Vec::new(),
                type_bindings: Vec::new(),
                entry_by_body: HashMap::new(),
                references: HashMap::new(),
                locals: HashMap::new(),
                arms: HashMap::new(),
                scopes: HashMap::new(),
                scope_nodes: Vec::new(),
                definitions: HashMap::new(),
            },
            values: HashMap::new(),
            types: Vec::new(),
            scope: None,
        };
        builder.callable(sig, body, None);
        builder.plan
    }

    pub(crate) fn entry_for_body(&self, body: &Expr<Routed>) -> &ExecutableEntry<'a> {
        let id = self
            .entry_by_body
            .get(&(body as *const Expr<Routed> as usize))
            .expect("an executable entry belongs to the planned routed body");
        &self.entries[id.0]
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn entry_for_body_mut(&mut self, body: &Expr<Routed>) -> &mut ExecutableEntry<'a> {
        let id = self.entry_by_body[&(body as *const Expr<Routed> as usize)];
        &mut self.entries[id.0]
    }

    pub(crate) fn entry(&self, id: EntryId) -> &ExecutableEntry<'a> {
        &self.entries[id.0]
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = &ExecutableEntry<'a>> {
        self.entries.iter()
    }

    pub(crate) fn binding(&self, id: BindingId) -> &ValueBinding<'a> {
        &self.bindings[id.0]
    }

    pub(crate) fn reference_binding(&self, expr: &Expr<Routed>) -> BindingId {
        self.references[&(expr as *const Expr<Routed> as usize)]
    }

    pub(crate) fn local_binding(&self, expr: &Expr<Routed>) -> BindingId {
        self.locals[&(expr as *const Expr<Routed> as usize)]
    }

    pub(crate) fn arm_binding(&self, expr: &Expr<Routed>, index: usize) -> BindingId {
        self.arms[&(expr as *const Expr<Routed> as usize, index)]
    }

    pub(crate) fn binding_definition(&self, binding: BindingId) -> Option<BindingDefinition<'a>> {
        self.definitions.get(&binding).copied()
    }

    /// Exact active lexical identities, nearest binding first. The persistent
    /// parent chain shares prefixes instead of copying a scope at every node.
    pub(crate) fn scope_bindings(&self, expr: &Expr<Routed>) -> Vec<BindingId> {
        let mut scope = self.scopes[&(expr as *const Expr<Routed> as usize)];
        let mut names = HashSet::new();
        let mut result = Vec::new();
        while let Some(index) = scope {
            let (binding, parent) = self.scope_nodes[index];
            if names.insert(self.binding(binding).name) {
                result.push(binding);
            }
            scope = parent;
        }
        result
    }

    pub(crate) fn type_binding(&self, id: TypeBindingId) -> &TypeBinding<'a> {
        &self.type_bindings[id.0]
    }

    pub(crate) fn nested_capture_names(&self) -> Vec<Vec<String>> {
        self.entries
            .iter()
            .skip(1)
            .map(|entry| {
                entry
                    .captures()
                    .iter()
                    .map(|id| self.binding(*id).name.to_owned())
                    .collect()
            })
            .collect()
    }
}

struct Builder<'a> {
    plan: ExecutableBodyPlan<'a>,
    values: HashMap<&'a str, BindingId>,
    types: Vec<TypeBindingId>,
    scope: Option<usize>,
}

#[derive(Default)]
struct Uses {
    values: Vec<BindingId>,
    seen: HashSet<BindingId>,
    owner: bool,
}

impl Uses {
    fn insert(&mut self, id: BindingId) {
        if self.seen.insert(id) {
            self.values.push(id);
        }
    }
}

impl<'a> Builder<'a> {
    fn bind(
        &mut self,
        name: &'a str,
        ty: Option<&'a Type<Routed>>,
        origin: BindingOrigin,
    ) -> (BindingId, Option<BindingId>) {
        let id = BindingId(self.plan.bindings.len());
        self.plan.bindings.push(ValueBinding { name, ty, origin });
        self.plan.scope_nodes.push((id, self.scope));
        self.scope = Some(self.plan.scope_nodes.len() - 1);
        (id, self.values.insert(name, id))
    }

    fn restore(&mut self, name: &'a str, previous: Option<BindingId>) {
        self.scope = self.plan.scope_nodes[self.scope.expect("a restored binding has a scope")].1;
        if let Some(previous) = previous {
            self.values.insert(name, previous);
        } else {
            self.values.remove(name);
        }
    }

    fn callable(
        &mut self,
        sig: &'a Signature<Routed>,
        body: &'a Expr<Routed>,
        parent: Option<EntryId>,
    ) -> EntryId {
        let id = EntryId(self.plan.entries.len());
        self.plan.entries.push(ExecutableEntry {
            id,
            body,
            stages: Vec::new(),
            type_scope: Vec::new(),
            parent,
            requires_owner: false,
        });
        assert!(
            self.plan
                .entry_by_body
                .insert(body as *const Expr<Routed> as usize, id)
                .is_none(),
            "each callable has a distinct routed body occurrence"
        );
        let saved_types = self.types.len();
        let mut previous_values = Vec::new();
        let mut stages = Vec::new();
        for (stage, input) in signature_stages(sig).into_iter().enumerate() {
            let mut bindings = Vec::new();
            let mut type_binding = None;
            match input {
                StageInput::Type(param) => {
                    let binder = TypeBindingId(self.plan.type_bindings.len());
                    self.plan.type_bindings.push(TypeBinding {
                        param,
                        entry: id,
                        stage,
                    });
                    self.types.push(binder);
                    type_binding = Some(binder);
                }
                StageInput::Value(params) => {
                    for param in params {
                        let SignatureParam::Value(param) = param else {
                            unreachable!("checked value stages contain value parameters")
                        };
                        let (binding, previous) = self.bind(
                            &param.name,
                            param.ty.as_ref(),
                            BindingOrigin::Parameter { entry: id, stage },
                        );
                        bindings.push(binding);
                        previous_values.push((param.name.as_str(), previous));
                    }
                }
            }
            stages.push(ExecutableStage {
                input,
                bindings,
                type_binding,
                captures: Vec::new(),
            });
        }
        self.plan.entries[id.0].type_scope = self.types.clone();
        let mut uses = Uses::default();
        self.walk(body, id, &mut uses);
        for (stage_index, stage) in stages.iter_mut().enumerate() {
            stage.captures = uses
                .values
                .iter()
                .copied()
                .filter(|binding| match self.plan.binding(*binding).origin {
                    BindingOrigin::Parameter { entry, stage } => entry != id || stage < stage_index,
                    BindingOrigin::Local { entry } => entry != id,
                })
                .collect();
        }
        self.plan.entries[id.0].stages = stages;
        self.plan.entries[id.0].requires_owner = uses.owner;
        for (name, previous) in previous_values.into_iter().rev() {
            self.restore(name, previous);
        }
        self.types.truncate(saved_types);
        id
    }

    fn reference(&mut self, expr: &Expr<Routed>, name: &str, uses: &mut Uses) {
        let binding = *self
            .values
            .get(name)
            .expect("a checked routed local reference has an active lexical binder");
        self.plan
            .references
            .insert(expr as *const Expr<Routed> as usize, binding);
        uses.insert(binding);
    }

    fn walk(&mut self, expr: &'a Expr<Routed>, entry: EntryId, uses: &mut Uses) {
        self.plan
            .scopes
            .insert(expr as *const Expr<Routed> as usize, self.scope);
        match expr {
            Expr::LowBoundRef { name, .. } => self.reference(expr, name, uses),
            Expr::LowClosureCall { name, args, .. } => {
                self.reference(expr, name, uses);
                for arg in args {
                    self.walk(arg, entry, uses);
                }
            }
            Expr::FnExpr { sig, body, .. } => {
                let child = self.callable(sig, body, Some(entry));
                for capture in self.plan.entries[child.0].captures() {
                    uses.insert(*capture);
                }
                uses.owner |= self.plan.entries[child.0].requires_owner;
            }
            Expr::Let {
                name,
                ty,
                value,
                body,
                ..
            } => {
                self.walk(value, entry, uses);
                let (binding, previous) =
                    self.bind(name, ty.as_ref(), BindingOrigin::Local { entry });
                self.plan
                    .locals
                    .insert(expr as *const Expr<Routed> as usize, binding);
                self.plan
                    .definitions
                    .insert(binding, BindingDefinition::Local { value });
                self.walk(body, entry, uses);
                self.restore(name, previous);
            }
            Expr::Seq { value, body, .. } => {
                self.walk(value, entry, uses);
                self.walk(body, entry, uses);
            }
            Expr::LowIndirectCall { callee, args, .. } => {
                self.walk(callee, entry, uses);
                for arg in args {
                    self.walk(arg, entry, uses);
                }
            }
            Expr::LowTypeApplication { callee, .. } => self.walk(callee, entry, uses),
            Expr::LowHostCall { args, .. }
            | Expr::LowModuleCall { args, .. }
            | Expr::LowQualifiedModuleCall { args, .. } => {
                uses.owner = true;
                for arg in args {
                    self.walk(arg, entry, uses);
                }
            }
            Expr::LowAbsurdCall { value_arg, .. } => self.walk(value_arg, entry, uses),
            Expr::LowNewtypeCtor { payload, .. }
            | Expr::LowQualifiedNewtypeMember { payload, .. } => {
                uses.owner = true;
                self.walk(payload, entry, uses);
            }
            Expr::LowNewtypeProj { target, .. } => {
                uses.owner = true;
                self.walk(target, entry, uses);
            }
            Expr::LowCpsProjectorApply {
                receiver,
                continuation,
                ..
            } => {
                uses.owner = true;
                self.walk(receiver, entry, uses);
                self.walk(continuation, entry, uses);
            }
            Expr::EnrichedTuple { items, .. } => {
                for item in items {
                    self.walk(item, entry, uses);
                }
            }
            Expr::EnrichedInject { payload, .. } => self.walk(payload, entry, uses),
            Expr::EnrichedMatch {
                scrutinee, arms, ..
            } => {
                self.walk(scrutinee, entry, uses);
                for (index, arm) in arms.iter().enumerate() {
                    let (binding, previous) =
                        self.bind(&arm.param, None, BindingOrigin::Local { entry });
                    self.plan
                        .arms
                        .insert((expr as *const Expr<Routed> as usize, index), binding);
                    self.plan.definitions.insert(
                        binding,
                        BindingDefinition::Arm {
                            source: expr,
                            index,
                        },
                    );
                    self.walk(&arm.body, entry, uses);
                    self.restore(&arm.param, previous);
                }
            }
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.walk(cond, entry, uses);
                self.walk(then_branch, entry, uses);
                self.walk(else_branch, entry, uses);
            }
            Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
                self.walk(target, entry, uses);
            }
            Expr::LowHostFnValueRef { .. }
            | Expr::LowModuleFnValueRef { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => uses.owner = true,
            Expr::Unit { .. } => {}
            Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Path { ext, .. } | Expr::Call { ext, .. } => match *ext {},
            Expr::Tuple { ext, .. } => match *ext {},
            Expr::FnPlaceholder { ext, .. } => match *ext {},
            Expr::LabelValue { ext, .. } => match *ext {},
            Expr::RowLet { ext, .. } => match *ext {},
            Expr::Elaborator { ext, .. } | Expr::UserElaborator { ext, .. } => match *ext {},
            Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
            Expr::Ufcs { ext, .. } => match *ext {},
            Expr::OpChain { ext, .. } => match *ext {},
            Expr::RecCall { ext, .. } => match *ext {},
        }
    }
}

#[cfg(test)]
mod tests;
