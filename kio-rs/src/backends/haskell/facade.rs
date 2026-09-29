//! Native Haskell realization of the shared boundary-facade plan.
//!
//! This module is intentionally a renderer, not another planner.  It projects
//! the package-complete [`PreparedBoundaryCallableSites`] transaction into the
//! Haskell backend's exact native types, stable boundary identities, and live
//! source-parameter adapters.  Public callable stages, positional slots,
//! structural keys, and direct-Unit versus retained-Unit distinctions all
//! come from the shared plan.

use std::collections::BTreeMap;

use crate::ast::{Meta, Routed, Type, TypeParam};
use crate::backends::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadeExecutionPlan, BoundaryFacadeExecutionUse,
    BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, BoundaryNewtypePayloadPlan,
    BoundaryNewtypeSurface, BoundaryNominalDeclaration, BoundaryNominalDependencies,
    CallableExecutionLayout, CallableExecutionStage, CallableValueStageLayout, FacadeBinderId,
    FacadeKind, FacadeUse, FacadeUseId, PreparedBoundaryCallableSite,
    PreparedBoundaryCallableSites, QualifiedTypeName, SemanticKey,
};

use super::naming::{BoundaryId, BoundaryRoot, BoundaryStep, ItemId, ModuleId, StructuralKey};
use super::skin::HaskellShapes;

/// One semantic declaration-head stage rendered for Haskell.
#[derive(Clone, Debug)]
pub(crate) enum HaskellCallableHeadStage {
    Type(TypeParam),
    Value {
        slots: Vec<HaskellFacadeSlot>,
        execution: Option<CallableValueStageLayout>,
    },
}

/// One exact public slot with its stable Haskell ABI identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HaskellFacadeSlot {
    boundary: BoundaryId,
    ty: Type<Routed>,
}

/// One exact prepared structural boundary and its nominal-cut-preserving slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HaskellStructuralOccurrence {
    kind: FacadeKind,
    slots: Vec<HaskellFacadeSlot>,
    keys: Vec<StructuralKey>,
}

impl HaskellStructuralOccurrence {
    pub(crate) fn kind(&self) -> FacadeKind {
        self.kind
    }

    pub(crate) fn slots(&self) -> &[HaskellFacadeSlot] {
        &self.slots
    }

    pub(crate) fn keys(&self) -> &[StructuralKey] {
        &self.keys
    }
}

impl HaskellFacadeSlot {
    pub(crate) fn boundary(&self) -> &BoundaryId {
        &self.boundary
    }

    pub(crate) fn ty(&self) -> &Type<Routed> {
        &self.ty
    }
}

/// Declaration-head cut plus its post-cut result.
#[derive(Clone, Debug)]
pub(crate) struct HaskellCallableEntry {
    stages: Vec<HaskellCallableHeadStage>,
    returned: HaskellFacadeSlot,
    live: bool,
}

impl HaskellCallableEntry {
    pub(crate) fn stages(&self) -> &[HaskellCallableHeadStage] {
        &self.stages
    }

    pub(crate) fn returned(&self) -> &HaskellFacadeSlot {
        &self.returned
    }

    pub(crate) fn is_live(&self) -> bool {
        self.live
    }
}

/// One exact prepared callable site.
#[derive(Clone, Debug)]
pub(crate) struct HaskellFacadeSite {
    id: BoundaryFacadeSiteId,
    entry: HaskellCallableEntry,
}

/// Haskell's exact realization decision for one prepared public newtype.
///
/// The shared inventory, rather than a second source walk, determines which
/// declarations participate in the host contract.  Native body metadata may
/// still inspect the selected declaration by exact identity after this cut.
#[derive(Clone, Debug)]
pub(crate) struct HaskellPublicNewtype {
    surface: BoundaryNewtypeSurface,
    has_type_params: bool,
    has_existential_params: bool,
}

impl HaskellPublicNewtype {
    pub(crate) fn requires_nominal_carrier(&self) -> bool {
        self.has_type_params || self.has_existential_params || self.surface.uses_nominal_carrier()
    }
}

impl HaskellFacadeSite {
    pub(crate) fn id(&self) -> &BoundaryFacadeSiteId {
        &self.id
    }

    pub(crate) fn entry(&self) -> &HaskellCallableEntry {
        &self.entry
    }
}

/// Package-complete native projection of the shared callable catalog.
#[derive(Clone, Debug)]
pub(crate) struct HaskellFacadeCatalog {
    sites: BTreeMap<BoundaryFacadeSiteId, HaskellFacadeSite>,
    public_newtypes: BTreeMap<QualifiedTypeName, HaskellPublicNewtype>,
    boundary_types: BTreeMap<BoundaryId, Type<Routed>>,
    function_layouts: BTreeMap<BoundaryId, CallableValueStageLayout>,
    structural_occurrences: BTreeMap<BoundaryId, HaskellStructuralOccurrence>,
}

impl HaskellFacadeCatalog {
    pub(crate) fn new(
        prepared: &PreparedBoundaryCallableSites,
        shapes: &HaskellShapes<'_>,
    ) -> Self {
        let mut catalog = Self {
            sites: BTreeMap::new(),
            public_newtypes: prepared
                .public_newtypes()
                .map(|newtype| {
                    (
                        newtype.name().clone(),
                        HaskellPublicNewtype {
                            surface: newtype.surface().clone(),
                            has_type_params: !newtype.type_params().is_empty(),
                            has_existential_params: !newtype.existential_params().is_empty(),
                        },
                    )
                })
                .collect(),
            boundary_types: BTreeMap::new(),
            function_layouts: BTreeMap::new(),
            structural_occurrences: BTreeMap::new(),
        };
        for site in prepared.sites() {
            let realized = catalog.realize_site(site, shapes);
            let replaced = catalog.sites.insert(realized.id.clone(), realized);
            assert!(
                replaced.is_none(),
                "the prepared Haskell facade has one realization per exact site"
            );
        }
        catalog
    }

    pub(crate) fn sites(
        &self,
    ) -> impl DoubleEndedIterator<Item = &HaskellFacadeSite> + ExactSizeIterator {
        self.sites.values()
    }

    pub(crate) fn public_newtypes(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&QualifiedTypeName, &HaskellPublicNewtype)> + ExactSizeIterator
    {
        self.public_newtypes.iter()
    }

    pub(crate) fn public_newtype(&self, module: &str, name: &str) -> Option<&HaskellPublicNewtype> {
        let name = QualifiedTypeName::new(module_segments(module), name)?;
        self.public_newtypes.get(&name)
    }

    pub(crate) fn host_site(&self, module: &str, name: &str) -> &HaskellFacadeSite {
        self.site(
            module,
            BoundaryFacadeSiteOwner::HostFunction {
                name: name.to_owned(),
            },
        )
    }

    pub(crate) fn exported_fn_site(&self, module: &str, name: &str) -> &HaskellFacadeSite {
        self.site(
            module,
            BoundaryFacadeSiteOwner::ExportedFunction {
                name: name.to_owned(),
            },
        )
    }

    pub(crate) fn exact_site(&self, id: &BoundaryFacadeSiteId) -> &HaskellFacadeSite {
        self.sites
            .get(id)
            .unwrap_or_else(|| panic!("missing prepared Haskell facade site {id:?}"))
    }

    pub(crate) fn newtype_constructor_site(
        &self,
        module: &str,
        newtype: &str,
        member: &str,
    ) -> &HaskellFacadeSite {
        self.site(
            module,
            BoundaryFacadeSiteOwner::NewtypeConstructor {
                newtype: newtype.to_owned(),
                member: member.to_owned(),
            },
        )
    }

    pub(crate) fn newtype_projector_site(
        &self,
        module: &str,
        newtype: &str,
        member: &str,
    ) -> &HaskellFacadeSite {
        self.site(
            module,
            BoundaryFacadeSiteOwner::NewtypeProjector {
                newtype: newtype.to_owned(),
                member: member.to_owned(),
            },
        )
    }

    pub(crate) fn function_layout(
        &self,
        boundary: &BoundaryId,
    ) -> Option<&CallableValueStageLayout> {
        self.function_layouts.get(boundary)
    }

    pub(crate) fn boundary_type(&self, boundary: &BoundaryId) -> Option<&Type<Routed>> {
        self.boundary_types.get(boundary)
    }

    pub(crate) fn structural_occurrence(
        &self,
        boundary: &BoundaryId,
        expected: FacadeKind,
    ) -> Option<&HaskellStructuralOccurrence> {
        self.structural_occurrences
            .get(boundary)
            .and_then(|occurrence| (occurrence.kind == expected).then_some(occurrence))
    }

    fn site(&self, module: &str, owner: BoundaryFacadeSiteOwner) -> &HaskellFacadeSite {
        let id = BoundaryFacadeSiteId::new(module_segments(module), owner)
            .expect("a bridged Haskell site has a nonempty exact module identity");
        self.sites
            .get(&id)
            .unwrap_or_else(|| panic!("missing prepared Haskell facade site {id:?}"))
    }

    fn realize_site(
        &mut self,
        prepared: PreparedBoundaryCallableSite<'_>,
        shapes: &HaskellShapes<'_>,
    ) -> HaskellFacadeSite {
        let id = prepared.site().clone();
        let callable = prepared.plan();
        let semantic = callable.entry();
        let execution = prepared.execution();
        let mut execution_stages = execution.map(|layout| layout.head_stages().iter());
        let mut scope = BTreeMap::<FacadeBinderId, Type<Routed>>::new();
        let mut stages = Vec::with_capacity(semantic.head_stages.len());
        let mut argument_index = 0u32;

        for stage in semantic.head_stages {
            let paired = execution_stages.as_mut().and_then(Iterator::next);
            match stage {
                BoundaryCallableHeadStage::Type {
                    id: binder_id,
                    binder,
                } => {
                    if let Some(CallableExecutionStage::Type { .. }) = paired {
                    } else if execution.is_some() {
                        unreachable!("a live Haskell type stage lost its paired execution action")
                    }
                    let param = TypeParam {
                        name: binder.name.clone(),
                        span: binder.span,
                        kind: Some(binder.kind.clone()),
                    };
                    scope.insert(
                        binder_id,
                        Type::synth_path(vec![binder.name.clone()], Vec::new(), binder.span),
                    );
                    stages.push(HaskellCallableHeadStage::Type(param));
                }
                BoundaryCallableHeadStage::Value { slots } => {
                    let paired_layout = match paired {
                        Some(CallableExecutionStage::Value(layout)) => Some(layout.clone()),
                        None if execution.is_none() => None,
                        _ => unreachable!(
                            "a live Haskell value stage lost its paired execution layout"
                        ),
                    };
                    let mut realized_slots = Vec::with_capacity(slots.len());
                    for use_id in slots {
                        let boundary =
                            boundary_for(prepared.site(), BoundaryRoot::Arg(argument_index));
                        argument_index = argument_index
                            .checked_add(1)
                            .expect("a Haskell facade argument index fits in u32");
                        let ty = self.realize_boundary_use(
                            prepared,
                            callable.facade(),
                            execution.map(CallableExecutionLayout::root_uses),
                            *use_id,
                            &boundary,
                            &scope,
                            shapes,
                        );
                        realized_slots.push(HaskellFacadeSlot { boundary, ty });
                    }
                    if let Some(layout) = &paired_layout {
                        assert_eq!(
                            layout.facade_slot_count(),
                            realized_slots.len(),
                            "the Haskell value stage renders every shared facade slot"
                        );
                    }
                    stages.push(HaskellCallableHeadStage::Value {
                        slots: realized_slots,
                        execution: paired_layout,
                    });
                }
            }
        }
        if let Some(mut remaining) = execution_stages {
            assert!(
                remaining.next().is_none(),
                "the Haskell callable consumed every paired head stage"
            );
        }

        let returned_boundary = boundary_for(prepared.site(), BoundaryRoot::Ret);
        let returned_ty = self.realize_boundary_use(
            prepared,
            callable.facade(),
            execution.map(CallableExecutionLayout::root_uses),
            semantic.returned,
            &returned_boundary,
            &scope,
            shapes,
        );
        HaskellFacadeSite {
            id,
            entry: HaskellCallableEntry {
                stages,
                returned: HaskellFacadeSlot {
                    boundary: returned_boundary,
                    ty: returned_ty,
                },
                live: execution.is_some(),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn realize_boundary_use(
        &mut self,
        site: PreparedBoundaryCallableSite<'_>,
        plan: &BoundaryFacadePlan,
        execution: Option<&BoundaryFacadeExecutionPlan>,
        use_id: FacadeUseId,
        boundary: &BoundaryId,
        substitutions: &BTreeMap<FacadeBinderId, Type<Routed>>,
        shapes: &HaskellShapes<'_>,
    ) -> Type<Routed> {
        let ty = self.materialize_use(site, plan, use_id, substitutions, shapes);
        claim_same(&mut self.boundary_types, boundary.clone(), ty.clone());
        self.register_nested_uses(
            site,
            plan,
            execution,
            use_id,
            boundary,
            substitutions,
            shapes,
        );
        ty
    }

    #[allow(clippy::too_many_arguments)]
    fn register_nested_uses(
        &mut self,
        site: PreparedBoundaryCallableSite<'_>,
        plan: &BoundaryFacadePlan,
        execution: Option<&BoundaryFacadeExecutionPlan>,
        use_id: FacadeUseId,
        boundary: &BoundaryId,
        substitutions: &BTreeMap<FacadeBinderId, Type<Routed>>,
        shapes: &HaskellShapes<'_>,
    ) {
        match plan.use_at(use_id) {
            FacadeUse::Apply {
                constructor, args, ..
            } => {
                if self.register_transparent_application(
                    site,
                    plan,
                    execution,
                    *constructor,
                    args,
                    boundary,
                    substitutions,
                    shapes,
                ) {
                    return;
                }
                for (index, arg) in args.iter().enumerate() {
                    let nested = boundary.nested(BoundaryStep::App(
                        index.try_into().expect("type application index fits u32"),
                    ));
                    self.realize_boundary_use(
                        site,
                        plan,
                        execution,
                        *arg,
                        &nested,
                        substitutions,
                        shapes,
                    );
                }
            }
            FacadeUse::Nominal { name, .. } => {
                self.register_transparent_nominal(site, name, &[], boundary, substitutions, shapes);
            }
            FacadeUse::Product { shell, args, .. } | FacadeUse::Sum { shell, args, .. } => {
                let keys = shell
                    .ordered_keys()
                    .iter()
                    .map(structural_key)
                    .collect::<Vec<_>>();
                let mut slots = Vec::with_capacity(args.len());
                for (index, arg) in args.iter().enumerate() {
                    let nested = boundary.nested(BoundaryStep::Slot(
                        index.try_into().expect("structural slot index fits u32"),
                    ));
                    let ty = self.realize_boundary_use(
                        site,
                        plan,
                        execution,
                        *arg,
                        &nested,
                        substitutions,
                        shapes,
                    );
                    slots.push(HaskellFacadeSlot {
                        boundary: nested,
                        ty,
                    });
                }
                claim_same(
                    &mut self.structural_occurrences,
                    boundary.clone(),
                    HaskellStructuralOccurrence {
                        kind: shell.kind(),
                        slots,
                        keys,
                    },
                );
            }
            FacadeUse::Function { slots, result, .. } => {
                if let Some(execution) = execution {
                    let BoundaryFacadeExecutionUse::Function(layout) = execution.use_at(use_id)
                    else {
                        unreachable!("a live semantic Function owns one paired layout")
                    };
                    claim_same(&mut self.function_layouts, boundary.clone(), layout.clone());
                }
                for (index, slot) in slots.iter().enumerate() {
                    let nested = boundary.nested(BoundaryStep::CallbackArg(
                        index.try_into().expect("callback argument index fits u32"),
                    ));
                    self.realize_boundary_use(
                        site,
                        plan,
                        execution,
                        *slot,
                        &nested,
                        substitutions,
                        shapes,
                    );
                }
                let nested = boundary.nested(BoundaryStep::CallbackRet);
                self.realize_boundary_use(
                    site,
                    plan,
                    execution,
                    *result,
                    &nested,
                    substitutions,
                    shapes,
                );
            }
            FacadeUse::Forall { binder, result, .. } => {
                let mut nested_substitutions = substitutions.clone();
                let binder = plan.binder(*binder);
                nested_substitutions.insert(
                    match plan.use_at(use_id) {
                        FacadeUse::Forall { binder, .. } => *binder,
                        _ => unreachable!(),
                    },
                    Type::synth_path(vec![binder.name.clone()], Vec::new(), binder.span),
                );
                // A `forall` is an application stage at the same boundary,
                // not a second boundary value. Keep the outer polymorphic
                // type as the identity's sole claimed type while registering
                // the result's nested callable and structural uses.
                self.register_nested_uses(
                    site,
                    plan,
                    execution,
                    *result,
                    boundary,
                    &nested_substitutions,
                    shapes,
                );
            }
            FacadeUse::Unit { .. } | FacadeUse::Bottom { .. } | FacadeUse::Bound { .. } => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn register_transparent_application(
        &mut self,
        site: PreparedBoundaryCallableSite<'_>,
        plan: &BoundaryFacadePlan,
        _execution: Option<&BoundaryFacadeExecutionPlan>,
        constructor: FacadeUseId,
        args: &[FacadeUseId],
        boundary: &BoundaryId,
        substitutions: &BTreeMap<FacadeBinderId, Type<Routed>>,
        shapes: &HaskellShapes<'_>,
    ) -> bool {
        let FacadeUse::Nominal { name, .. } = plan.use_at(constructor) else {
            return false;
        };
        let rendered_args = args
            .iter()
            .map(|arg| self.materialize_use(site, plan, *arg, substitutions, shapes))
            .collect::<Vec<_>>();
        self.register_transparent_nominal(
            site,
            name,
            &rendered_args,
            boundary,
            substitutions,
            shapes,
        )
    }

    fn register_transparent_nominal(
        &mut self,
        site: PreparedBoundaryCallableSite<'_>,
        name: &QualifiedTypeName,
        arguments: &[Type<Routed>],
        boundary: &BoundaryId,
        _substitutions: &BTreeMap<FacadeBinderId, Type<Routed>>,
        shapes: &HaskellShapes<'_>,
    ) -> bool {
        if shapes
            .newtype_has_nominal_boundary_carrier(&name.module_segments().join("/"), name.name())
        {
            return false;
        }
        let Some(BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        }) = site.nominals().declaration(name)
        else {
            return false;
        };
        let Some(execution) = site
            .execution()
            .and_then(|execution| execution.transparent_payload(name))
        else {
            return false;
        };
        let substitutions = payload_substitutions(payload, arguments);
        self.realize_boundary_use(
            site,
            payload.facade(),
            Some(execution),
            payload.payload_root(),
            boundary,
            &substitutions,
            shapes,
        );
        true
    }

    fn materialize_use(
        &self,
        site: PreparedBoundaryCallableSite<'_>,
        plan: &BoundaryFacadePlan,
        use_id: FacadeUseId,
        substitutions: &BTreeMap<FacadeBinderId, Type<Routed>>,
        shapes: &HaskellShapes<'_>,
    ) -> Type<Routed> {
        match plan.use_at(use_id) {
            FacadeUse::Unit { span } => Type::Unit {
                meta: Meta::new(*span),
            },
            FacadeUse::Bottom { span } => Type::Bottom {
                meta: Meta::new(*span),
            },
            FacadeUse::Bound { binder, span } => {
                substitutions.get(binder).cloned().unwrap_or_else(|| {
                    Type::synth_path(vec![plan.binder(*binder).name.clone()], Vec::new(), *span)
                })
            }
            FacadeUse::Nominal { name, span } => {
                if let Some(payload) = transparent_payload(site.nominals(), name, shapes) {
                    let substitutions = payload_substitutions(payload, &[]);
                    return self.materialize_use(
                        site,
                        payload.facade(),
                        payload.payload_root(),
                        &substitutions,
                        shapes,
                    );
                }
                Type::synth_path(
                    name.module_segments()
                        .iter()
                        .cloned()
                        .chain(std::iter::once(name.name().to_owned()))
                        .collect(),
                    Vec::new(),
                    *span,
                )
            }
            FacadeUse::Apply {
                constructor,
                args,
                span: _,
            } => {
                let rendered_args = args
                    .iter()
                    .map(|arg| self.materialize_use(site, plan, *arg, substitutions, shapes))
                    .collect::<Vec<_>>();
                if let FacadeUse::Nominal { name, .. } = plan.use_at(*constructor)
                    && let Some(payload) = transparent_payload(site.nominals(), name, shapes)
                {
                    let substitutions = payload_substitutions(payload, &rendered_args);
                    return self.materialize_use(
                        site,
                        payload.facade(),
                        payload.payload_root(),
                        &substitutions,
                        shapes,
                    );
                }
                let mut head =
                    self.materialize_use(site, plan, *constructor, substitutions, shapes);
                let Type::Path {
                    args: head_args, ..
                } = &mut head
                else {
                    unreachable!("a prepared Haskell type application has a path head")
                };
                head_args.extend(rendered_args);
                head
            }
            FacadeUse::Product { args, span, .. } => Type::right_fold_args(
                args.iter()
                    .map(|arg| self.materialize_use(site, plan, *arg, substitutions, shapes))
                    .collect(),
                *span,
            ),
            FacadeUse::Sum { args, span, .. } => right_fold_sum(
                args.iter()
                    .map(|arg| self.materialize_use(site, plan, *arg, substitutions, shapes))
                    .collect(),
                *span,
            ),
            FacadeUse::Function {
                slots,
                result,
                caps,
                span,
                ..
            } => Type::Function {
                param: Box::new(Type::right_fold_args(
                    slots
                        .iter()
                        .map(|slot| self.materialize_use(site, plan, *slot, substitutions, shapes))
                        .collect(),
                    *span,
                )),
                ret: Box::new(self.materialize_use(site, plan, *result, substitutions, shapes)),
                meta: Meta::new(*span),
                abi_arity: slots.len(),
                caps: caps.clone(),
            },
            FacadeUse::Forall {
                binder,
                result,
                span,
            } => {
                let binder_meta = plan.binder(*binder);
                let param = TypeParam {
                    name: binder_meta.name.clone(),
                    span: binder_meta.span,
                    kind: Some(binder_meta.kind.clone()),
                };
                let mut nested = substitutions.clone();
                nested.insert(
                    *binder,
                    Type::synth_path(vec![param.name.clone()], Vec::new(), param.span),
                );
                Type::Forall {
                    param,
                    body: Box::new(self.materialize_use(site, plan, *result, &nested, shapes)),
                    meta: Meta::new(*span),
                }
            }
        }
    }
}

fn transparent_payload<'a>(
    nominals: &'a BoundaryNominalDependencies,
    name: &QualifiedTypeName,
    shapes: &HaskellShapes<'_>,
) -> Option<&'a BoundaryNewtypePayloadPlan> {
    if shapes.newtype_has_nominal_boundary_carrier(&name.module_segments().join("/"), name.name()) {
        return None;
    }
    match nominals.declaration(name) {
        Some(BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        }) => Some(payload),
        _ => None,
    }
}

fn payload_substitutions(
    payload: &BoundaryNewtypePayloadPlan,
    arguments: &[Type<Routed>],
) -> BTreeMap<FacadeBinderId, Type<Routed>> {
    assert!(
        arguments.len() <= payload.declaration_binders().len(),
        "a transparent Haskell nominal has no more arguments than declaration binders"
    );
    payload
        .declaration_binders()
        .iter()
        .copied()
        .zip(arguments.iter().cloned())
        .collect()
}

fn boundary_for(site: &BoundaryFacadeSiteId, root: BoundaryRoot) -> BoundaryId {
    let module = site.module_segments().join("/");
    match site.owner() {
        BoundaryFacadeSiteOwner::HostFunction { name } => BoundaryId::env(&module, name, root),
        BoundaryFacadeSiteOwner::ExportedFunction { name } => {
            BoundaryId::exp(ItemId::item(&module, name), root)
        }
        BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member }
        | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
            BoundaryId::exp(ItemId::newtype_member(&module, newtype, member), root)
        }
    }
}

fn module_segments(module: &str) -> Vec<String> {
    module.split('/').map(str::to_owned).collect()
}

fn structural_key(key: &SemanticKey) -> StructuralKey {
    match key {
        SemanticKey::Bare { name } => StructuralKey::BareNewtype(name.clone()),
        SemanticKey::Qualified {
            module_segments,
            name,
        } => StructuralKey::QualifiedNewtype {
            module: ModuleId::from_path(&module_segments.join("/")),
            newtype: name.clone(),
        },
        SemanticKey::Positional { index } => StructuralKey::Positional(
            (*index)
                .try_into()
                .expect("a shared facade key index fits Haskell's u32 ABI codec"),
        ),
    }
}

fn right_fold_sum(mut slots: Vec<Type<Routed>>, span: crate::span::Span) -> Type<Routed> {
    assert!(!slots.is_empty(), "a prepared sum has at least one arm");
    if slots.len() == 1 {
        return slots.pop().expect("one sum arm");
    }
    let mut acc = slots.pop().expect("a nonempty sum has a final arm");
    while let Some(left) = slots.pop() {
        acc = Type::Sum {
            left: Box::new(left),
            right: Box::new(acc),
            meta: Meta::new(span),
        };
    }
    acc
}

fn claim_same<K, V>(map: &mut BTreeMap<K, V>, key: K, value: V)
where
    K: Ord + std::fmt::Debug,
    V: Eq + std::fmt::Debug,
{
    match map.entry(key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(value);
        }
        std::collections::btree_map::Entry::Occupied(entry) => {
            assert_eq!(
                entry.get(),
                &value,
                "one Haskell facade identity rendered inconsistently"
            );
        }
    }
}
