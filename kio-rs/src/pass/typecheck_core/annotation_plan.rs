//! Pure, transient planning state for partial source annotations.
//!
//! This module owns no solver or phase authority. Planning borrows the active
//! lexical stack until a plan must move; that one caller-owned boundary
//! promotes the exact binder prefix into the same persistent name map used by
//! the GoalStore rigid scope. Goal owners and retained binder proofs are
//! minted only by the later committed materialization boundary.

#[cfg(feature = "surface")]
use std::collections::{HashMap, HashSet};

#[cfg(feature = "surface")]
use crate::ast::Type;
use crate::ast::{Kind, Phase};
#[cfg(feature = "surface")]
use crate::ast::{Lowered, TypeGoalOwner, TypeGoalRef};
use crate::error::Error;
#[cfg(feature = "surface")]
use crate::pass::visit_mut;
use crate::span::Span;

use super::InternedType;
use super::TyperPhase;
use super::aliases::{
    AliasBinderLookup, AliasCtx, AliasSourceMaterialized, AliasSourceOccurrenceId,
    AliasSourceOccurrenceTransport, materialize_aliases_with_source_transport_and_binder_lookup,
};
#[cfg(all(test, feature = "surface"))]
use super::aliases::{AliasBinderOrigin, AliasSourceDisposition, AliasTransportContribution};
use super::env::{Local, TypeBinderId, TypeCtx};
#[cfg(feature = "surface")]
use super::goals::{
    ClosedGoalOutput, ExpectedEquationOperand, GoalEscape, GoalOrigin, GoalOwnerKind,
    GoalSolutionPolicy, GoalStore, ScopedType,
};
use super::persistent_exact::PersistentExactNameMap;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AnnotationPlanWork {
    pub(super) classifications: usize,
    pub(super) transport_materializations: usize,
    pub(super) rejections: usize,
    pub(super) committed_materializations: usize,
    pub(super) goals_allocated: usize,
    pub(super) binder_events_published: usize,
}

#[cfg(test)]
thread_local! {
    static ANNOTATION_PLAN_WORK: std::cell::Cell<AnnotationPlanWork> =
        const { std::cell::Cell::new(AnnotationPlanWork {
            classifications: 0,
            transport_materializations: 0,
            rejections: 0,
            committed_materializations: 0,
            goals_allocated: 0,
            binder_events_published: 0,
        }) };
}

#[cfg(test)]
fn update_annotation_plan_work(update: impl FnOnce(&mut AnnotationPlanWork)) {
    ANNOTATION_PLAN_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

#[cfg(all(test, feature = "surface"))]
pub(super) fn reset_annotation_plan_work() {
    ANNOTATION_PLAN_WORK.with(|work| work.set(AnnotationPlanWork::default()));
}

#[cfg(all(test, feature = "surface"))]
pub(super) fn annotation_plan_work() -> AnnotationPlanWork {
    ANNOTATION_PLAN_WORK.with(std::cell::Cell::get)
}

/// Candidate-only firing evidence for the phase/policy seams that consume the
/// one shared annotation/header plan. This state exists only in crate tests;
/// production planning remains representation- and probe-free.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AnnotationPlanConsumerWork {
    pub(crate) lowered_check: usize,
    pub(crate) lowered_synth: usize,
    pub(crate) ordinary_retained: usize,
    pub(crate) expected_lambda: usize,
    pub(crate) prime_immediate: usize,
    pub(crate) prime_check: usize,
    pub(crate) prime_synth: usize,
}

#[cfg(test)]
thread_local! {
    static ANNOTATION_PLAN_CONSUMER_WORK: std::cell::Cell<AnnotationPlanConsumerWork> =
        const { std::cell::Cell::new(AnnotationPlanConsumerWork {
            lowered_check: 0,
            lowered_synth: 0,
            ordinary_retained: 0,
            expected_lambda: 0,
            prime_immediate: 0,
            prime_check: 0,
            prime_synth: 0,
        }) };
}

#[cfg(test)]
fn update_annotation_plan_consumer_work(update: impl FnOnce(&mut AnnotationPlanConsumerWork)) {
    ANNOTATION_PLAN_CONSUMER_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

#[cfg(test)]
pub(crate) fn reset_annotation_plan_consumer_work() {
    ANNOTATION_PLAN_CONSUMER_WORK.with(|work| work.set(AnnotationPlanConsumerWork::default()));
}

#[cfg(test)]
pub(crate) fn annotation_plan_consumer_work() -> AnnotationPlanConsumerWork {
    ANNOTATION_PLAN_CONSUMER_WORK.with(std::cell::Cell::get)
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn record_lowered_check_consumer() {
    update_annotation_plan_consumer_work(|work| work.lowered_check += 1);
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn record_lowered_synth_consumer() {
    update_annotation_plan_consumer_work(|work| work.lowered_synth += 1);
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn record_ordinary_retained_consumer() {
    update_annotation_plan_consumer_work(|work| work.ordinary_retained += 1);
}

#[cfg(test)]
pub(crate) fn record_expected_lambda_consumer() {
    update_annotation_plan_consumer_work(|work| work.expected_lambda += 1);
}

#[cfg(test)]
pub(crate) fn record_prime_immediate_consumer() {
    update_annotation_plan_consumer_work(|work| work.prime_immediate += 1);
}

#[cfg(test)]
pub(crate) fn record_prime_check_consumer() {
    update_annotation_plan_consumer_work(|work| work.prime_check += 1);
}

#[cfg(test)]
pub(crate) fn record_prime_synth_consumer() {
    update_annotation_plan_consumer_work(|work| work.prime_synth += 1);
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AnnotationBinderProof {
    Ambient(TypeBinderId),
    Source { declaration_span: Span },
    Embedded { declaration_span: Span },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlanningLexicalBinding {
    kind: Kind,
    proof: AnnotationBinderProof,
}

impl PlanningLexicalBinding {
    fn proof(&self) -> &AnnotationBinderProof {
        &self.proof
    }
}

enum PlanningLexicalStorage<'a, 'm, P>
where
    P: Phase,
{
    Borrowed(&'a [Local<'m, P>]),
    Promoted(PromotedPlanningLexicalView),
}

/// Exact ambient lexical evidence for one caller-owned planning batch.
///
/// A streaming plan remains a slice borrow and allocates nothing. Before a
/// plan is retained, declined, suspended, or otherwise moved beyond that
/// borrow, the caller promotes this value once and clones the resulting
/// persistent root for sibling plans.
pub(crate) struct PlanningLexicalView<'a, 'm, P>
where
    P: Phase,
{
    storage: PlanningLexicalStorage<'a, 'm, P>,
    source_binders: &'a [crate::ast::SignatureParam<P>],
    source_binder_end: usize,
}

#[derive(Clone, Default)]
pub(crate) struct PromotedPlanningLexicalView {
    bindings: PersistentExactNameMap<PlanningLexicalBinding>,
}

impl<'a, 'm, P> PlanningLexicalView<'a, 'm, P>
where
    P: TyperPhase,
{
    pub(crate) fn borrowed(tcx: &'a TypeCtx<'m, '_, P>) -> Self {
        Self {
            storage: PlanningLexicalStorage::Borrowed(&tcx.locals),
            source_binders: &[],
            source_binder_end: 0,
        }
    }

    pub(crate) fn with_signature_binders(
        tcx: &'a TypeCtx<'m, '_, P>,
        signature: &'a crate::ast::Signature<P>,
    ) -> Self {
        Self {
            storage: PlanningLexicalStorage::Borrowed(&tcx.locals),
            source_binders: &signature.params,
            source_binder_end: 0,
        }
    }

    #[cfg(feature = "surface")]
    pub(crate) fn with_promoted_signature_binders(
        ambient: PromotedPlanningLexicalView,
        signature: &'a crate::ast::Signature<P>,
    ) -> Self {
        Self {
            storage: PlanningLexicalStorage::Promoted(ambient),
            source_binders: &signature.params,
            source_binder_end: 0,
        }
    }

    pub(crate) fn advance_signature_prefix(&mut self, end: usize) {
        assert!(
            self.source_binder_end <= end && end <= self.source_binders.len(),
            "a planning signature prefix must advance monotonically"
        );
        if let PlanningLexicalStorage::Promoted(root) = &mut self.storage {
            for param in &self.source_binders[self.source_binder_end..end] {
                let crate::ast::SignatureParam::Type(param) = param else {
                    continue;
                };
                #[cfg(test)]
                update_planning_lexical_work(|work| work.bindings_scanned += 1);
                root.bindings = std::mem::take(&mut root.bindings).push(
                    param.name.clone(),
                    PlanningLexicalBinding {
                        kind: param.effective_kind(),
                        proof: AnnotationBinderProof::Source {
                            declaration_span: param.span,
                        },
                    },
                    |left, right| left == right,
                );
            }
        }
        self.source_binder_end = end;
    }

    fn active_source_binders(&self) -> &[crate::ast::SignatureParam<P>] {
        &self.source_binders[..self.source_binder_end]
    }

    pub(crate) fn lookup(&self, name: &str) -> Option<PlanningLexicalBinding> {
        match &self.storage {
            PlanningLexicalStorage::Borrowed(locals) => {
                #[cfg(test)]
                update_planning_lexical_work(|work| work.lookups += 1);
                if let Some(binding) = self.active_source_binders().iter().rev().find_map(|param| {
                    let crate::ast::SignatureParam::Type(param) = param else {
                        return None;
                    };
                    (param.name == name).then(|| PlanningLexicalBinding {
                        kind: param.effective_kind(),
                        proof: AnnotationBinderProof::Source {
                            declaration_span: param.span,
                        },
                    })
                }) {
                    return Some(binding);
                }
                locals.iter().rev().find_map(|local| match local {
                    Local::TypeParam {
                        name: bound,
                        kind,
                        binder_id,
                        ..
                    } if bound == name => Some(PlanningLexicalBinding {
                        kind: kind.clone(),
                        proof: AnnotationBinderProof::Ambient(*binder_id),
                    }),
                    _ => None,
                })
            }
            PlanningLexicalStorage::Promoted(root) => root.lookup(name).cloned(),
        }
    }

    pub(crate) fn promote(&mut self) -> &PromotedPlanningLexicalView {
        if let PlanningLexicalStorage::Borrowed(locals) = &self.storage {
            #[cfg(test)]
            update_planning_lexical_work(|work| work.promotions += 1);
            let mut bindings = PersistentExactNameMap::default();
            for local in *locals {
                let Local::TypeParam {
                    name,
                    kind,
                    binder_id,
                    ..
                } = local
                else {
                    continue;
                };
                #[cfg(test)]
                update_planning_lexical_work(|work| work.bindings_scanned += 1);
                bindings = bindings.push(
                    name.to_string(),
                    PlanningLexicalBinding {
                        kind: kind.clone(),
                        proof: AnnotationBinderProof::Ambient(*binder_id),
                    },
                    |left, right| left == right,
                );
            }
            for param in self.active_source_binders() {
                let crate::ast::SignatureParam::Type(param) = param else {
                    continue;
                };
                bindings = bindings.push(
                    param.name.clone(),
                    PlanningLexicalBinding {
                        kind: param.effective_kind(),
                        proof: AnnotationBinderProof::Source {
                            declaration_span: param.span,
                        },
                    },
                    |left, right| left == right,
                );
            }
            #[cfg(test)]
            if !bindings.is_empty() {
                update_planning_lexical_work(|work| work.persistent_roots_built += 1);
            }
            self.storage =
                PlanningLexicalStorage::Promoted(PromotedPlanningLexicalView { bindings });
            self.source_binders = &[];
            self.source_binder_end = 0;
        }
        let PlanningLexicalStorage::Promoted(root) = &self.storage else {
            unreachable!("a planning lexical view was just promoted")
        };
        root
    }

    pub(crate) fn into_promoted(mut self) -> PromotedPlanningLexicalView {
        self.promote();
        let PlanningLexicalStorage::Promoted(root) = self.storage else {
            unreachable!("a planning lexical view was just promoted")
        };
        root
    }

    #[cfg(test)]
    #[cfg(all(test, feature = "surface"))]
    fn is_promoted(&self) -> bool {
        matches!(self.storage, PlanningLexicalStorage::Promoted(_))
    }
}

impl PromotedPlanningLexicalView {
    pub(crate) fn lookup(&self, name: &str) -> Option<&PlanningLexicalBinding> {
        #[cfg(test)]
        update_planning_lexical_work(|work| work.lookups += 1);
        self.bindings.get(name)
    }
}

impl<P> AliasBinderLookup for PlanningLexicalView<'_, '_, P>
where
    P: TyperPhase,
{
    fn contains_alias_binder(&self, name: &str) -> bool {
        self.lookup(name).is_some()
    }

    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
        match &self.storage {
            PlanningLexicalStorage::Borrowed(locals) => {
                for local in locals.iter().rev() {
                    if let Local::TypeParam { name, .. } = local {
                        visit(name);
                    }
                }
                for param in self.active_source_binders() {
                    if let crate::ast::SignatureParam::Type(param) = param {
                        visit(param.name.as_str());
                    }
                }
            }
            PlanningLexicalStorage::Promoted(root) => root.for_each_alias_binder(visit),
        }
    }
}

impl<P> super::types::KindBinderLookup for PlanningLexicalView<'_, '_, P>
where
    P: TyperPhase,
{
    fn kind_of(&self, name: &str) -> Option<Kind> {
        match &self.storage {
            PlanningLexicalStorage::Borrowed(_) => {
                self.active_source_binders().iter().rev().find_map(|param| {
                    let crate::ast::SignatureParam::Type(param) = param else {
                        return None;
                    };
                    (param.name == name).then(|| param.effective_kind())
                })
            }
            PlanningLexicalStorage::Promoted(root) => {
                root.lookup(name).map(|binding| binding.kind.clone())
            }
        }
    }

    fn for_each(&self, visit: &mut dyn FnMut(&str)) {
        match &self.storage {
            PlanningLexicalStorage::Borrowed(_) => {
                for param in self.active_source_binders() {
                    if let crate::ast::SignatureParam::Type(param) = param {
                        visit(param.name.as_str());
                    }
                }
            }
            PlanningLexicalStorage::Promoted(root) => root.for_each_alias_binder(visit),
        }
    }
}

impl AliasBinderLookup for PromotedPlanningLexicalView {
    fn contains_alias_binder(&self, name: &str) -> bool {
        self.lookup(name).is_some()
    }

    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
        self.bindings.for_each(|name, _| visit(name));
    }
}

/// Run the planning-only transport policy through the one shared alias
/// materializer while borrowing the exact lexical view from this batch.
pub(crate) fn materialize_source_annotation<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    lexical: &dyn AliasBinderLookup,
    identity_canonical: bool,
) -> AliasSourceMaterialized<P>
where
    P: TyperPhase + crate::pass::resolve::ExportContractPhase + Clone,
    'm: 'v,
{
    materialize_aliases_with_source_transport_and_binder_lookup(
        ty,
        ctx,
        lexical,
        identity_canonical,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AnnotationHole {
    pub(crate) source: AliasSourceOccurrenceId,
    pub(crate) span: Span,
    pub(crate) required_kind: Kind,
}

#[derive(Debug, PartialEq, Eq)]
enum AnnotationBinderEvent {
    Enter(crate::ast::TypeParam),
    Exit,
    Reference {
        span: Span,
        name: String,
        proof: AnnotationBinderProof,
    },
}

enum PlannedAnnotationType<'a, P>
where
    P: Phase,
{
    Borrowed(&'a crate::ast::Type<P>),
    Materialized(crate::ast::Type<P>),
}

/// Pure result of validating and classifying one source annotation.
///
/// A closed annotation borrows its original tree and owns no event or alias
/// transport allocation. An open annotation owns exactly one ordinary
/// alias-materialized tree plus the sparse source/output relation needed by
/// mechanical Infer-to-Goal conversion and diagnostics.
pub(crate) struct AnnotationPlan<'a, P>
where
    P: Phase,
{
    ty: PlannedAnnotationType<'a, P>,
    identity_canonical: bool,
    holes: Vec<AnnotationHole>,
    transport: Option<AliasSourceOccurrenceTransport>,
    binder_events: Vec<AnnotationBinderEvent>,
    rejection: Option<Box<Error>>,
}

#[cfg(all(test, feature = "surface"))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct AnnotationPlanObservation {
    closed_borrowed: bool,
    identity_canonical: bool,
    holes: Vec<AnnotationHole>,
    infer_routes: Vec<(Span, AliasSourceDisposition, Option<AliasBinderOrigin>)>,
    contributions: Vec<AliasTransportContribution>,
    root_present: bool,
    emitted_infer_count: usize,
    binder_event_count: usize,
    rejection: Option<crate::error::Diagnostic>,
}

impl<'a, P> AnnotationPlan<'a, P>
where
    P: Phase,
{
    pub(crate) fn ty(&self) -> &crate::ast::Type<P> {
        match &self.ty {
            PlannedAnnotationType::Borrowed(ty) => ty,
            PlannedAnnotationType::Materialized(ty) => ty,
        }
    }

    pub(crate) fn identity_is_canonical(&self) -> bool {
        self.identity_canonical
    }

    pub(crate) fn holes(&self) -> &[AnnotationHole] {
        &self.holes
    }

    pub(crate) fn is_closed_borrowed(&self) -> bool {
        matches!(self.ty, PlannedAnnotationType::Borrowed(_))
    }

    /// A source-closed phase must never retain the Lowered-only occurrence
    /// relation or publication walk. Keeping this as a real phase-boundary
    /// invariant also makes both fields structurally live in Prime builds.
    pub(crate) fn has_no_transport_state(&self) -> bool {
        self.transport.is_none() && self.binder_events.is_empty()
    }

    /// Report the source-level admissibility decision after the caller has
    /// completed its pre-existing structural preflight, but before it opens
    /// an owner or publishes any part of this plan.
    pub(crate) fn require_admitted(&self) -> Result<(), Error> {
        match &self.rejection {
            Some(error) => Err((**error).clone()),
            None => Ok(()),
        }
    }

    #[cfg(all(test, feature = "surface"))]
    fn observation(&self) -> AnnotationPlanObservation {
        let (infer_routes, contributions, root_present, emitted_infer_count) = self
            .transport
            .as_ref()
            .map(|transport| {
                let infer_routes = transport
                    .sources
                    .iter()
                    .enumerate()
                    .filter(|(_, source)| source.is_infer)
                    .map(|(index, source)| {
                        let id = AliasSourceOccurrenceId::from_index(index);
                        (
                            source.span,
                            source.disposition,
                            transport.first_enclosing_forall(id).cloned(),
                        )
                    })
                    .collect();
                (
                    infer_routes,
                    transport.contributions.clone(),
                    transport.root.is_some(),
                    transport.emitted_infers.len(),
                )
            })
            .unwrap_or_else(|| (Vec::new(), Vec::new(), false, 0));
        AnnotationPlanObservation {
            closed_borrowed: self.is_closed_borrowed(),
            identity_canonical: self.identity_canonical,
            holes: self.holes.clone(),
            infer_routes,
            contributions,
            root_present,
            emitted_infer_count,
            binder_event_count: self.binder_events.len(),
            rejection: self
                .rejection
                .as_ref()
                .map(|error| error.diagnostic().clone()),
        }
    }
}

#[cfg(feature = "surface")]
pub(crate) struct MaterializedAnnotation {
    pub(crate) value: ScopedType,
    binder_events: Vec<AnnotationBinderEvent>,
}

#[cfg(feature = "surface")]
enum AnnotationPublication<'a> {
    Closed(&'a crate::ast::Type<Lowered>),
    Events(Vec<AnnotationBinderEvent>),
}

#[cfg(feature = "surface")]
impl AnnotationPublication<'_> {
    fn publish<'m>(self, tcx: &mut TypeCtx<'m, '_, Lowered>) {
        match self {
            AnnotationPublication::Closed(written) => {
                super::types::publish_validated_type_binders(written, tcx)
            }
            AnnotationPublication::Events(events) => {
                #[cfg(test)]
                update_annotation_plan_work(|work| {
                    work.binder_events_published += events.len();
                });
                publish_annotation_binder_events(events, tcx)
            }
        }
    }
}

#[cfg(feature = "surface")]
struct CommittedAnnotation<'a> {
    resolved: InternedType<Lowered>,
    publication: AnnotationPublication<'a>,
}

pub(crate) struct PlannedHeaderAnnotation<'a, P>
where
    P: Phase,
{
    pub(crate) written: &'a crate::ast::Type<P>,
    pub(crate) plan: AnnotationPlan<'a, P>,
}

#[cfg(feature = "surface")]
enum HeaderAnnotationState<'a> {
    Missing,
    Planned(PlannedHeaderAnnotation<'a, Lowered>),
    Reserved(ReservedAnnotation),
    Staged,
    Committed(AnnotationPublication<'a>),
}

#[cfg(feature = "surface")]
pub(crate) struct ReservedAnnotation {
    written: InternedType<Lowered>,
    value: ScopedType,
    binder_events: Vec<AnnotationBinderEvent>,
}

#[cfg(feature = "surface")]
impl ReservedAnnotation {
    pub(crate) fn stage_into(
        self,
        publication: &mut crate::pass::typecheck_full::publication::PublicationBuilder,
        tcx: &mut TypeCtx<'_, '_, Lowered>,
    ) -> (InternedType<Lowered>, ScopedType) {
        stage_annotation_binder_events(self.binder_events, publication, tcx);
        (self.written, self.value)
    }
}

#[cfg(feature = "surface")]
struct PlannedValueSlot<'a> {
    param: &'a crate::ast::Param<Lowered>,
    ty: Option<InternedType<Lowered>>,
    expected_alpha_edge_count: usize,
    annotation: HeaderAnnotationState<'a>,
    needs_type_publication: bool,
}

pub(crate) struct PlannedSourceValueSlot<'a, P>
where
    P: Phase,
{
    pub(crate) param: &'a crate::ast::Param<P>,
    pub(crate) annotation: Option<PlannedHeaderAnnotation<'a, P>>,
}

/// Phase-neutral, pure source-header plan. Lowered wraps this descriptor with
/// owner/publication state; Prime consumes the same borrowed group and
/// annotation decisions with an uninhabited source-placeholder payload.
pub(crate) struct PlannedSourceLambdaHeader<'a, P>
where
    P: Phase,
{
    pub(crate) signature: &'a crate::ast::Signature<P>,
    pub(crate) value_slots: Vec<PlannedSourceValueSlot<'a, P>>,
    pub(crate) return_annotation: Option<PlannedHeaderAnnotation<'a, P>>,
    ambient_lexical: Option<PromotedPlanningLexicalView>,
}

impl<'a, P> PlannedSourceLambdaHeader<'a, P>
where
    P: Phase,
{
    pub(crate) fn group_refs(&self) -> crate::ast::SignatureGroupRefs<'a, P> {
        let signature: &'a crate::ast::Signature<P> = self.signature;
        signature.canonical_group_refs()
    }

    pub(crate) fn promote_ambient_lexical<'m>(&mut self, tcx: &TypeCtx<'m, '_, P>)
    where
        P: TyperPhase,
    {
        if self.ambient_lexical.is_none() {
            self.ambient_lexical = Some(PlanningLexicalView::borrowed(tcx).into_promoted());
        }
    }

    pub(crate) fn value_slots_require_context(&self) -> bool {
        self.value_slots.iter().any(|slot| {
            slot.annotation
                .as_ref()
                .is_none_or(|annotation| !annotation.plan.holes().is_empty())
        })
    }

    pub(crate) fn require_admitted_in_source_order(&self) -> Result<(), Error> {
        for slot in &self.value_slots {
            if let Some(annotation) = &slot.annotation {
                annotation.plan.require_admitted()?;
            }
        }
        if let Some(annotation) = &self.return_annotation {
            annotation.plan.require_admitted()?;
        }
        Ok(())
    }
}

#[cfg(feature = "surface")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlannedBinderId(usize);

#[cfg(feature = "surface")]
#[derive(Debug)]
struct PlannedAlphaEdge {
    expected_name: String,
    expected_binder_span: Span,
    source: PlannedBinderId,
    source_name: String,
    source_binder_span: Span,
    kind: Kind,
    source_binder_id: Option<super::env::TypeBinderId>,
}

#[cfg(feature = "surface")]
pub(crate) enum PlannedLambdaReturn {
    BodySynthesized,
    Concrete(InternedType<Lowered>),
    ContextRequired { hole_span: Span },
}

#[cfg(feature = "surface")]
type SelectedLambdaCheck = (InternedType<Lowered>, Vec<InternedType<Lowered>>);

#[cfg(feature = "surface")]
type SelectedLambdaSynth = (Vec<InternedType<Lowered>>, Option<InternedType<Lowered>>);

#[cfg(all(test, feature = "surface"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ExpectedAlphaMaterializationWork {
    canonicalization_calls: usize,
    canonicalization_input_nodes: usize,
    calls: usize,
    input_nodes: usize,
    protected_name_nodes: usize,
    alpha_edge_installs: usize,
    fresh_name_probes: usize,
}

#[cfg(all(test, feature = "surface"))]
thread_local! {
    static EXPECTED_ALPHA_MATERIALIZATION_WORK:
        std::cell::Cell<ExpectedAlphaMaterializationWork> =
        const { std::cell::Cell::new(ExpectedAlphaMaterializationWork {
            canonicalization_calls: 0,
            canonicalization_input_nodes: 0,
            calls: 0,
            input_nodes: 0,
            protected_name_nodes: 0,
            alpha_edge_installs: 0,
            fresh_name_probes: 0,
        }) };
}

#[cfg(all(test, feature = "surface"))]
fn lowered_type_node_count(ty: &Type<Lowered>) -> usize {
    match ty {
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => 1,
        Type::Function { param, ret, .. }
        | Type::Product {
            left: param,
            right: ret,
            ..
        }
        | Type::Sum {
            left: param,
            right: ret,
            ..
        } => 1 + lowered_type_node_count(param) + lowered_type_node_count(ret),
        Type::Path { args, .. } | Type::Goal { args, .. } => {
            1 + args.iter().map(lowered_type_node_count).sum::<usize>()
        }
        Type::Forall { body, .. } => 1 + lowered_type_node_count(body),
        Type::LabelSugar { ext, .. } => match *ext {},
    }
}

#[cfg(all(test, feature = "surface"))]
fn reset_expected_alpha_materialization_work() {
    EXPECTED_ALPHA_MATERIALIZATION_WORK
        .with(|work| work.set(ExpectedAlphaMaterializationWork::default()));
}

#[cfg(all(test, feature = "surface"))]
fn expected_alpha_materialization_work() -> ExpectedAlphaMaterializationWork {
    EXPECTED_ALPHA_MATERIALIZATION_WORK.with(std::cell::Cell::get)
}

#[cfg(feature = "surface")]
fn record_expected_alpha_materialization(_ty: &Type<Lowered>) {
    #[cfg(test)]
    EXPECTED_ALPHA_MATERIALIZATION_WORK.with(|work| {
        let mut current = work.get();
        current.calls += 1;
        current.input_nodes += lowered_type_node_count(_ty);
        work.set(current);
    });
}

#[cfg(feature = "surface")]
#[derive(Clone)]
struct ExpectedAlphaEmission {
    name: String,
    source: Option<PlannedBinderId>,
}

#[cfg(feature = "surface")]
fn collect_expected_alpha_free_names(
    ty: &Type<Lowered>,
    out: &mut HashSet<String>,
    bound: &mut HashMap<String, usize>,
) {
    #[cfg(test)]
    EXPECTED_ALPHA_MATERIALIZATION_WORK.with(|work| {
        let mut current = work.get();
        current.protected_name_nodes += 1;
        work.set(current);
    });
    match ty {
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
        Type::Path { segments, args, .. } => {
            if let [name] = segments.as_slice()
                && !bound.contains_key(name.as_str())
            {
                out.insert(name.name.clone());
            }
            for arg in args {
                collect_expected_alpha_free_names(arg, out, bound);
            }
        }
        Type::Function { param, ret, .. } => {
            collect_expected_alpha_free_names(param, out, bound);
            collect_expected_alpha_free_names(ret, out, bound);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_expected_alpha_free_names(left, out, bound);
            collect_expected_alpha_free_names(right, out, bound);
        }
        Type::Forall { param, body, .. } => {
            *bound.entry(param.name.clone()).or_default() += 1;
            collect_expected_alpha_free_names(body, out, bound);
            let count = bound
                .get_mut(&param.name)
                .expect("an expected-alpha free-name walk lost its binder");
            *count -= 1;
            if *count == 0 {
                bound.remove(&param.name);
            }
        }
        Type::Goal { args, .. } => {
            for arg in args {
                collect_expected_alpha_free_names(arg, out, bound);
            }
        }
        Type::LabelSugar { ext, .. } => match *ext {},
    }
}

/// One commit-local converter from exact selected binder edges to ordinary
/// source-aligned `Type<Lowered>`. The lexical stack associates a bare path
/// with the nearest selected or structural binder before changing its
/// spelling, so a same-spelled nested `forall` shadows an edge instead of
/// becoming a global substitution key.
#[cfg(feature = "surface")]
struct ExpectedAlphaMaterializer {
    bindings: HashMap<String, Vec<ExpectedAlphaEmission>>,
    source_names: HashSet<String>,
    taken: HashSet<String>,
    next_suffix: HashMap<String, usize>,
}

#[cfg(feature = "surface")]
impl ExpectedAlphaMaterializer {
    fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            source_names: HashSet::new(),
            taken: HashSet::new(),
            next_suffix: HashMap::new(),
        }
    }

    fn advance_edges(&mut self, edges: &[PlannedAlphaEdge]) {
        for edge in edges {
            #[cfg(test)]
            EXPECTED_ALPHA_MATERIALIZATION_WORK.with(|work| {
                let mut current = work.get();
                current.alpha_edge_installs += 1;
                work.set(current);
            });
            assert!(
                edge.source_binder_id.is_some(),
                "an expected/source alpha edge materialized before its source proof was minted"
            );
            self.source_names.insert(edge.source_name.clone());
            self.bindings
                .entry(edge.expected_name.clone())
                .or_default()
                .push(ExpectedAlphaEmission {
                    name: edge.source_name.clone(),
                    source: Some(edge.source),
                });
        }
    }

    fn prepare_type(&mut self, ty: &Type<Lowered>) {
        self.taken.clear();
        self.next_suffix.clear();
        collect_expected_alpha_free_names(ty, &mut self.taken, &mut HashMap::new());
    }

    fn push_local(&mut self, original: &str, emitted: String) {
        self.bindings
            .entry(original.to_owned())
            .or_default()
            .push(ExpectedAlphaEmission {
                name: emitted,
                source: None,
            });
    }

    fn pop_local(&mut self, original: &str) {
        let emissions = self
            .bindings
            .get_mut(original)
            .expect("an expected-alpha binder stack lost its local entry");
        let popped = emissions
            .pop()
            .expect("an expected-alpha binder stack underflowed");
        assert!(
            popped.source.is_none(),
            "an expected-alpha binder stack popped a selected source edge"
        );
        if emissions.is_empty() {
            self.bindings.remove(original);
        }
    }

    fn name_is_taken(&self, name: &str) -> bool {
        #[cfg(test)]
        EXPECTED_ALPHA_MATERIALIZATION_WORK.with(|work| {
            let mut current = work.get();
            current.fresh_name_probes += 1;
            work.set(current);
        });
        self.source_names.contains(name) || self.taken.contains(name)
    }

    fn fresh_local_name(&mut self, base: &str) -> String {
        if !self.name_is_taken(base) {
            let emitted = base.to_owned();
            self.taken.insert(emitted.clone());
            return emitted;
        }

        let mut suffix = self.next_suffix.get(base).copied().unwrap_or(2);
        loop {
            let emitted = crate::naming::indexed_name(base, suffix);
            suffix += 1;
            if !self.name_is_taken(&emitted) {
                self.next_suffix.insert(base.to_owned(), suffix);
                self.taken.insert(emitted.clone());
                return emitted;
            }
        }
    }

    fn materialize(&mut self, ty: &Type<Lowered>) -> Type<Lowered> {
        match ty {
            Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => ty.clone(),
            Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                caps,
            } => Type::Function {
                param: Box::new(self.materialize(param)),
                ret: Box::new(self.materialize(ret)),
                meta: crate::ast::Meta::new(meta.span),
                abi_arity: *abi_arity,
                caps: *caps,
            },
            Type::Product { left, right, meta } => Type::Product {
                left: Box::new(self.materialize(left)),
                right: Box::new(self.materialize(right)),
                meta: crate::ast::Meta::new(meta.span),
            },
            Type::Sum { left, right, meta } => Type::Sum {
                left: Box::new(self.materialize(left)),
                right: Box::new(self.materialize(right)),
                meta: crate::ast::Meta::new(meta.span),
            },
            Type::Path {
                segments,
                args,
                meta,
                ..
            } => {
                let args = args.iter().map(|arg| self.materialize(arg)).collect();
                let emitted = match segments.as_slice() {
                    [name] => self
                        .bindings
                        .get(name.as_str())
                        .and_then(|bindings| bindings.last())
                        .map(|binding| binding.name.clone()),
                    _ => None,
                };
                match emitted {
                    Some(name) => Type::synth_path(vec![name], args, meta.span),
                    None => Type::synth_path_segments(segments.clone(), args, meta.span),
                }
            }
            Type::Forall { param, body, meta } => {
                let emitted = self.fresh_local_name(&param.name);
                self.push_local(&param.name, emitted.clone());
                let body = self.materialize(body);
                self.pop_local(&param.name);
                Type::Forall {
                    param: crate::ast::TypeParam {
                        name: emitted,
                        span: param.span,
                        kind: param.kind.clone(),
                    },
                    body: Box::new(body),
                    meta: crate::ast::Meta::new(meta.span),
                }
            }
            Type::Goal {
                goal,
                args,
                meta,
                ext,
            } => Type::Goal {
                goal: *goal,
                args: args.iter().map(|arg| self.materialize(arg)).collect(),
                meta: crate::ast::Meta::new(meta.span),
                ext: *ext,
            },
            Type::LabelSugar { ext, .. } => match *ext {},
        }
    }
}

#[cfg(feature = "surface")]
fn canonicalize_expected_layer<'v, 'a, 'm>(
    ty: &'v Type<Lowered>,
    ctx: &AliasCtx<'a, 'm, Lowered>,
    binders: &'a dyn AliasBinderLookup,
    already_canonical: bool,
) -> (Type<Lowered>, bool)
where
    'a: 'v,
    'm: 'v,
{
    #[cfg(test)]
    EXPECTED_ALPHA_MATERIALIZATION_WORK.with(|work| {
        let mut current = work.get();
        current.canonicalization_calls += 1;
        current.canonicalization_input_nodes += lowered_type_node_count(ty);
        work.set(current);
    });
    super::aliases::canonicalize_for_comparison_with_binder_lookup(
        ty,
        ctx,
        binders,
        already_canonical,
    )
}

#[cfg(feature = "surface")]
fn take_expected_frontier<'v, 'a, 'm>(
    ty: Type<Lowered>,
    ctx: &AliasCtx<'a, 'm, Lowered>,
    binders: &'v dyn AliasBinderLookup,
    identity_canonical: bool,
) -> (Type<Lowered>, bool, Option<Type<Lowered>>)
where
    'a: 'v,
    'm: 'v,
{
    match super::aliases::unfold_alias_frontier_for_comparison_with_binder_lookup(
        &ty,
        ctx,
        binders,
        identity_canonical,
    ) {
        Some((unfolded, canonical)) => (unfolded, canonical, Some(ty)),
        None => (ty, identity_canonical, None),
    }
}

#[cfg(feature = "surface")]
struct ExpectedAlphaBinderLookup<'a> {
    lexical: &'a dyn AliasBinderLookup,
    selected: &'a HashSet<String>,
}

#[cfg(feature = "surface")]
impl AliasBinderLookup for ExpectedAlphaBinderLookup<'_> {
    fn contains_alias_binder(&self, name: &str) -> bool {
        self.selected.contains(name) || self.lexical.contains_alias_binder(name)
    }

    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
        self.lexical.for_each_alias_binder(visit);
        for name in self.selected {
            visit(name);
        }
    }
}

#[cfg(feature = "surface")]
fn materialize_expected_alpha(
    ty: InternedType<Lowered>,
    materializer: &mut ExpectedAlphaMaterializer,
) -> InternedType<Lowered> {
    record_expected_alpha_materialization(ty.as_type());
    let identity_canonical = ty.identity_is_canonical();
    materializer.prepare_type(ty.as_type());
    InternedType::fresh_with_identity(materializer.materialize(ty.as_type()), identity_canonical)
}

/// One pure lambda-header plan over the authoritative signature cursor.
/// Expected-layer traversal, readiness classification, and every annotation
/// diagnostic complete before this value may mutate `TypeCtx` or allocate an
/// annotation goal owner.
#[cfg(feature = "surface")]
pub(crate) struct LambdaHeaderPlan<'a> {
    signature: &'a crate::ast::Signature<Lowered>,
    value_slots: Vec<PlannedValueSlot<'a>>,
    return_annotation: HeaderAnnotationState<'a>,
    body_expected: Option<InternedType<Lowered>>,
    body_expected_alpha_edge_count: usize,
    alpha_edges: Option<Vec<PlannedAlphaEdge>>,
    alpha_edges_bound: bool,
    ambient_lexical: Option<PromotedPlanningLexicalView>,
}

fn planned_annotation<'a, 'm, P>(
    written: &'a crate::ast::Type<P>,
    tcx: &TypeCtx<'m, '_, P>,
    lexical: &PlanningLexicalView<'_, 'm, P>,
) -> Result<PlannedHeaderAnnotation<'a, P>, Error>
where
    P: TyperPhase + crate::pass::resolve::ExportContractPhase + Clone,
    'm: 'a,
{
    Ok(PlannedHeaderAnnotation {
        written,
        plan: plan_source_annotation(written, tcx, lexical, false)?,
    })
}

pub(crate) fn plan_source_lambda_header<'m, P>(
    signature: &'m crate::ast::Signature<P>,
    return_annotation: Option<&'m crate::ast::Type<P>>,
    tcx: &TypeCtx<'m, '_, P>,
) -> Result<PlannedSourceLambdaHeader<'m, P>, Error>
where
    P: TyperPhase + crate::pass::resolve::ExportContractPhase + Clone,
{
    let mut lexical = PlanningLexicalView::with_signature_binders(tcx, signature);
    let mut value_slots = Vec::with_capacity(signature.value_param_count());
    let mut prefix_end = 0usize;

    for group in signature.canonical_group_refs() {
        match group {
            crate::ast::SignatureGroupRef::Type(params) => {
                prefix_end += params.len();
                lexical.advance_signature_prefix(prefix_end);
            }
            crate::ast::SignatureGroupRef::Value(params) => {
                prefix_end += params.len();
                lexical.advance_signature_prefix(prefix_end);
                for entry in params {
                    let crate::ast::SignatureParam::Value(param) = entry else {
                        unreachable!("signature value group contains only value parameters")
                    };
                    let annotation = param
                        .ty
                        .as_ref()
                        .map(|written| planned_annotation(written, tcx, &lexical))
                        .transpose()?;
                    value_slots.push(PlannedSourceValueSlot { param, annotation });
                }
            }
        }
    }

    let return_annotation = return_annotation
        .map(|written| planned_annotation(written, tcx, &lexical))
        .transpose()?;
    Ok(PlannedSourceLambdaHeader {
        signature,
        value_slots,
        return_annotation,
        ambient_lexical: None,
    })
}

#[cfg(feature = "surface")]
pub(crate) fn plan_lowered_lambda_header<'m>(
    signature: &'m crate::ast::Signature<Lowered>,
    return_annotation: Option<&'m crate::ast::Type<Lowered>>,
    tcx: &TypeCtx<'m, '_, Lowered>,
) -> Result<LambdaHeaderPlan<'m>, Error> {
    let source = plan_source_lambda_header(signature, return_annotation, tcx)?;
    let value_slots = source
        .value_slots
        .into_iter()
        .map(|slot| {
            let annotation = slot.annotation.map_or(
                HeaderAnnotationState::Missing,
                HeaderAnnotationState::Planned,
            );
            let needs_type_publication = match &annotation {
                HeaderAnnotationState::Missing => true,
                HeaderAnnotationState::Planned(planned) => !planned.plan.holes().is_empty(),
                HeaderAnnotationState::Reserved(_)
                | HeaderAnnotationState::Staged
                | HeaderAnnotationState::Committed(_) => {
                    unreachable!("a new lambda plan already committed an annotation")
                }
            };
            PlannedValueSlot {
                param: slot.param,
                ty: None,
                expected_alpha_edge_count: 0,
                annotation,
                needs_type_publication,
            }
        })
        .collect();
    let return_annotation = source.return_annotation.map_or(
        HeaderAnnotationState::Missing,
        HeaderAnnotationState::Planned,
    );
    Ok(LambdaHeaderPlan {
        signature: source.signature,
        value_slots,
        return_annotation,
        body_expected: None,
        body_expected_alpha_edge_count: 0,
        alpha_edges: None,
        alpha_edges_bound: false,
        ambient_lexical: source.ambient_lexical,
    })
}

#[cfg(feature = "surface")]
impl AnnotationPlan<'_, Lowered> {
    fn validated_type<'m>(
        &self,
        written: &'m crate::ast::Type<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> InternedType<Lowered> {
        self.require_admitted()
            .expect("a rejected annotation reached validated type construction");
        assert!(
            self.holes.is_empty(),
            "an annotation with an unresolved retained occurrence needs an equation"
        );
        if self.is_closed_borrowed() {
            tcx.intern_type(written)
        } else {
            InternedType::fresh_with_identity(self.ty().clone(), self.identity_canonical)
        }
    }

    fn into_validated_type_and_publication<'m>(
        self,
        written: &'m crate::ast::Type<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> (InternedType<Lowered>, AnnotationPublication<'m>) {
        assert!(
            self.holes.is_empty(),
            "an annotation with an unresolved retained occurrence needs an equation"
        );
        let ty = self.validated_type(written, tcx);
        let publication = self.into_validated_publication(written);
        (ty, publication)
    }

    fn into_validated_publication<'m>(
        self,
        written: &'m crate::ast::Type<Lowered>,
    ) -> AnnotationPublication<'m> {
        assert!(
            self.holes.is_empty(),
            "an annotation with an unresolved retained occurrence needs an equation"
        );
        if self.is_closed_borrowed() {
            assert!(
                self.binder_events.is_empty(),
                "a closed borrowed annotation retained a publication event table"
            );
            AnnotationPublication::Closed(written)
        } else {
            AnnotationPublication::Events(self.binder_events)
        }
    }

    pub(crate) fn reserve_for_owner<'m>(
        self,
        written: &'m crate::ast::Type<Lowered>,
        store: &mut GoalStore,
        owner: TypeGoalOwner,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<ReservedAnnotation, Error> {
        let scope = store.owner_scope_snapshot(owner, written.span())?;
        self.reserve_for_owner_at_scope(written, store, owner, &scope, tcx)
    }

    pub(crate) fn reserve_for_owner_at_scope<'m>(
        self,
        written: &'m crate::ast::Type<Lowered>,
        store: &mut GoalStore,
        owner: TypeGoalOwner,
        scope: &super::RigidScope,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<ReservedAnnotation, Error> {
        self.require_admitted()?;
        let written_view = tcx.intern_type(written);
        if self.holes.is_empty() {
            let ty = self.validated_type(written, tcx);
            let binder_events = self.binder_events;
            let value = store.scoped_type_at_lexical_prefix(scope, ty);
            return Ok(ReservedAnnotation {
                written: written_view,
                value,
                binder_events,
            });
        }
        let MaterializedAnnotation {
            value,
            binder_events,
        } = self.materialize_at_scope(store, owner, scope)?;
        Ok(ReservedAnnotation {
            written: written_view,
            value,
            binder_events,
        })
    }

    fn finish_closed<'m>(
        self,
        written: &'m crate::ast::Type<Lowered>,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) {
        self.require_admitted()
            .expect("a rejected annotation reached closed publication");
        assert!(
            self.is_closed_borrowed(),
            "only a borrowed closed annotation may skip materialization"
        );
        assert!(
            self.binder_events.is_empty(),
            "a closed borrowed annotation retained a publication event table"
        );
        super::types::publish_validated_type_binders(written, tcx);
    }

    /// Commit one admitted source annotation as a single forward equation.
    /// A closed or fully dropped plan uses the ordinary equivalence seam and
    /// allocates no goal state. A plan with retained occurrences opens one
    /// root-local annotation owner, closes it atomically, and only then
    /// publishes its already-validated binder metadata.
    pub(crate) fn commit_against<'m>(
        self,
        written: &'m crate::ast::Type<Lowered>,
        expected: &InternedType<Lowered>,
        span: Span,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<InternedType<Lowered>, Error> {
        let committed = self.commit_against_deferred(written, expected, span, tcx)?;
        committed.publication.publish(tcx);
        Ok(committed.resolved)
    }

    fn commit_against_deferred<'m>(
        self,
        written: &'m crate::ast::Type<Lowered>,
        expected: &InternedType<Lowered>,
        span: Span,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<CommittedAnnotation<'m>, Error> {
        self.require_admitted()?;
        if self.holes.is_empty() {
            super::require_type_equiv_state(
                self.ty(),
                expected.as_type(),
                span,
                &tcx.env.alias_ctx(),
                self.identity_canonical,
                expected.identity_is_canonical(),
            )?;
            let publication = if self.is_closed_borrowed() {
                assert!(
                    self.binder_events.is_empty(),
                    "a closed borrowed annotation retained a publication event table"
                );
                AnnotationPublication::Closed(written)
            } else {
                AnnotationPublication::Events(self.binder_events)
            };
            return Ok(CommittedAnnotation {
                resolved: expected.clone(),
                publication,
            });
        }

        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(tcx);
        let owner = store.begin_owner(None, scope, GoalOwnerKind::Annotation, span)?;
        let expected = store.scoped_type(owner, expected.clone(), span)?;
        let materialized = self.materialize(&mut store, owner)?;
        let found = materialized.value.clone();
        let mut delta = store.begin_delta(owner, span)?;
        store.constrain_equation(
            &mut delta,
            found,
            ExpectedEquationOperand::solver(expected),
            span,
            tcx,
        )?;
        let prepared = store.prepare_owner_with_publication(
            delta,
            vec![(materialized.value, GoalEscape::ClosedAt(owner))],
            Vec::new(),
            tcx,
        )?;
        let (commit, outputs, publication) = prepared.into_parts();
        assert!(
            publication.into_outputs().is_empty(),
            "an annotation equation produced publication-only output"
        );
        let mut outputs = outputs.into_outputs();
        assert_eq!(
            outputs.len(),
            1,
            "an annotation equation produced an unexpected output count"
        );
        let ClosedGoalOutput::GoalFree(output) = outputs.pop().unwrap() else {
            unreachable!("a root annotation equation retained an open inference owner")
        };
        assert_eq!(
            output.destination(),
            owner,
            "a root annotation equation closed at a different owner"
        );
        let resolved = output.into_scoped_type().into_interned_type();
        store.commit_prepared_owner(commit);
        Ok(CommittedAnnotation {
            resolved,
            publication: AnnotationPublication::Events(materialized.binder_events),
        })
    }

    /// Commit this pure plan into one already-selected lawful annotation
    /// owner. Each retained source occurrence allocates one goal; duplicated
    /// materialized leaves reuse that exact goal. The generic same-phase type
    /// visitor performs only mechanical Infer-to-Goal replacement.
    pub(crate) fn materialize(
        self,
        store: &mut GoalStore,
        owner: TypeGoalOwner,
    ) -> Result<MaterializedAnnotation, Error> {
        let scope = store.owner_scope_snapshot(owner, self.ty().span())?;
        self.materialize_at_scope(store, owner, &scope)
    }

    fn materialize_at_scope(
        self,
        store: &mut GoalStore,
        owner: TypeGoalOwner,
        scope: &super::RigidScope,
    ) -> Result<MaterializedAnnotation, Error> {
        let AnnotationPlan {
            ty,
            identity_canonical,
            holes,
            transport,
            binder_events,
            rejection,
        } = self;
        assert!(
            rejection.is_none(),
            "a rejected annotation reached goal materialization"
        );
        #[cfg(test)]
        update_annotation_plan_work(|work| {
            work.committed_materializations += 1;
            work.goals_allocated += holes.len();
        });
        let mut by_source = HashMap::with_capacity(holes.len());
        for hole in &holes {
            let goal = store.alloc_goal(
                owner,
                hole.required_kind.clone(),
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::annotation(hole.span),
            )?;
            assert!(
                by_source.insert(hole.source, goal).is_none(),
                "one source annotation occurrence allocated more than one goal"
            );
        }

        let mut ty = match ty {
            PlannedAnnotationType::Borrowed(ty) => ty.clone(),
            PlannedAnnotationType::Materialized(ty) => ty,
        };
        if let Some(transport) = &transport {
            struct InferToGoal<'a> {
                transport: &'a AliasSourceOccurrenceTransport,
                by_source: &'a HashMap<AliasSourceOccurrenceId, TypeGoalRef>,
                next: usize,
            }

            impl visit_mut::TypecheckVisitMut<Lowered> for InferToGoal<'_> {
                fn visit_type(&mut self, ty: &mut Type<Lowered>) {
                    let Type::Infer { meta, .. } = ty else {
                        visit_mut::walk_type(self, ty);
                        return;
                    };
                    let (output, source) = self
                        .transport
                        .emitted_infers
                        .get(self.next)
                        .copied()
                        .expect("materialized annotation emitted an untracked Infer leaf");
                    self.next += 1;
                    let emitted_span = output
                        .map_or(self.transport.sources[source.index()].span, |output| {
                            self.transport.outputs[output.index()].span
                        });
                    assert_eq!(
                        emitted_span, meta.span,
                        "the occurrence transport and materialized type walk diverged"
                    );
                    let goal = self.by_source[&source];
                    let meta = meta.clone();
                    *ty = Type::Goal {
                        goal,
                        args: Vec::new(),
                        meta,
                        ext: (),
                    };
                }
            }

            use visit_mut::TypecheckVisitMut as _;
            let mut converter = InferToGoal {
                transport,
                by_source: &by_source,
                next: 0,
            };
            converter.visit_type(&mut ty);
            assert_eq!(
                converter.next,
                transport.emitted_infers.len(),
                "the materialized type omitted a transported Infer leaf"
            );
        }
        let value = store.scoped_type_at_lexical_prefix(
            scope,
            InternedType::fresh_with_identity(ty, identity_canonical),
        );
        Ok(MaterializedAnnotation {
            value,
            binder_events,
        })
    }
}

#[cfg(feature = "surface")]
fn missing_synth_parameter(name: &str, span: Span) -> Error {
    Error::type_(
        span,
        format!(
            "cannot synthesize the type of `fn` parameter `{name}`: add a concrete annotation or use the lambda in a checking position"
        ),
    )
}

#[cfg(feature = "surface")]
fn open_synth_parameter(name: &str, written: &Type<Lowered>, hole: &AnnotationHole) -> Error {
    let reason = if matches!(written, Type::Infer { .. }) {
        "a `_` parameter annotation requires a surrounding expected function type"
    } else {
        "a nested `_` placeholder in a `fn` signature has no surrounding context to resolve against"
    };
    Error::type_(
        hole.span,
        format!(
            "cannot synthesize the type of `fn` parameter `{name}`: {reason}; write the annotation out fully or use the lambda in a checking position"
        ),
    )
}

#[cfg(feature = "surface")]
fn open_synth_return(hole: &AnnotationHole) -> Error {
    open_synth_return_at(hole.span)
}

#[cfg(feature = "surface")]
pub(crate) fn open_synth_return_at(span: Span) -> Error {
    Error::type_(
        span,
        "nested `_` placeholder in a `fn` signature has no surrounding context to resolve against — this lambda literal sits in a synthesis-only position. Bind it at a position whose expected type fills the placeholder (a value-arg slot of a call to an annotated `fn`, or a return position with an explicit type), or write the annotation out fully",
    )
}

#[cfg(feature = "surface")]
enum SelectedExpectedValueGroup {
    Deferred,
    TooShort,
    Ready {
        bindings: Vec<InternedType<Lowered>>,
        zero_head: Option<InternedType<Lowered>>,
        zero_original: Option<Type<Lowered>>,
    },
}

#[cfg(feature = "surface")]
fn select_expected_value_group<'v, 'a, 'm>(
    mut layer: Type<Lowered>,
    mut identity_canonical: bool,
    value_param_count: usize,
    alias_ctx: &AliasCtx<'a, 'm, Lowered>,
    lexical: &'v dyn AliasBinderLookup,
) -> SelectedExpectedValueGroup
where
    'a: 'v,
    'm: 'v,
{
    if value_param_count == 0 {
        let (head, canonical, original) =
            take_expected_frontier(layer, alias_ctx, lexical, identity_canonical);
        return if matches!(head, Type::Goal { .. } | Type::Infer { .. }) {
            SelectedExpectedValueGroup::Deferred
        } else {
            SelectedExpectedValueGroup::Ready {
                bindings: Vec::new(),
                zero_head: Some(InternedType::fresh_with_identity(head, canonical)),
                zero_original: original,
            }
        };
    }

    let mut bindings = Vec::with_capacity(value_param_count);
    for index in 0..value_param_count {
        if index + 1 == value_param_count {
            bindings.push(InternedType::fresh_with_identity(layer, identity_canonical));
            return SelectedExpectedValueGroup::Ready {
                bindings,
                zero_head: None,
                zero_original: None,
            };
        }
        let (head, canonical, _) =
            take_expected_frontier(layer, alias_ctx, lexical, identity_canonical);
        match head {
            Type::Goal { .. } | Type::Infer { .. } => {
                return SelectedExpectedValueGroup::Deferred;
            }
            Type::Product { left, right, .. } => {
                bindings.push(InternedType::fresh_with_identity(*left, canonical));
                layer = *right;
                identity_canonical = canonical;
            }
            _ => return SelectedExpectedValueGroup::TooShort,
        }
    }
    unreachable!("a nonempty expected value group returns from its final slot")
}

#[cfg(feature = "surface")]
impl<'m> LambdaHeaderPlan<'m> {
    pub(crate) fn promote_ambient_lexical(&mut self, tcx: &TypeCtx<'m, '_, Lowered>) {
        if self.ambient_lexical.is_none() {
            self.ambient_lexical = Some(PlanningLexicalView::borrowed(tcx).into_promoted());
        }
    }

    fn planning_lexical_view<'a>(
        &'a self,
        tcx: &'a TypeCtx<'m, '_, Lowered>,
    ) -> PlanningLexicalView<'a, 'm, Lowered> {
        match &self.ambient_lexical {
            Some(ambient) => PlanningLexicalView::with_promoted_signature_binders(
                ambient.clone(),
                self.signature,
            ),
            None => PlanningLexicalView::with_signature_binders(tcx, self.signature),
        }
    }

    pub(crate) fn group_refs(&self) -> crate::ast::SignatureGroupRefs<'m, crate::ast::Lowered> {
        let signature: &'m crate::ast::Signature<crate::ast::Lowered> = self.signature;
        signature.canonical_group_refs()
    }

    pub(crate) fn require_admitted_in_source_order(&self) -> Result<(), Error> {
        for slot in &self.value_slots {
            if let HeaderAnnotationState::Planned(planned) = &slot.annotation {
                planned.plan.require_admitted()?;
            }
        }
        if let HeaderAnnotationState::Planned(planned) = &self.return_annotation {
            planned.plan.require_admitted()?;
        }
        Ok(())
    }

    pub(crate) fn value_slot_needs_type_publication(&self, value_index: usize) -> bool {
        self.value_slots
            .get(value_index)
            .expect("a lambda header requested an unknown value slot")
            .needs_type_publication
    }

    /// Whether this exact lexical slice needs solver state. Missing
    /// annotations may receive an enclosing expectation, and open source
    /// annotations own goals. Closed annotations retain only sparse
    /// binder/occurrence proof metadata and need no empty GoalStore owner.
    pub(crate) fn annotation_slice_needs_owner(
        &self,
        value_range: std::ops::Range<usize>,
        include_return: bool,
    ) -> bool {
        let value_state = self
            .value_slots
            .get(value_range)
            .expect("a lambda lexical slice named an invalid value range")
            .iter()
            .any(|slot| match &slot.annotation {
                HeaderAnnotationState::Planned(planned) => !planned.plan.holes().is_empty(),
                HeaderAnnotationState::Missing => true,
                HeaderAnnotationState::Reserved(_)
                | HeaderAnnotationState::Staged
                | HeaderAnnotationState::Committed(_) => {
                    unreachable!("a pure lambda plan already committed an annotation")
                }
            });
        value_state
            || (include_return
                && matches!(
                    &self.return_annotation,
                    HeaderAnnotationState::Planned(planned)
                        if !planned.plan.holes().is_empty()
                ))
    }

    pub(crate) fn value_slot_supplies_shape(&self, value_index: usize) -> bool {
        !self
            .value_slots
            .get(value_index)
            .expect("a lambda header requested an unknown value slot")
            .needs_type_publication
    }

    pub(crate) fn requires_immediate_selection(&self) -> bool {
        let open_value = self.value_slots.iter().any(|slot| match &slot.annotation {
            HeaderAnnotationState::Missing => true,
            HeaderAnnotationState::Planned(planned) => !planned.plan.holes().is_empty(),
            HeaderAnnotationState::Reserved(_)
            | HeaderAnnotationState::Staged
            | HeaderAnnotationState::Committed(_) => {
                unreachable!("a pure lambda plan already committed an annotation")
            }
        });
        open_value
            || matches!(
                &self.return_annotation,
                HeaderAnnotationState::Planned(planned)
                    if !matches!(planned.written, Type::Infer { .. })
                        && !planned.plan.holes().is_empty()
            )
    }

    pub(crate) fn value_slot_synth_type(
        &self,
        value_index: usize,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Option<InternedType<Lowered>> {
        let slot = self
            .value_slots
            .get(value_index)
            .expect("a lambda header requested an unknown value slot");
        match &slot.annotation {
            HeaderAnnotationState::Missing => None,
            HeaderAnnotationState::Planned(planned) if planned.plan.holes().is_empty() => {
                Some(planned.plan.validated_type(planned.written, tcx))
            }
            HeaderAnnotationState::Planned(_) => None,
            HeaderAnnotationState::Reserved(_)
            | HeaderAnnotationState::Staged
            | HeaderAnnotationState::Committed(_) => {
                unreachable!("a pure lambda plan already committed an annotation")
            }
        }
    }

    pub(crate) fn planned_return(&self, tcx: &TypeCtx<'m, '_, Lowered>) -> PlannedLambdaReturn {
        match &self.return_annotation {
            HeaderAnnotationState::Missing => PlannedLambdaReturn::BodySynthesized,
            HeaderAnnotationState::Planned(planned)
                if matches!(planned.written, Type::Infer { .. }) =>
            {
                PlannedLambdaReturn::BodySynthesized
            }
            HeaderAnnotationState::Planned(planned) => match planned.plan.holes().first() {
                Some(hole) => PlannedLambdaReturn::ContextRequired {
                    hole_span: hole.span,
                },
                None => {
                    PlannedLambdaReturn::Concrete(planned.plan.validated_type(planned.written, tcx))
                }
            },
            HeaderAnnotationState::Reserved(_)
            | HeaderAnnotationState::Staged
            | HeaderAnnotationState::Committed(_) => {
                unreachable!("a pure lambda plan already committed an annotation")
            }
        }
    }

    pub(crate) fn complete_written_type(
        &self,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Option<InternedType<Lowered>>, Error> {
        self.require_admitted_in_source_order()?;
        let PlannedLambdaReturn::Concrete(result) = self.planned_return(tcx) else {
            return Ok(None);
        };
        let Some(parameters) = (0..self.value_slots.len())
            .map(|index| self.value_slot_synth_type(index, tcx))
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(None);
        };
        Ok(Some(InternedType::fresh_canonical(
            super::synth::build_fn_type_from_body_type(
                self.signature,
                result,
                parameters,
                span,
                tcx,
            ),
        )))
    }

    pub(crate) fn select_check(
        &mut self,
        expected: &InternedType<Lowered>,
        fn_span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Option<SelectedLambdaCheck>, Error> {
        assert!(
            self.body_expected.is_none(),
            "a lambda plan selected checking more than once"
        );
        assert!(
            self.alpha_edges.is_none(),
            "a lambda plan retained alpha edges before checking was selected"
        );
        let mut lexical = self.planning_lexical_view(tcx);
        let mut prefix_end = 0usize;
        let mut saw_value_group = false;
        let mut selected_slots = Vec::with_capacity(self.value_slots.len());
        let type_param_count = self
            .signature
            .params
            .iter()
            .filter(|param| matches!(param, crate::ast::SignatureParam::Type(_)))
            .count();
        let mut selected_alpha_edges = Vec::with_capacity(type_param_count);
        let mut selected_expected_binders = HashSet::with_capacity(type_param_count);
        let alias_ctx = tcx.env.alias_ctx();
        let expected_binders = ExpectedAlphaBinderLookup {
            lexical: &lexical,
            selected: &selected_expected_binders,
        };
        // Canonicalize the complete expected tree once, then consume its
        // structural spine by ownership. Alias frontiers introduced below
        // that tree are expanded only when the one header cursor reaches
        // them; no later layer clones or re-qualifies its remaining suffix.
        let (mut expected_layer, mut expected_layer_canonical) = canonicalize_expected_layer(
            expected.as_type(),
            &alias_ctx,
            &expected_binders,
            expected.identity_is_canonical(),
        );

        for group in self.group_refs() {
            match group {
                crate::ast::SignatureGroupRef::Type(params) => {
                    for entry in params {
                        let crate::ast::SignatureParam::Type(source_param) = entry else {
                            unreachable!("signature type group contains only type parameters")
                        };
                        let expected_binders = ExpectedAlphaBinderLookup {
                            lexical: &lexical,
                            selected: &selected_expected_binders,
                        };
                        let (unfolded, canonical, _) = take_expected_frontier(
                            expected_layer,
                            &alias_ctx,
                            &expected_binders,
                            expected_layer_canonical,
                        );
                        if matches!(&unfolded, Type::Goal { .. } | Type::Infer { .. }) {
                            return Ok(None);
                        }
                        let Type::Forall {
                            param: expected_param,
                            body,
                            ..
                        } = unfolded
                        else {
                            return Err(Error::type_(
                                fn_span,
                                format!(
                                    "`fn` declares type parameter `{}` but the expected type has no matching forall layer",
                                    source_param.name
                                ),
                            ));
                        };
                        if expected_param.effective_kind() != source_param.effective_kind() {
                            return Err(Error::type_(
                                source_param.span,
                                format!(
                                    "`fn` type parameter `{}` has kind `{}`, but the matching expected forall parameter has kind `{}`",
                                    source_param.name,
                                    source_param.effective_kind(),
                                    expected_param.effective_kind()
                                ),
                            ));
                        }
                        let source = PlannedBinderId(selected_alpha_edges.len());
                        selected_alpha_edges.push(PlannedAlphaEdge {
                            expected_name: expected_param.name.clone(),
                            expected_binder_span: expected_param.span,
                            source,
                            source_name: source_param.name.clone(),
                            source_binder_span: source_param.span,
                            kind: source_param.effective_kind(),
                            source_binder_id: None,
                        });
                        selected_expected_binders.insert(expected_param.name);
                        expected_layer = *body;
                        expected_layer_canonical = canonical;
                    }
                    prefix_end += params.len();
                    lexical.advance_signature_prefix(prefix_end);
                }
                crate::ast::SignatureGroupRef::Value(params) => {
                    saw_value_group = true;
                    prefix_end += params.len();
                    lexical.advance_signature_prefix(prefix_end);
                    let expected_binders = ExpectedAlphaBinderLookup {
                        lexical: &lexical,
                        selected: &selected_expected_binders,
                    };
                    let (unfolded, canonical, original) = take_expected_frontier(
                        expected_layer,
                        &alias_ctx,
                        &expected_binders,
                        expected_layer_canonical,
                    );
                    if matches!(&unfolded, Type::Goal { .. } | Type::Infer { .. }) {
                        return Ok(None);
                    }
                    let (expected_param, ret) = match unfolded {
                        Type::Function { param, ret, .. } => (param, ret),
                        found => {
                            return Err(Error::type_(
                                fn_span,
                                format!(
                                    "lambda expression used where a function type was expected, but the expected type is `{}`",
                                    super::display_type(original.as_ref().unwrap_or(&found))
                                ),
                            ));
                        }
                    };
                    let selected_group = select_expected_value_group(
                        *expected_param,
                        canonical,
                        params.len(),
                        &alias_ctx,
                        &expected_binders,
                    );
                    let (bindings, zero_head, zero_original) = match selected_group {
                        SelectedExpectedValueGroup::Deferred => return Ok(None),
                        SelectedExpectedValueGroup::TooShort => {
                            return Err(Error::type_(
                                fn_span,
                                format!(
                                    "`fn` value-parameter count ({}) exceeds the expected function arity (the param type's right-spine has fewer leaves)",
                                    params.len()
                                ),
                            ));
                        }
                        SelectedExpectedValueGroup::Ready {
                            bindings,
                            zero_head,
                            zero_original,
                        } => (bindings, zero_head, zero_original),
                    };
                    if let Some(expected_param) = zero_head
                        && !matches!(expected_param.as_type(), Type::Unit { .. })
                    {
                        return Err(Error::type_(
                            fn_span,
                            format!(
                                "`fn` has no value parameters but the expected function takes `{}`",
                                super::display_type(
                                    zero_original.as_ref().unwrap_or(expected_param.as_type())
                                )
                            ),
                        ));
                    }
                    for (entry, ty) in params.iter().zip(bindings) {
                        let crate::ast::SignatureParam::Value(param) = entry else {
                            unreachable!("signature value group contains only value parameters")
                        };
                        let slot = self
                            .value_slots
                            .get(selected_slots.len())
                            .expect("lambda plan lost a value-parameter descriptor");
                        assert_eq!(slot.param.meta.span, param.meta.span);
                        if !self.value_slot_supplies_shape(selected_slots.len()) {
                            let open_head = super::aliases::unfold_alias_frontier_for_comparison_with_binder_lookup(
                                ty.as_type(),
                                &alias_ctx,
                                &expected_binders,
                                ty.identity_is_canonical(),
                            )
                            .map_or_else(
                                || matches!(ty.as_type(), Type::Goal { .. } | Type::Infer { .. }),
                                |(head, _)| matches!(head, Type::Goal { .. } | Type::Infer { .. }),
                            );
                            if open_head {
                                return Ok(None);
                            }
                        }
                        selected_slots.push((ty, selected_alpha_edges.len()));
                    }
                    expected_layer = *ret;
                    expected_layer_canonical = canonical;
                }
            }
        }

        if !saw_value_group {
            let expected_binders = ExpectedAlphaBinderLookup {
                lexical: &lexical,
                selected: &selected_expected_binders,
            };
            let (unfolded, canonical, original) = take_expected_frontier(
                expected_layer,
                &alias_ctx,
                &expected_binders,
                expected_layer_canonical,
            );
            if matches!(&unfolded, Type::Goal { .. } | Type::Infer { .. }) {
                return Ok(None);
            }
            let (param, ret) = match unfolded {
                Type::Function { param, ret, .. } => (param, ret),
                found => {
                    return Err(Error::type_(
                        fn_span,
                        format!(
                            "lambda expression used where a function type was expected, but the expected type is `{}`",
                            super::display_type(original.as_ref().unwrap_or(&found))
                        ),
                    ));
                }
            };
            let (param, _) = super::aliases::unfold_and_qualify_state_with_binder_lookup(
                &param,
                &alias_ctx,
                &expected_binders,
                canonical,
            );
            if matches!(&param, Type::Goal { .. } | Type::Infer { .. }) {
                return Ok(None);
            }
            if !matches!(&param, Type::Unit { .. }) {
                return Err(Error::type_(
                    fn_span,
                    format!(
                        "`fn` has no value parameters but the expected function takes `{}`",
                        super::display_type(&param)
                    ),
                ));
            }
            expected_layer = *ret;
            expected_layer_canonical = canonical;
        }

        assert_eq!(selected_slots.len(), self.value_slots.len());
        assert_eq!(selected_alpha_edges.len(), type_param_count);
        let expected_layer =
            InternedType::fresh_with_identity(expected_layer, expected_layer_canonical);
        // Preserve the established expected-shape/arity precedence. Planning
        // records the new source admissibility error, but checking reports it
        // only after the complete header shape is known and before any owner,
        // binder, goal, or publication effect.
        self.require_admitted_in_source_order()?;
        for (slot, (selected, edge_count)) in self.value_slots.iter_mut().zip(&selected_slots) {
            slot.ty = Some(selected.clone());
            slot.expected_alpha_edge_count = *edge_count;
        }
        self.alpha_edges = Some(selected_alpha_edges);
        self.alpha_edges_bound = false;
        self.body_expected_alpha_edge_count = type_param_count;
        self.body_expected = Some(expected_layer.clone());
        Ok(Some((
            expected_layer,
            selected_slots
                .into_iter()
                .map(|(selected, _)| selected)
                .collect(),
        )))
    }

    /// Bind the pure plan's exact expected/source alpha edges to the one set
    /// of source binder proofs minted by the selected client, then perform the
    /// mechanical capture-avoiding spelling conversion. Pure selection keeps
    /// the expected operands untouched; this is the first boundary allowed to
    /// construct source-aligned types for ordinary downstream checking.
    pub(crate) fn bind_selected_alpha_edges(
        &mut self,
        retained_binders: &[super::env::RetainedTypeBinder<'_>],
    ) -> SelectedLambdaCheck {
        assert!(
            !self.alpha_edges_bound,
            "a lambda plan bound its expected/source alpha edges more than once"
        );
        let edges = self
            .alpha_edges
            .as_mut()
            .expect("only a checked lambda plan can bind expected/source alpha edges");
        assert_eq!(
            edges.len(),
            retained_binders.len(),
            "a checked lambda plan received the wrong source binder proof count"
        );
        for (index, (edge, binder)) in edges.iter_mut().zip(retained_binders).enumerate() {
            assert_eq!(
                edge.source,
                PlannedBinderId(index),
                "a checked lambda plan reordered its planned source binder identities"
            );
            assert_eq!(
                edge.source_name,
                binder.param().name,
                "a checked lambda plan rebound an alpha edge to another source name"
            );
            assert_eq!(
                edge.source_binder_span,
                binder.param().span,
                "a checked lambda plan rebound an alpha edge to another source declaration"
            );
            assert_eq!(
                edge.kind,
                binder.param().effective_kind(),
                "a checked lambda plan rebound an alpha edge to another source kind"
            );
            assert!(
                !edge.expected_name.is_empty(),
                "an expected binder at {:?} lost its selected name",
                edge.expected_binder_span
            );
            edge.source_binder_id = Some(binder.id());
        }

        let edges = self
            .alpha_edges
            .as_ref()
            .expect("a checked lambda plan lost its bound alpha edges");
        let mut materializer = ExpectedAlphaMaterializer::new();
        let mut active_edge_count = 0usize;
        let mut selected_slots = Vec::with_capacity(self.value_slots.len());
        for slot in &mut self.value_slots {
            let selected = slot
                .ty
                .take()
                .expect("a checked lambda plan lost an expected value slot");
            let edge_count = slot.expected_alpha_edge_count;
            assert!(
                edge_count <= edges.len(),
                "an expected value slot named an unknown alpha-edge prefix"
            );
            assert!(
                active_edge_count <= edge_count,
                "expected value slots moved backward through the alpha-edge prefix"
            );
            materializer.advance_edges(&edges[active_edge_count..edge_count]);
            active_edge_count = edge_count;
            let selected = if edge_count == 0 {
                selected
            } else {
                materialize_expected_alpha(selected, &mut materializer)
            };
            slot.ty = Some(selected.clone());
            selected_slots.push(selected);
        }
        let body_expected = self
            .body_expected
            .take()
            .expect("a checked lambda plan lost its expected body type");
        assert!(
            self.body_expected_alpha_edge_count <= edges.len(),
            "an expected body named an unknown alpha-edge prefix"
        );
        assert!(
            active_edge_count <= self.body_expected_alpha_edge_count,
            "the expected body moved backward through the alpha-edge prefix"
        );
        materializer.advance_edges(&edges[active_edge_count..self.body_expected_alpha_edge_count]);
        active_edge_count = self.body_expected_alpha_edge_count;
        assert_eq!(
            active_edge_count,
            edges.len(),
            "the expected body did not consume every selected alpha edge"
        );
        let body_expected = if active_edge_count == 0 {
            body_expected
        } else {
            materialize_expected_alpha(body_expected, &mut materializer)
        };
        self.body_expected = Some(body_expected.clone());
        self.alpha_edges_bound = true;
        (body_expected, selected_slots)
    }

    pub(crate) fn select_synth(
        &mut self,
        diagnose: bool,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Option<SelectedLambdaSynth>, Error> {
        assert!(
            self.body_expected.is_none(),
            "a checked lambda plan cannot select standalone synthesis"
        );
        let mut value_param_types = Vec::with_capacity(self.value_slots.len());
        for slot in &mut self.value_slots {
            let ty = match &slot.annotation {
                HeaderAnnotationState::Missing => {
                    if diagnose {
                        return Err(missing_synth_parameter(
                            slot.param.name.as_str(),
                            slot.param.meta.span,
                        ));
                    }
                    return Ok(None);
                }
                HeaderAnnotationState::Planned(planned) => {
                    planned.plan.require_admitted()?;
                    if let Some(hole) = planned.plan.holes().first() {
                        if diagnose {
                            return Err(open_synth_parameter(
                                slot.param.name.as_str(),
                                planned.written,
                                hole,
                            ));
                        }
                        return Ok(None);
                    }
                    planned.plan.validated_type(planned.written, tcx)
                }
                HeaderAnnotationState::Reserved(_)
                | HeaderAnnotationState::Staged
                | HeaderAnnotationState::Committed(_) => {
                    unreachable!("a pure lambda plan already committed an annotation")
                }
            };
            slot.ty = Some(ty.clone());
            value_param_types.push(ty);
        }

        let annotated_return = match &self.return_annotation {
            HeaderAnnotationState::Missing => None,
            HeaderAnnotationState::Planned(planned)
                if matches!(planned.written, Type::Infer { .. }) =>
            {
                None
            }
            HeaderAnnotationState::Planned(planned) => {
                planned.plan.require_admitted()?;
                if let Some(hole) = planned.plan.holes().first() {
                    if diagnose {
                        return Err(open_synth_return(hole));
                    }
                    return Ok(None);
                }
                Some(planned.plan.validated_type(planned.written, tcx))
            }
            HeaderAnnotationState::Reserved(_)
            | HeaderAnnotationState::Staged
            | HeaderAnnotationState::Committed(_) => {
                unreachable!("a pure lambda plan already committed an annotation")
            }
        };
        Ok(Some((value_param_types, annotated_return)))
    }

    /// Reserve exactly the annotations whose written positions share one
    /// lexical signature prefix. The caller builds the retained owner chain in
    /// source order, so a later signature binder can never enter an earlier
    /// annotation goal's authority.
    pub(crate) fn reserve_annotation_slice(
        &mut self,
        value_range: std::ops::Range<usize>,
        include_return: bool,
        store: &mut GoalStore,
        owner: TypeGoalOwner,
        scope: &super::RigidScope,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        assert!(
            self.alpha_edges.is_none() || self.alpha_edges_bound,
            "a selected checked header reached owner reservation before binding its exact alpha edges"
        );
        let slots = self
            .value_slots
            .get_mut(value_range)
            .expect("a lambda lexical slice named an invalid value range");
        for slot in slots {
            reserve_header_annotation(&mut slot.annotation, store, owner, scope, tcx)?;
        }
        if include_return {
            reserve_header_annotation(&mut self.return_annotation, store, owner, scope, tcx)?;
        }
        Ok(())
    }

    pub(crate) fn constrain_reserved_value(
        &mut self,
        value_index: usize,
        expected: ScopedType,
        store: &GoalStore,
        delta: &mut super::goals::GoalDelta,
        publication: &mut crate::pass::typecheck_full::publication::PublicationBuilder,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        let slot = self
            .value_slots
            .get_mut(value_index)
            .expect("a retained lambda plan received an unknown value slot");
        constrain_reserved_header_annotation(
            &mut slot.annotation,
            expected,
            store,
            delta,
            publication,
            tcx,
        )
    }

    pub(crate) fn constrain_reserved_return(
        &mut self,
        expected: ScopedType,
        store: &GoalStore,
        delta: &mut super::goals::GoalDelta,
        publication: &mut crate::pass::typecheck_full::publication::PublicationBuilder,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        constrain_reserved_header_annotation(
            &mut self.return_annotation,
            expected,
            store,
            delta,
            publication,
            tcx,
        )
    }

    fn check_body(
        mut self,
        body: &'m crate::ast::Expr<Lowered>,
        fn_site: crate::ast::ExpressionSite,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        assert_eq!(
            self.value_slots.len(),
            self.signature.value_param_count(),
            "lambda plan lost a value-parameter slot"
        );
        let binder_mark = tcx.save();
        let retained_binders = self
            .signature
            .params
            .iter()
            .filter_map(|entry| match entry {
                crate::ast::SignatureParam::Type(param) => {
                    Some(tcx.push_retained_type_param(param))
                }
                crate::ast::SignatureParam::Value(_) => None,
            })
            .collect::<Vec<_>>();
        tcx.restore(binder_mark);
        let _ = self.bind_selected_alpha_edges(&retained_binders);
        let body_expected = self
            .body_expected
            .clone()
            .expect("a checked lambda plan retains its expected body type");
        let mark = tcx.save();
        let alpha_edges = self
            .alpha_edges
            .take()
            .expect("a checked lambda plan retains its expected/source alpha edges");
        let checked = (|| {
            let mut slots = self.value_slots.iter_mut();
            let mut alpha_edges = alpha_edges.iter();
            let mut retained = retained_binders.iter();
            for entry in &self.signature.params {
                match entry {
                    crate::ast::SignatureParam::Type(param) => {
                        let edge = alpha_edges
                            .next()
                            .expect("lambda plan lost an expected/source alpha edge");
                        assert_eq!(
                            edge.source_binder_span, param.span,
                            "lambda plan rebound an alpha edge to another source binder"
                        );
                        let binder = retained
                            .next()
                            .expect("lambda plan lost a retained source binder");
                        assert_eq!(
                            edge.source_binder_id,
                            Some(binder.id()),
                            "lambda plan rebound an alpha edge to another source proof"
                        );
                        tcx.push_retained_type_binder(binder);
                    }
                    crate::ast::SignatureParam::Value(param) => {
                        let slot = slots
                            .next()
                            .expect("lambda plan lost a value-parameter slot");
                        let expected = slot
                            .ty
                            .as_ref()
                            .expect("a checked lambda plan retains every expected value slot");
                        assert_eq!(
                            slot.param.meta.span, param.meta.span,
                            "lambda plan reordered value-parameter slots"
                        );
                        let annotation =
                            std::mem::replace(&mut slot.annotation, HeaderAnnotationState::Missing);
                        slot.annotation = match annotation {
                            HeaderAnnotationState::Missing => HeaderAnnotationState::Missing,
                            HeaderAnnotationState::Planned(planned) => {
                                let committed = planned.plan.commit_against_deferred(
                                    planned.written,
                                    expected,
                                    param.meta.span,
                                    tcx,
                                )?;
                                let _resolved = committed.resolved;
                                HeaderAnnotationState::Committed(committed.publication)
                            }
                            HeaderAnnotationState::Reserved(_)
                            | HeaderAnnotationState::Staged
                            | HeaderAnnotationState::Committed(_) => {
                                unreachable!("a lambda annotation was committed twice")
                            }
                        };
                        tcx.push_value_interned(param.name.clone(), expected.clone());
                        tcx.attach_decl_span_to_local(&param.name, param.meta.span);
                    }
                }
            }
            debug_assert!(slots.next().is_none());
            debug_assert!(alpha_edges.next().is_none());
            debug_assert!(retained.next().is_none());

            let return_annotation =
                std::mem::replace(&mut self.return_annotation, HeaderAnnotationState::Missing);
            self.return_annotation = match return_annotation {
                HeaderAnnotationState::Missing => HeaderAnnotationState::Missing,
                HeaderAnnotationState::Planned(planned) => {
                    let committed = planned.plan.commit_against_deferred(
                        planned.written,
                        &body_expected,
                        planned.written.span(),
                        tcx,
                    )?;
                    let _resolved = committed.resolved;
                    HeaderAnnotationState::Committed(committed.publication)
                }
                HeaderAnnotationState::Reserved(_)
                | HeaderAnnotationState::Staged
                | HeaderAnnotationState::Committed(_) => {
                    unreachable!("a lambda return annotation was committed twice")
                }
            };

            #[cfg(test)]
            super::apply::record_lambda_body_entry(fn_site.span, tcx);
            <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::check_value_against_interned(
                body,
                &body_expected,
                tcx,
            )
        })();
        tcx.restore(mark);
        checked?;

        let publication_mark = tcx.save();
        let mut slots = self.value_slots.into_iter();
        let mut retained = retained_binders.iter();
        let mut value_index = 0usize;
        for entry in &self.signature.params {
            match entry {
                crate::ast::SignatureParam::Type(param) => {
                    let binder = retained
                        .next()
                        .expect("lambda publication lost a retained source binder");
                    assert_eq!(binder.param().span, param.span);
                    tcx.push_retained_type_binder(binder);
                    <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_binder_decl_at(
                        param.span,
                        super::ResolvedBinderKind::TypeParam,
                        param.name.as_str(),
                        tcx,
                    );
                }
                crate::ast::SignatureParam::Value(param) => {
                    let slot = slots
                        .next()
                        .expect("lambda publication lost a value-parameter slot");
                    let ty = slot
                        .ty
                        .expect("a checked lambda publication lost its expected value slot");
                    tcx.push_value_interned(param.name.clone(), ty.clone());
                    <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_binder_decl_at(
                        param.meta.span,
                        super::ResolvedBinderKind::Local,
                        param.name.as_str(),
                        tcx,
                    );
                    match slot.annotation {
                        HeaderAnnotationState::Missing => {}
                        HeaderAnnotationState::Committed(publication) => publication.publish(tcx),
                        HeaderAnnotationState::Planned(_)
                        | HeaderAnnotationState::Reserved(_)
                        | HeaderAnnotationState::Staged => {
                            unreachable!("a planned annotation reached body-success publication")
                        }
                    }
                    <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_fn_param_type(
                        fn_site,
                        value_index,
                        ty,
                        tcx,
                    );
                    value_index += 1;
                }
            }
        }
        debug_assert!(slots.next().is_none());
        debug_assert!(retained.next().is_none());
        match self.return_annotation {
            HeaderAnnotationState::Missing => {}
            HeaderAnnotationState::Committed(publication) => publication.publish(tcx),
            HeaderAnnotationState::Planned(_)
            | HeaderAnnotationState::Reserved(_)
            | HeaderAnnotationState::Staged => {
                unreachable!("a planned return annotation reached body-success publication")
            }
        }
        tcx.restore(publication_mark);
        Ok(())
    }

    fn synth_body(
        mut self,
        full: &'m crate::ast::Expr<Lowered>,
        body: &'m crate::ast::Expr<Lowered>,
        _fn_span: Span,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<super::Synth<Lowered>, Error> {
        assert!(
            self.body_expected.is_none(),
            "a synthesis lambda plan must not retain an enclosing expected type"
        );
        let mark = tcx.save();
        assert!(
            self.alpha_edges.take().is_none(),
            "a synthesis lambda plan retained expected/source alpha edges"
        );
        let type_param_count = self
            .signature
            .params
            .iter()
            .filter(|param| matches!(param, crate::ast::SignatureParam::Type(_)))
            .count();
        let mut retained_binders = Vec::with_capacity(type_param_count);
        let synthesized = (|| {
            let value_param_count = self.value_slots.len();
            let mut slots = self.value_slots.iter_mut();
            let mut value_param_types = Vec::with_capacity(value_param_count);
            for entry in &self.signature.params {
                match entry {
                    crate::ast::SignatureParam::Type(param) => {
                        retained_binders.push(tcx.push_retained_type_param(param));
                    }
                    crate::ast::SignatureParam::Value(param) => {
                        let slot = slots
                            .next()
                            .expect("lambda plan lost a value-parameter slot");
                        assert_eq!(
                            slot.param.meta.span, param.meta.span,
                            "lambda plan reordered value-parameter slots"
                        );
                        assert!(
                            slot.ty.is_none(),
                            "a synthesis lambda plan retained an expected value slot"
                        );
                        let annotation =
                            std::mem::replace(&mut slot.annotation, HeaderAnnotationState::Missing);
                        let (ty, annotation) = match annotation {
                            HeaderAnnotationState::Missing => {
                                return Err(missing_synth_parameter(
                                    param.name.as_str(),
                                    param.meta.span,
                                ));
                            }
                            HeaderAnnotationState::Planned(planned) => {
                                planned.plan.require_admitted()?;
                                if let Some(hole) = planned.plan.holes().first() {
                                    return Err(open_synth_parameter(
                                        param.name.as_str(),
                                        planned.written,
                                        hole,
                                    ));
                                }
                                let (ty, publication) = planned
                                    .plan
                                    .into_validated_type_and_publication(planned.written, tcx);
                                (ty, HeaderAnnotationState::Committed(publication))
                            }
                            HeaderAnnotationState::Reserved(_)
                            | HeaderAnnotationState::Staged
                            | HeaderAnnotationState::Committed(_) => {
                                unreachable!("a lambda annotation was committed twice")
                            }
                        };
                        slot.ty = Some(ty.clone());
                        slot.annotation = annotation;
                        tcx.push_value_interned(param.name.clone(), ty.clone());
                        tcx.attach_decl_span_to_local(&param.name, param.meta.span);
                        value_param_types.push(ty);
                    }
                }
            }
            debug_assert!(slots.next().is_none());

            let return_annotation =
                std::mem::replace(&mut self.return_annotation, HeaderAnnotationState::Missing);
            let (body_ty, return_annotation) = match return_annotation {
                HeaderAnnotationState::Missing => {
                    #[cfg(test)]
                    super::apply::record_lambda_body_entry(full.span(), tcx);
                    let body_ty = super::synth::synth_value_type(
                        <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::synth_expr(
                            body, tcx,
                        )?,
                    )?;
                    (body_ty, HeaderAnnotationState::Missing)
                }
                HeaderAnnotationState::Planned(planned)
                    if matches!(planned.written, Type::Infer { .. }) =>
                {
                    #[cfg(test)]
                    super::apply::record_lambda_body_entry(full.span(), tcx);
                    let body_ty = super::synth::synth_value_type(
                        <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::synth_expr(
                            body, tcx,
                        )?,
                    )?;
                    let committed = planned.plan.commit_against_deferred(
                        planned.written,
                        &body_ty,
                        planned.written.span(),
                        tcx,
                    )?;
                    (
                        committed.resolved,
                        HeaderAnnotationState::Committed(committed.publication),
                    )
                }
                HeaderAnnotationState::Planned(planned) => {
                    planned.plan.require_admitted()?;
                    if let Some(hole) = planned.plan.holes().first() {
                        return Err(open_synth_return(hole));
                    }
                    let (expected, publication) = planned
                        .plan
                        .into_validated_type_and_publication(planned.written, tcx);
                    #[cfg(test)]
                    super::apply::record_lambda_body_entry(full.span(), tcx);
                    <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::check_value_against_interned(
                        body,
                        &expected,
                        tcx,
                    )?;
                    (expected, HeaderAnnotationState::Committed(publication))
                }
                HeaderAnnotationState::Reserved(_)
                | HeaderAnnotationState::Staged
                | HeaderAnnotationState::Committed(_) => {
                    unreachable!("a lambda return annotation was committed twice")
                }
            };
            self.return_annotation = return_annotation;
            Ok((body_ty, value_param_types))
        })();
        tcx.restore(mark);
        let (body_ty, value_param_types) = synthesized?;

        let publication_mark = tcx.save();
        let mut slots = self.value_slots.into_iter();
        let mut retained = retained_binders.iter();
        for entry in &self.signature.params {
            match entry {
                crate::ast::SignatureParam::Type(param) => {
                    let binder = retained
                        .next()
                        .expect("lambda publication lost a retained source binder");
                    assert_eq!(binder.param().span, param.span);
                    tcx.push_retained_type_binder(binder);
                    <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_binder_decl_at(
                        param.span,
                        super::ResolvedBinderKind::TypeParam,
                        param.name.as_str(),
                        tcx,
                    );
                }
                crate::ast::SignatureParam::Value(param) => {
                    let slot = slots
                        .next()
                        .expect("lambda publication lost a value-parameter slot");
                    let ty = slot
                        .ty
                        .expect("a synthesized lambda lost its value-parameter type");
                    tcx.push_value_interned(param.name.clone(), ty);
                    <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_binder_decl_at(
                        param.meta.span,
                        super::ResolvedBinderKind::Local,
                        param.name.as_str(),
                        tcx,
                    );
                    match slot.annotation {
                        HeaderAnnotationState::Committed(publication) => publication.publish(tcx),
                        HeaderAnnotationState::Missing
                        | HeaderAnnotationState::Planned(_)
                        | HeaderAnnotationState::Reserved(_)
                        | HeaderAnnotationState::Staged => {
                            unreachable!("an unresolved synthesized parameter reached publication")
                        }
                    }
                }
            }
        }
        debug_assert!(slots.next().is_none());
        debug_assert!(retained.next().is_none());
        match self.return_annotation {
            HeaderAnnotationState::Missing => {}
            HeaderAnnotationState::Committed(publication) => publication.publish(tcx),
            HeaderAnnotationState::Planned(_)
            | HeaderAnnotationState::Reserved(_)
            | HeaderAnnotationState::Staged => {
                unreachable!("a planned return annotation reached body-success publication")
            }
        }
        tcx.restore(publication_mark);

        Ok(super::synth::finish_synth_fn_from_body_type(
            full,
            self.signature,
            body_ty,
            value_param_types,
            tcx,
        ))
    }
}

#[cfg(feature = "surface")]
fn reserve_header_annotation<'m>(
    state: &mut HeaderAnnotationState<'m>,
    store: &mut GoalStore,
    owner: TypeGoalOwner,
    scope: &super::RigidScope,
    tcx: &TypeCtx<'m, '_, Lowered>,
) -> Result<(), Error> {
    let current = std::mem::replace(state, HeaderAnnotationState::Missing);
    *state = match current {
        HeaderAnnotationState::Missing => HeaderAnnotationState::Missing,
        HeaderAnnotationState::Planned(planned) => HeaderAnnotationState::Reserved(
            planned
                .plan
                .reserve_for_owner_at_scope(planned.written, store, owner, scope, tcx)?,
        ),
        HeaderAnnotationState::Reserved(_)
        | HeaderAnnotationState::Staged
        | HeaderAnnotationState::Committed(_) => {
            unreachable!("a lambda header reserved an annotation more than once")
        }
    };
    Ok(())
}

#[cfg(feature = "surface")]
fn constrain_reserved_header_annotation<'m>(
    state: &mut HeaderAnnotationState<'m>,
    expected: ScopedType,
    store: &GoalStore,
    delta: &mut super::goals::GoalDelta,
    publication: &mut crate::pass::typecheck_full::publication::PublicationBuilder,
    tcx: &mut TypeCtx<'m, '_, Lowered>,
) -> Result<(), Error> {
    let current = std::mem::replace(state, HeaderAnnotationState::Missing);
    *state = match current {
        HeaderAnnotationState::Missing => HeaderAnnotationState::Missing,
        HeaderAnnotationState::Reserved(reserved) => {
            let span = reserved.written.as_type().span();
            store.constrain_equation(
                delta,
                reserved.value,
                ExpectedEquationOperand::solver(expected),
                span,
                tcx,
            )?;
            stage_annotation_binder_events(reserved.binder_events, publication, tcx);
            HeaderAnnotationState::Staged
        }
        HeaderAnnotationState::Staged => HeaderAnnotationState::Staged,
        HeaderAnnotationState::Planned(_) | HeaderAnnotationState::Committed(_) => {
            unreachable!("a retained lambda constrained an unreserved annotation")
        }
    };
    Ok(())
}

#[cfg(feature = "surface")]
fn stage_annotation_binder_events<'m>(
    events: Vec<AnnotationBinderEvent>,
    publication: &mut crate::pass::typecheck_full::publication::PublicationBuilder,
    tcx: &mut TypeCtx<'m, '_, Lowered>,
) {
    let mut marks = Vec::new();
    for event in events {
        match event {
            AnnotationBinderEvent::Enter(param) => {
                let mark = tcx.save();
                tcx.push_owned_retained_type_param(&param);
                publication.record_local_decl(
                    param.name.clone(),
                    param.span,
                    super::ResolvedBinderKind::TypeParam,
                );
                marks.push(mark);
            }
            AnnotationBinderEvent::Exit => {
                let mark = marks
                    .pop()
                    .expect("annotation binder publication exited an unknown forall");
                tcx.restore(mark);
            }
            AnnotationBinderEvent::Reference { span, name, proof } => {
                let actual = tcx.locals.iter().rev().find_map(|local| match local {
                    Local::TypeParam {
                        name: bound,
                        decl_span,
                        binder_id,
                        ..
                    } if bound == &name => Some((*decl_span, *binder_id)),
                    _ => None,
                });
                match proof {
                    AnnotationBinderProof::Ambient(expected) => assert_eq!(
                        actual.map(|(_, binder)| binder),
                        Some(expected),
                        "annotation binder publication changed its ambient proof"
                    ),
                    AnnotationBinderProof::Source { declaration_span }
                    | AnnotationBinderProof::Embedded { declaration_span } => assert_eq!(
                        actual.map(|(span, _)| span),
                        Some(declaration_span),
                        "annotation binder publication changed its embedded proof"
                    ),
                }
                publication.record_position_binder(
                    span,
                    crate::pass::typecheck_full::resolved_binder_for_position(
                        super::ResolvedBinderKind::TypeParam,
                        name.as_str(),
                        None,
                        None,
                        tcx,
                    ),
                );
            }
        }
    }
    assert!(
        marks.is_empty(),
        "annotation binder publication left an embedded forall open"
    );
}

#[cfg(feature = "surface")]
pub(crate) fn check_lowered_fn_against<'m>(
    signature: &'m crate::ast::Signature<Lowered>,
    return_annotation: Option<&'m crate::ast::Type<Lowered>>,
    body: &'m crate::ast::Expr<Lowered>,
    expected: &InternedType<Lowered>,
    fn_site: crate::ast::ExpressionSite,
    tcx: &mut TypeCtx<'m, '_, Lowered>,
) -> Result<(), Error> {
    let fn_span = fn_site.span;
    let mut plan = plan_lowered_lambda_header(signature, return_annotation, tcx)?;
    #[cfg(test)]
    record_lowered_check_consumer();
    let Some(_) = plan.select_check(expected, fn_span, tcx)? else {
        unreachable!("a direct checked lambda received an open retained expectation")
    };
    plan.check_body(body, fn_site, tcx)
}

#[cfg(feature = "surface")]
pub(crate) fn synth_lowered_fn<'m>(
    full: &'m crate::ast::Expr<Lowered>,
    signature: &'m crate::ast::Signature<Lowered>,
    return_annotation: Option<&'m crate::ast::Type<Lowered>>,
    body: &'m crate::ast::Expr<Lowered>,
    fn_span: Span,
    tcx: &mut TypeCtx<'m, '_, Lowered>,
) -> Result<super::Synth<Lowered>, Error> {
    let plan = plan_lowered_lambda_header(signature, return_annotation, tcx)?;
    #[cfg(test)]
    record_lowered_synth_consumer();
    plan.synth_body(full, body, fn_span, tcx)
}

/// Synthesize one local binding through the shared source-annotation plan.
/// Complete annotations retain ordinary bidirectional checking; admitted
/// placeholders never flow backward into the RHS and are constrained only
/// after that RHS has synthesized.
#[cfg(feature = "surface")]
pub(crate) fn synth_let_bound_type<'m>(
    annotation: Option<&'m crate::ast::Type<Lowered>>,
    value: &'m crate::ast::Expr<Lowered>,
    tcx: &mut TypeCtx<'m, '_, Lowered>,
) -> Result<InternedType<Lowered>, Error> {
    let Some(annotation) = annotation else {
        return Ok(
            <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::synth_expr(value, tcx)?.ty,
        );
    };
    let lexical = PlanningLexicalView::borrowed(tcx);
    let plan = plan_source_annotation(annotation, tcx, &lexical, false)?;
    drop(lexical);
    plan.require_admitted()?;
    if plan.is_closed_borrowed() {
        let expected = tcx.intern_type(annotation);
        <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::check_value_against_interned(
            value, &expected, tcx,
        )?;
        plan.finish_closed(annotation, tcx);
        return Ok(expected);
    }

    let found =
        <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::synth_expr(value, tcx)?.ty;
    plan.commit_against(annotation, &found, annotation.span(), tcx)?;
    Ok(found)
}

#[cfg(feature = "surface")]
pub(crate) fn check_lowered_let_against<'m>(
    source: &'m crate::ast::Expr<Lowered>,
    expected: &InternedType<Lowered>,
    tcx: &mut TypeCtx<'m, '_, Lowered>,
) -> Result<(), Error> {
    let crate::ast::Expr::Let {
        name,
        name_span,
        ty: annotation,
        value,
        body,
        ..
    } = source
    else {
        unreachable!("Lowered let checking received a non-let expression")
    };
    let annotation = annotation.as_ref();
    let bound_ty = synth_let_bound_type(annotation, value, tcx)?;
    if super::synth::let_annotation_is_elided(annotation) {
        <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_inlay_let_type(
            *name_span,
            name,
            bound_ty.clone_type(),
            tcx,
        );
    }
    let mark = tcx.save();
    tcx.push_value_interned(name.to_owned(), bound_ty);
    <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_binder_decl_at(
        *name_span,
        super::ResolvedBinderKind::Local,
        name,
        tcx,
    );
    let checked =
        <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::check_value_against_interned(
            body, expected, tcx,
        );
    tcx.restore(mark);
    checked?;
    <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_position_type(
        source.span(),
        expected.clone(),
        tcx,
    );
    Ok(())
}

pub(crate) fn synth_closed_let_bound_type<'m, P>(
    annotation: Option<&'m crate::ast::Type<P>>,
    value: &'m crate::ast::Expr<P>,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<InternedType<P>, Error>
where
    P: TyperPhase + crate::pass::resolve::ExportContractPhase + Clone,
{
    let Some(annotation) = annotation else {
        return Ok(<P::Typer as super::Typer<P>>::synth_expr(value, tcx)?.ty);
    };
    let lexical = PlanningLexicalView::borrowed(tcx);
    let plan = plan_source_annotation(annotation, tcx, &lexical, false)?;
    drop(lexical);
    plan.require_admitted()?;
    assert!(
        plan.holes().is_empty() && plan.is_closed_borrowed() && plan.has_no_transport_state(),
        "Kio' admitted an open source let annotation"
    );
    let expected =
        InternedType::fresh_with_identity(plan.ty().clone(), plan.identity_is_canonical());
    <P::Typer as super::Typer<P>>::check_value_against_interned(value, &expected, tcx)?;
    Ok(expected)
}

pub(crate) fn synth_prime_let<'m>(
    name: &str,
    name_span: Span,
    annotation: Option<&'m crate::ast::Type<crate::ast::Prime>>,
    value: &'m crate::ast::Expr<crate::ast::Prime>,
    body: &'m crate::ast::Expr<crate::ast::Prime>,
    tcx: &mut TypeCtx<'m, '_, crate::ast::Prime>,
) -> Result<super::Synth<crate::ast::Prime>, Error> {
    let bound_ty = synth_closed_let_bound_type(annotation, value, tcx)?;
    if super::synth::let_annotation_is_elided(annotation) {
        <<crate::ast::Prime as TyperPhase>::Typer as super::Typer<crate::ast::Prime>>::record_inlay_let_type(
            name_span,
            name,
            bound_ty.clone_type(),
            tcx,
        );
    }
    let mark = tcx.save();
    tcx.push_value_interned(name.to_owned(), bound_ty);
    <<crate::ast::Prime as TyperPhase>::Typer as super::Typer<crate::ast::Prime>>::record_binder_decl_at(
        name_span,
        super::ResolvedBinderKind::Local,
        name,
        tcx,
    );
    let body_ty =
        <<crate::ast::Prime as TyperPhase>::Typer as super::Typer<crate::ast::Prime>>::synth_expr(
            body, tcx,
        );
    tcx.restore(mark);
    body_ty
}

pub(crate) fn check_prime_let_against<'m>(
    source: &'m crate::ast::Expr<crate::ast::Prime>,
    expected: &InternedType<crate::ast::Prime>,
    tcx: &mut TypeCtx<'m, '_, crate::ast::Prime>,
) -> Result<(), Error> {
    let crate::ast::Expr::Let {
        name,
        name_span,
        ty: annotation,
        value,
        body,
        ..
    } = source
    else {
        unreachable!("Prime let checking received a non-let expression")
    };
    let annotation = annotation.as_ref();
    let bound_ty = synth_closed_let_bound_type(annotation, value, tcx)?;
    if super::synth::let_annotation_is_elided(annotation) {
        <<crate::ast::Prime as TyperPhase>::Typer as super::Typer<crate::ast::Prime>>::record_inlay_let_type(
            *name_span,
            name,
            bound_ty.clone_type(),
            tcx,
        );
    }
    let mark = tcx.save();
    tcx.push_value_interned(name.to_owned(), bound_ty);
    <<crate::ast::Prime as TyperPhase>::Typer as super::Typer<crate::ast::Prime>>::record_binder_decl_at(
        *name_span,
        super::ResolvedBinderKind::Local,
        name,
        tcx,
    );
    let checked = <<crate::ast::Prime as TyperPhase>::Typer as super::Typer<
        crate::ast::Prime,
    >>::check_value_against_interned(body, expected, tcx);
    tcx.restore(mark);
    checked?;
    <<crate::ast::Prime as TyperPhase>::Typer as super::Typer<crate::ast::Prime>>::record_position_type(
        source.span(),
        expected.clone(),
        tcx,
    );
    Ok(())
}

#[cfg(feature = "surface")]
pub(crate) fn synth_let<'m>(
    name: &str,
    name_span: Span,
    annotation: Option<&'m crate::ast::Type<Lowered>>,
    value: &'m crate::ast::Expr<Lowered>,
    body: &'m crate::ast::Expr<Lowered>,
    tcx: &mut TypeCtx<'m, '_, Lowered>,
) -> Result<super::Synth<Lowered>, Error> {
    let bound_ty = synth_let_bound_type(annotation, value, tcx)?;
    if super::synth::let_annotation_is_elided(annotation) {
        <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_inlay_let_type(
            name_span,
            name,
            bound_ty.clone_type(),
            tcx,
        );
    }
    let mark = tcx.save();
    tcx.push_value_interned(name.to_owned(), bound_ty);
    <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::record_binder_decl_at(
        name_span,
        super::ResolvedBinderKind::Local,
        name,
        tcx,
    );
    let result = <<Lowered as TyperPhase>::Typer as super::Typer<Lowered>>::synth_expr(body, tcx);
    tcx.restore(mark);
    result
}

#[cfg(feature = "surface")]
fn publish_annotation_binder_events<'m, P>(
    events: Vec<AnnotationBinderEvent>,
    tcx: &mut TypeCtx<'m, '_, P>,
) where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let mut marks = Vec::new();
    for event in events {
        match event {
            AnnotationBinderEvent::Enter(param) => {
                let mark = tcx.save();
                tcx.push_owned_retained_type_param(&param);
                <P::Typer as super::Typer<P>>::record_binder_decl_at(
                    param.span,
                    super::ResolvedBinderKind::TypeParam,
                    param.name.as_str(),
                    tcx,
                );
                marks.push(mark);
            }
            AnnotationBinderEvent::Exit => {
                let mark = marks
                    .pop()
                    .expect("annotation binder publication exited an unknown forall");
                tcx.restore(mark);
            }
            AnnotationBinderEvent::Reference { span, name, proof } => {
                let actual = tcx.locals.iter().rev().find_map(|local| match local {
                    Local::TypeParam {
                        name: bound,
                        decl_span,
                        binder_id,
                        ..
                    } if bound == &name => Some((*decl_span, *binder_id)),
                    _ => None,
                });
                match proof {
                    AnnotationBinderProof::Ambient(expected) => assert_eq!(
                        actual.map(|(_, binder)| binder),
                        Some(expected),
                        "annotation binder publication changed its ambient proof"
                    ),
                    AnnotationBinderProof::Source { declaration_span }
                    | AnnotationBinderProof::Embedded { declaration_span } => assert_eq!(
                        actual.map(|(span, _)| span),
                        Some(declaration_span),
                        "annotation binder publication changed its embedded proof"
                    ),
                }
                <P::Typer as super::Typer<P>>::record_binder_at(
                    span,
                    super::ResolvedBinderKind::TypeParam,
                    name.as_str(),
                    None,
                    None,
                    tcx,
                );
            }
        }
    }
    assert!(
        marks.is_empty(),
        "annotation binder publication left an embedded forall open"
    );
}

struct AnnotationInferPolicy<'p, 'a, 'm, P>
where
    P: TyperPhase,
{
    holes: Vec<(Span, Kind)>,
    lexical: &'p PlanningLexicalView<'a, 'm, P>,
    embedded: Vec<(String, Span)>,
    binder_events: Vec<AnnotationBinderEvent>,
}

impl<P> super::types::InferPolicy for AnnotationInferPolicy<'_, '_, '_, P>
where
    P: TyperPhase,
{
    fn observe(&mut self, span: Span, required: Kind) -> Result<(), Error> {
        self.holes.push((span, required));
        Ok(())
    }

    fn defers_source_placeholder_scheme_error(&self) -> bool {
        true
    }

    fn enter_forall(&mut self, param: &crate::ast::TypeParam) {
        self.embedded.push((param.name.clone(), param.span));
        self.binder_events
            .push(AnnotationBinderEvent::Enter(param.clone()));
    }

    fn exit_forall(&mut self) {
        self.embedded
            .pop()
            .expect("the annotation kind walk exited an unknown forall binder");
        self.binder_events.push(AnnotationBinderEvent::Exit);
    }

    fn observe_binder_reference(&mut self, span: Span, name: &str) {
        let proof = self
            .embedded
            .iter()
            .rev()
            .find_map(|(bound, declaration_span)| {
                (bound == name).then_some(AnnotationBinderProof::Embedded {
                    declaration_span: *declaration_span,
                })
            })
            .unwrap_or_else(|| {
                self.lexical
                    .lookup(name)
                    .unwrap_or_else(|| {
                        panic!("the annotation kind walk selected missing ambient binder `{name}`")
                    })
                    .proof()
                    .clone()
            });
        self.binder_events.push(AnnotationBinderEvent::Reference {
            span,
            name: name.to_owned(),
            proof,
        });
    }
}

/// Build one allocation-adaptive annotation plan without mutating `TypeCtx`
/// or allocating a GoalStore owner. Kind/name/visibility/arity diagnostics run
/// first through the authoritative kind walk. Only a type that actually
/// contains a source placeholder invokes the proof-recording alias policy.
pub(crate) fn plan_source_annotation<'t, 'a, 'm, P>(
    ty: &'t crate::ast::Type<P>,
    tcx: &TypeCtx<'m, '_, P>,
    lexical: &PlanningLexicalView<'a, 'm, P>,
    identity_canonical: bool,
) -> Result<AnnotationPlan<'t, P>, Error>
where
    P: TyperPhase + crate::pass::resolve::ExportContractPhase + Clone,
    'm: 't,
{
    #[cfg(test)]
    update_annotation_plan_work(|work| work.classifications += 1);
    let mut infer = AnnotationInferPolicy {
        holes: Vec::new(),
        lexical,
        embedded: Vec::new(),
        binder_events: Vec::new(),
    };
    super::types::check_annotation_kinds_with_policy_and_binders(ty, tcx, lexical, &mut infer)?;
    if infer.holes.is_empty() {
        return Ok(AnnotationPlan {
            ty: PlannedAnnotationType::Borrowed(ty),
            identity_canonical,
            holes: Vec::new(),
            transport: None,
            binder_events: Vec::new(),
            rejection: None,
        });
    }

    #[cfg(test)]
    update_annotation_plan_work(|work| work.transport_materializations += 1);
    let materialized =
        materialize_source_annotation(ty, &tcx.env.alias_ctx(), lexical, identity_canonical);
    let rejection =
        materialized
            .transport
            .first_forall_nested_infer()
            .map(|(_, source, binder)| {
                let error = Error::type_(
            source.span,
            "type placeholder `_` cannot appear beneath a `forall` inside an annotation",
        )
        .with_help(
            "write a concrete type beneath this binder, or make the whole annotation slot `_`",
        );
                Box::new(match &binder.provider_file {
                    Some(file) => error.with_secondary_in_file(
                        file,
                        binder.span,
                        "this annotation introduces the enclosing binder",
                    ),
                    None => error.with_secondary(
                        binder.span,
                        "this annotation introduces the enclosing binder",
                    ),
                })
            });
    #[cfg(test)]
    if rejection.is_some() {
        update_annotation_plan_work(|work| work.rejections += 1);
    }

    let expected_source_count = infer.holes.len();
    let infer_sources = materialized
        .transport
        .sources
        .iter()
        .enumerate()
        .filter(|(_, source)| source.is_infer)
        .map(|(index, source)| (AliasSourceOccurrenceId::from_index(index), source));
    let mut holes = Vec::new();
    let mut source_count = 0;
    for ((span, required_kind), (source_id, source)) in infer.holes.into_iter().zip(infer_sources) {
        source_count += 1;
        assert_eq!(
            span, source.span,
            "the kind walk and source transport disagreed on placeholder order"
        );
        if source.disposition.is_retained() {
            holes.push(AnnotationHole {
                source: source_id,
                span,
                required_kind,
            });
        }
    }
    assert_eq!(
        source_count, expected_source_count,
        "the kind walk and source transport classified different placeholder sets"
    );
    assert_eq!(
        source_count,
        materialized
            .transport
            .sources
            .iter()
            .filter(|source| source.is_infer)
            .count(),
        "the kind walk and source transport classified different placeholder sets"
    );

    Ok(AnnotationPlan {
        ty: PlannedAnnotationType::Materialized(materialized.ty),
        identity_canonical: materialized.identity_canonical,
        holes,
        transport: Some(materialized.transport),
        binder_events: infer.binder_events,
        rejection,
    })
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PlanningLexicalWork {
    promotions: usize,
    bindings_scanned: usize,
    persistent_roots_built: usize,
    lookups: usize,
}

#[cfg(test)]
thread_local! {
    static PLANNING_LEXICAL_WORK: std::cell::Cell<PlanningLexicalWork> =
        const { std::cell::Cell::new(PlanningLexicalWork {
            promotions: 0,
            bindings_scanned: 0,
            persistent_roots_built: 0,
            lookups: 0,
        }) };
}

#[cfg(test)]
fn update_planning_lexical_work(update: impl FnOnce(&mut PlanningLexicalWork)) {
    PLANNING_LEXICAL_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

#[cfg(all(test, feature = "surface"))]
fn reset_planning_lexical_work() {
    PLANNING_LEXICAL_WORK.with(|work| work.set(PlanningLexicalWork::default()));
}

#[cfg(all(test, feature = "surface"))]
fn planning_lexical_work() -> PlanningLexicalWork {
    PLANNING_LEXICAL_WORK.with(std::cell::Cell::get)
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::ast::{Meta, Param, Signature, SignatureGroup, TypeParam};
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::parse;
    use crate::pass::resolve::Package;
    use crate::pass::typecheck_full::Elaborations;
    use crate::pipeline::Pipeline;
    use crate::span::Span;

    use super::*;

    fn with_type_ctx(test: impl for<'m, 'e> FnOnce(&mut TypeCtx<'m, 'e, crate::ast::Lowered>)) {
        with_package_type_ctx(&[("main.kio", "module main;")], "main", test);
    }

    fn bind_selected_alpha<'a, 'm>(
        plan: &mut LambdaHeaderPlan<'a>,
        signature: &Signature<crate::ast::Lowered>,
        tcx: &mut TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> SelectedLambdaCheck {
        let mark = tcx.save();
        let retained = signature
            .params
            .iter()
            .filter_map(|entry| match entry {
                crate::ast::SignatureParam::Type(param) => {
                    Some(tcx.push_owned_retained_type_param(param))
                }
                crate::ast::SignatureParam::Value(_) => None,
            })
            .collect::<Vec<_>>();
        tcx.restore(mark);
        plan.bind_selected_alpha_edges(&retained)
    }

    fn with_package_type_ctx(
        sources: &[(&str, &str)],
        checking_module: &str,
        test: impl for<'m, 'e> FnOnce(&mut TypeCtx<'m, 'e, crate::ast::Lowered>),
    ) {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    parse(source).unwrap_or_else(|error| panic!("parse {path}: {error:?}")),
                )
            })
            .collect();
        let (modules, _) = FullPipeline::lower_package(parsed, None).expect("lower package");
        let package = Package::build(Path::new(""), modules, None).expect("build package");
        package.resolve_imports().expect("resolve imports");
        let module = &package
            .module(checking_module)
            .unwrap_or_else(|| panic!("{checking_module} module"))
            .module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        test(&mut tcx);
    }

    fn span(start: u32) -> Span {
        Span::new(start, start + 1)
    }

    fn infer(start: u32) -> Type<crate::ast::Lowered> {
        Type::Infer {
            meta: Meta::new(span(start)),
            ext: (),
        }
    }

    fn unit(start: u32) -> Type<crate::ast::Lowered> {
        Type::Unit {
            meta: Meta::new(span(start)),
        }
    }

    fn path(name: &str, start: u32) -> Type<crate::ast::Lowered> {
        Type::synth_path(vec![name.to_owned()], Vec::new(), span(start))
    }

    fn path_args(
        name: &str,
        args: Vec<Type<crate::ast::Lowered>>,
        start: u32,
    ) -> Type<crate::ast::Lowered> {
        Type::synth_path(vec![name.to_owned()], args, span(start))
    }

    fn forall(
        name: &str,
        binder_start: u32,
        body: Type<crate::ast::Lowered>,
    ) -> Type<crate::ast::Lowered> {
        Type::Forall {
            param: TypeParam {
                name: name.to_owned(),
                span: span(binder_start),
                kind: None,
            },
            body: Box::new(body),
            meta: Meta::new(span(binder_start)),
        }
    }

    fn product(
        left: Type<crate::ast::Lowered>,
        right: Type<crate::ast::Lowered>,
        start: u32,
    ) -> Type<crate::ast::Lowered> {
        Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(span(start)),
        }
    }

    fn function(
        param: Type<crate::ast::Lowered>,
        ret: Type<crate::ast::Lowered>,
        start: u32,
    ) -> Type<crate::ast::Lowered> {
        Type::Function {
            param: Box::new(param),
            ret: Box::new(ret),
            meta: Meta::new(span(start)),
            abi_arity: 1,
            caps: (),
        }
    }

    fn value_param(
        name: &str,
        ty: Option<Type<crate::ast::Lowered>>,
        start: u32,
    ) -> Param<crate::ast::Lowered> {
        Param {
            name: name.to_owned(),
            ty,
            pattern: (),
            meta: Meta::new(span(start)),
        }
    }

    #[test]
    fn borrowed_view_is_allocation_free_and_finds_the_innermost_shadow() {
        with_type_ctx(|tcx| {
            tcx.push_type_param_kinded("A", Kind::Star, Span::new(1, 2));
            tcx.push_type_param_kinded("A", Kind::arrow_chain(1), Span::new(3, 4));
            reset_planning_lexical_work();

            let view = PlanningLexicalView::borrowed(tcx);
            assert_eq!(
                view.lookup("A").map(|binding| binding.kind),
                Some(Kind::arrow_chain(1))
            );
            assert!(!view.is_promoted());
            assert_eq!(
                planning_lexical_work(),
                PlanningLexicalWork {
                    promotions: 0,
                    bindings_scanned: 0,
                    persistent_roots_built: 0,
                    lookups: 1,
                }
            );
        });
    }

    #[test]
    fn one_promotion_is_shared_by_all_sibling_views() {
        const DEPTH: usize = 128;
        const SIBLINGS: usize = 128;
        with_type_ctx(|tcx| {
            for index in 0..DEPTH {
                let name: &'static str = Box::leak(format!("T{index}").into_boxed_str());
                let start = u32::try_from(index).expect("test binder index fits in u32");
                tcx.push_type_param_kinded(name, Kind::Star, Span::new(start, start + 1));
            }
            reset_planning_lexical_work();

            let root = PlanningLexicalView::borrowed(tcx).into_promoted();
            for _ in 0..SIBLINGS {
                let sibling = root.clone();
                assert_eq!(
                    sibling.lookup("T127").map(|binding| &binding.kind),
                    Some(&Kind::Star)
                );
            }
            assert_eq!(planning_lexical_work().promotions, 1);
            assert_eq!(planning_lexical_work().bindings_scanned, DEPTH);
            assert_eq!(planning_lexical_work().persistent_roots_built, 1);
            assert_eq!(planning_lexical_work().lookups, SIBLINGS);
        });
    }

    #[test]
    fn promoted_signature_prefix_indexes_source_binders_once() {
        const DEPTH: usize = 64;
        const LOOKUPS: usize = 64;
        with_type_ctx(|tcx| {
            let params = (0..DEPTH)
                .map(|index| TypeParam {
                    name: format!("Source{index}"),
                    span: span(u32::try_from(index + 1).expect("test span fits in u32")),
                    kind: Some(Kind::Star),
                })
                .collect();
            let signature: Signature<Lowered> =
                Signature::from_groups(vec![SignatureGroup::Type(params)]);
            let ambient = PlanningLexicalView::borrowed(tcx).into_promoted();
            reset_planning_lexical_work();

            let mut view =
                PlanningLexicalView::with_promoted_signature_binders(ambient, &signature);
            view.advance_signature_prefix(signature.params.len());
            assert_eq!(
                super::super::types::KindBinderLookup::kind_of(&view, "Source0"),
                Some(Kind::Star),
                "a promoted movable plan must retain its active source binder kind",
            );
            for _ in 0..LOOKUPS {
                assert!(view.contains_alias_binder("Source0"));
            }
            let work = planning_lexical_work();
            assert_eq!(work.bindings_scanned, DEPTH);
            assert_eq!(work.lookups, LOOKUPS + 1);
        });
    }

    #[test]
    fn consecutive_expected_foralls_do_not_rewalk_every_remaining_suffix() {
        const DEPTH: usize = 64;
        with_type_ctx(|tcx| {
            let mut groups = Vec::with_capacity(DEPTH + 1);
            for index in 0..DEPTH {
                groups.push(SignatureGroup::Type(vec![TypeParam {
                    name: format!("Source{index}"),
                    span: span(u32::try_from(index * 2 + 1).expect("test span fits in u32")),
                    kind: None,
                }]));
            }
            groups.push(SignatureGroup::Value(vec![value_param(
                "value",
                Some(product(path("Source0", 500), path("Source63", 501), 499)),
                499,
            )]));
            let signature = Signature::from_groups(groups);
            let mut expected = Type::Function {
                param: Box::new(product(
                    path("Expected0", 700),
                    path("Expected63", 701),
                    699,
                )),
                ret: Box::new(path("Expected31", 702)),
                meta: Meta::new(span(699)),
                abi_arity: 1,
                caps: (),
            };
            for index in (0..DEPTH).rev() {
                expected = forall(
                    &format!("Expected{index}"),
                    u32::try_from(index * 2 + 800).expect("test span fits in u32"),
                    expected,
                );
            }
            let expected = tcx.intern_type(&expected);
            let mut plan = plan_lowered_lambda_header(&signature, None, tcx)
                .expect("plan the deep checked lambda header");

            reset_expected_alpha_materialization_work();
            let selected = plan
                .select_check(&expected, span(1200), tcx)
                .expect("select the deep checked lambda")
                .expect("the complete expected scheme selects checking");
            assert_eq!(selected.1.len(), 1);
            let Type::Product { left, right, .. } = selected.1[0].as_type() else {
                panic!("the selected value slot must retain its product shape")
            };
            let (
                Type::Path { segments: left, .. },
                Type::Path {
                    segments: right, ..
                },
            ) = (left.as_ref(), right.as_ref())
            else {
                panic!("the pure selected slot must retain exact expected binder names")
            };
            assert_eq!(left[0].as_str(), "Expected0");
            assert_eq!(right[0].as_str(), "Expected63");
            let Type::Path { segments, .. } = selected.0.as_type() else {
                panic!("the pure selected result must retain its expected binder")
            };
            assert_eq!(segments[0].as_str(), "Expected31");
            let planning_work = expected_alpha_materialization_work();
            assert_eq!(
                planning_work.calls, 0,
                "pure lambda selection must not substitute expected binders by spelling"
            );

            let selected = bind_selected_alpha(&mut plan, &signature, tcx);
            let Type::Product { left, right, .. } = selected.1[0].as_type() else {
                panic!("the selected value slot must retain its product shape")
            };
            let Type::Path {
                segments: left_segments,
                ..
            } = left.as_ref()
            else {
                panic!("the selected left slot must be the first source binder")
            };
            let Type::Path {
                segments: right_segments,
                ..
            } = right.as_ref()
            else {
                panic!("the selected right slot must be the final source binder")
            };
            assert_eq!(left_segments[0].as_str(), "Source0");
            assert_eq!(right_segments[0].as_str(), "Source63");
            let Type::Path {
                segments: result_segments,
                ..
            } = selected.0.as_type()
            else {
                panic!("the selected result must be the middle source binder")
            };
            assert_eq!(result_segments[0].as_str(), "Source31");

            let work = expected_alpha_materialization_work();
            assert_eq!(
                work.canonicalization_calls, 1,
                "a checked header must canonicalize its expected tree exactly once",
            );
            assert!(
                work.canonicalization_input_nodes + work.input_nodes <= DEPTH * 8,
                "expected-alpha preparation revisited {} canonicalization nodes across {} calls and {} materialization nodes across {} calls for depth {DEPTH}",
                work.canonicalization_input_nodes,
                work.canonicalization_calls,
                work.input_nodes,
                work.calls,
            );
        });
    }

    #[test]
    fn wide_expected_group_installs_one_shared_alpha_prefix() {
        const DEPTH: usize = 32;
        const WIDTH: usize = 64;
        with_type_ctx(|tcx| {
            let mut groups = Vec::with_capacity(DEPTH + 1);
            for index in 0..DEPTH {
                groups.push(SignatureGroup::Type(vec![TypeParam {
                    name: format!("Source{index}"),
                    span: span(u32::try_from(index + 1).expect("test span fits in u32")),
                    kind: None,
                }]));
            }
            groups.push(SignatureGroup::Value(
                (0..WIDTH)
                    .map(|index| {
                        value_param(
                            &format!("value{index}"),
                            None,
                            u32::try_from(index + 100).expect("test span fits in u32"),
                        )
                    })
                    .collect(),
            ));
            let signature = Signature::from_groups(groups);

            let mut expected_param = path("Expected0", 500);
            for index in (1..WIDTH).rev() {
                expected_param = product(
                    path(
                        "Expected0",
                        u32::try_from(index + 500).expect("test span fits in u32"),
                    ),
                    expected_param,
                    u32::try_from(index + 600).expect("test span fits in u32"),
                );
            }
            let mut expected = Type::Function {
                param: Box::new(expected_param),
                ret: Box::new(path("Expected31", 800)),
                meta: Meta::new(span(499)),
                abi_arity: WIDTH,
                caps: (),
            };
            for index in (0..DEPTH).rev() {
                expected = forall(
                    &format!("Expected{index}"),
                    u32::try_from(index + 900).expect("test span fits in u32"),
                    expected,
                );
            }
            let expected = tcx.intern_type(&expected);
            let mut plan = plan_lowered_lambda_header(&signature, None, tcx)
                .expect("plan the deep and wide checked lambda header");

            reset_expected_alpha_materialization_work();
            plan.select_check(&expected, span(1000), tcx)
                .expect("select the deep and wide checked lambda")
                .expect("the complete expected scheme selects checking");
            let selected = bind_selected_alpha(&mut plan, &signature, tcx);
            assert_eq!(selected.1.len(), WIDTH);
            for selected in &selected.1 {
                let Type::Path { segments, .. } = selected.as_type() else {
                    panic!("the selected value slot lost its binder path")
                };
                assert_eq!(segments[0].as_str(), "Source0");
            }
            let Type::Path { segments, .. } = selected.0.as_type() else {
                panic!("the selected result lost its binder path")
            };
            assert_eq!(segments[0].as_str(), "Source31");

            let work = expected_alpha_materialization_work();
            assert_eq!(
                work.alpha_edge_installs, DEPTH,
                "one shared alpha prefix must install each exact edge once across all sibling operands",
            );
        });
    }

    #[test]
    fn repeated_expected_binder_names_bind_to_the_exact_source_prefix() {
        with_type_ctx(|tcx| {
            let signature = Signature::from_groups(vec![
                SignatureGroup::Type(vec![TypeParam {
                    name: "Source0".to_owned(),
                    span: span(1),
                    kind: None,
                }]),
                SignatureGroup::Value(vec![value_param("first", Some(path("Source0", 3)), 3)]),
                SignatureGroup::Type(vec![TypeParam {
                    name: "Source1".to_owned(),
                    span: span(5),
                    kind: None,
                }]),
                SignatureGroup::Value(vec![value_param("second", Some(path("Source1", 7)), 7)]),
            ]);
            let expected = forall(
                "Expected",
                11,
                function(
                    path("Expected", 13),
                    forall(
                        "Expected",
                        17,
                        function(path("Expected", 19), path("Expected", 23), 18),
                    ),
                    12,
                ),
            );
            let expected = tcx.intern_type(&expected);
            let mut plan = plan_lowered_lambda_header(&signature, None, tcx)
                .expect("plan the interleaved checked lambda header");

            let selected = plan
                .select_check(&expected, span(29), tcx)
                .expect("select the interleaved checked lambda")
                .expect("the complete expected scheme selects checking");
            for selected in [&selected.1[0], &selected.1[1], &selected.0] {
                let Type::Path { segments, .. } = selected.as_type() else {
                    panic!("the pure operand must retain its expected binder spelling")
                };
                assert_eq!(segments[0].as_str(), "Expected");
            }

            let selected = bind_selected_alpha(&mut plan, &signature, tcx);
            let source_name = |ty: &InternedType<crate::ast::Lowered>| {
                let Type::Path { segments, .. } = ty.as_type() else {
                    panic!("the bound operand must be a source binder path")
                };
                segments[0].as_str().to_owned()
            };
            assert_eq!(source_name(&selected.1[0]), "Source0");
            assert_eq!(source_name(&selected.1[1]), "Source1");
            assert_eq!(source_name(&selected.0), "Source1");
        });
    }

    #[test]
    fn exact_alpha_materialization_renames_a_nested_target_spelling() {
        with_type_ctx(|tcx| {
            let signature = Signature::from_groups(vec![
                SignatureGroup::Type(vec![TypeParam {
                    name: "Inner".to_owned(),
                    span: span(1),
                    kind: None,
                }]),
                SignatureGroup::Value(vec![value_param("value", None, 3)]),
            ]);
            let nested = forall(
                "Inner",
                11,
                function(path("Expected", 13), path("Inner", 17), 12),
            );
            let expected = forall("Expected", 7, function(nested, path("Expected", 19), 8));
            let expected = tcx.intern_type(&expected);
            let mut plan = plan_lowered_lambda_header(&signature, None, tcx)
                .expect("plan the capture-avoiding checked lambda header");
            plan.select_check(&expected, span(23), tcx)
                .expect("select the capture-avoiding checked lambda")
                .expect("the complete expected scheme selects checking");

            let selected = bind_selected_alpha(&mut plan, &signature, tcx);
            let Type::Forall {
                param: nested,
                body,
                ..
            } = selected.1[0].as_type()
            else {
                panic!("the expected value slot must retain its nested forall")
            };
            assert_ne!(nested.name, "Inner");
            let Type::Function { param, ret, .. } = body.as_ref() else {
                panic!("the nested forall must retain its function body")
            };
            let (
                Type::Path {
                    segments: mapped, ..
                },
                Type::Path {
                    segments: local, ..
                },
            ) = (param.as_ref(), ret.as_ref())
            else {
                panic!("the converted nested body must retain both binder paths")
            };
            assert_eq!(mapped[0].as_str(), "Inner");
            assert_eq!(local[0].as_str(), nested.name);
        });
    }

    #[test]
    fn nested_target_spelling_alpha_allocation_is_linear() {
        const DEPTH: usize = 64;
        with_type_ctx(|tcx| {
            let signature = Signature::from_groups(vec![
                SignatureGroup::Type(vec![TypeParam {
                    name: "Inner".to_owned(),
                    span: span(1),
                    kind: None,
                }]),
                SignatureGroup::Value(vec![value_param("value", None, 3)]),
            ]);
            let mut nested = path("Expected", 600);
            for index in (0..DEPTH).rev() {
                nested = product(
                    path(
                        &format!("Free{index}"),
                        u32::try_from(index + 400).expect("test span fits in u32"),
                    ),
                    nested,
                    u32::try_from(index + 500).expect("test span fits in u32"),
                );
            }
            for index in (0..DEPTH).rev() {
                nested = forall(
                    "Inner",
                    u32::try_from(index + 100).expect("test span fits in u32"),
                    nested,
                );
            }
            let expected = forall("Expected", 7, function(nested, path("Expected", 700), 8));
            let expected = tcx.intern_type(&expected);
            let mut plan = plan_lowered_lambda_header(&signature, None, tcx)
                .expect("plan the deep capture-avoiding checked lambda header");

            reset_expected_alpha_materialization_work();
            let pure = plan
                .select_check(&expected, span(800), tcx)
                .expect("select the deep capture-avoiding checked lambda")
                .expect("the complete expected scheme selects checking");
            let protected_name_nodes = lowered_type_node_count(pure.1[0].as_type())
                + lowered_type_node_count(pure.0.as_type());
            let selected = bind_selected_alpha(&mut plan, &signature, tcx);

            let mut cursor = selected.1[0].as_type();
            let mut emitted = std::collections::HashSet::with_capacity(DEPTH);
            for _ in 0..DEPTH {
                let Type::Forall { param, body, .. } = cursor else {
                    panic!("the selected value slot lost a nested forall")
                };
                assert_ne!(param.name, "Inner");
                assert!(
                    emitted.insert(param.name.clone()),
                    "nested alpha allocation reused `{}`",
                    param.name
                );
                cursor = body.as_ref();
            }
            for index in 0..DEPTH {
                let Type::Product { left, right, .. } = cursor else {
                    panic!("the selected value slot lost its free-name product spine")
                };
                let Type::Path { segments, .. } = left.as_ref() else {
                    panic!("the free-name product spine lost a path leaf")
                };
                assert_eq!(segments[0].as_str(), format!("Free{index}"));
                cursor = right.as_ref();
            }
            let Type::Path { segments, .. } = cursor else {
                panic!("the nested forall chain lost its selected source occurrence")
            };
            assert_eq!(segments[0].as_str(), "Inner");

            let work = expected_alpha_materialization_work();
            assert_eq!(
                work.protected_name_nodes, protected_name_nodes,
                "the protected-name summary must visit each exact materialized input node once",
            );
            assert_eq!(
                work.fresh_name_probes,
                DEPTH * 2,
                "each colliding binder should probe its base and one monotonic suffix exactly once",
            );
        });
    }

    #[test]
    fn wide_expected_value_group_does_not_rewalk_every_remaining_suffix() {
        const WIDTH: usize = 64;
        with_type_ctx(|tcx| {
            let value_params = (0..WIDTH)
                .map(|index| {
                    value_param(
                        &format!("value{index}"),
                        Some(unit(
                            u32::try_from(index + 1).expect("test span fits in u32"),
                        )),
                        u32::try_from(index + 1).expect("test span fits in u32"),
                    )
                })
                .collect();
            let signature = Signature::from_groups(vec![SignatureGroup::Value(value_params)]);
            let mut expected_param = unit(700);
            for index in (0..WIDTH - 1).rev() {
                expected_param = product(
                    unit(u32::try_from(index + 800).expect("test span fits in u32")),
                    expected_param,
                    u32::try_from(index + 900).expect("test span fits in u32"),
                );
            }
            let expected = tcx.intern_type(&Type::Function {
                param: Box::new(expected_param),
                ret: Box::new(unit(1200)),
                meta: Meta::new(span(699)),
                abi_arity: WIDTH,
                caps: (),
            });
            let mut plan = plan_lowered_lambda_header(&signature, None, tcx)
                .expect("plan the wide checked lambda header");

            reset_expected_alpha_materialization_work();
            let selected = plan
                .select_check(&expected, span(1300), tcx)
                .expect("select the wide checked lambda")
                .expect("the complete expected value group selects checking");
            assert_eq!(selected.1.len(), WIDTH);

            let work = expected_alpha_materialization_work();
            assert_eq!(
                work.canonicalization_calls, 1,
                "a wide checked header must canonicalize its expected tree exactly once",
            );
            assert!(
                work.canonicalization_input_nodes + work.input_nodes <= WIDTH * 8,
                "wide expected preparation revisited {} canonicalization nodes across {} calls and {} materialization nodes across {} calls for width {WIDTH}",
                work.canonicalization_input_nodes,
                work.canonicalization_calls,
                work.input_nodes,
                work.calls,
            );
        });
    }

    #[test]
    fn lazy_expected_alpha_keeps_a_same_spelled_module_alias_shadowed() {
        with_package_type_ctx(
            &[("main.kio", "module main; type Expected = .;")],
            "main",
            |tcx| {
                let signature = Signature::from_groups(vec![
                    SignatureGroup::Type(vec![TypeParam {
                        name: "Source".to_owned(),
                        span: span(3),
                        kind: None,
                    }]),
                    SignatureGroup::Value(vec![value_param("value", Some(path("Source", 7)), 5)]),
                ]);
                let expected = forall(
                    "Expected",
                    11,
                    Type::Function {
                        param: Box::new(path("Expected", 13)),
                        ret: Box::new(path("Expected", 17)),
                        meta: Meta::new(span(12)),
                        abi_arity: 1,
                        caps: (),
                    },
                );
                let expected = tcx.intern_type(&expected);
                let mut plan = plan_lowered_lambda_header(&signature, None, tcx)
                    .expect("plan the alpha-equivalent checked lambda");
                let selected = plan
                    .select_check(&expected, span(23), tcx)
                    .expect("select the alpha-equivalent checked lambda")
                    .expect("the complete expected scheme selects checking");

                for selected in [&selected.1[0], &selected.0] {
                    let Type::Path { segments, .. } = selected.as_type() else {
                        panic!(
                            "the expected binder must remain rigid instead of unfolding the alias"
                        )
                    };
                    assert_eq!(segments[0].as_str(), "Expected");
                }
                assert_eq!(
                    expected_alpha_materialization_work().calls,
                    0,
                    "pure selection must retain exact expected spelling"
                );

                let selected = bind_selected_alpha(&mut plan, &signature, tcx);
                for selected in [&selected.1[0], &selected.0] {
                    let Type::Path { segments, .. } = selected.as_type() else {
                        panic!("the committed alpha edge must produce a source-aligned path")
                    };
                    assert_eq!(segments[0].as_str(), "Source");
                }
            },
        );
    }

    #[test]
    fn closed_header_kind_validation_reuses_borrowed_signature_binders() {
        with_type_ctx(|tcx| {
            let signature = Signature::from_groups(vec![
                SignatureGroup::Type(vec![TypeParam {
                    name: "F".to_owned(),
                    span: span(3),
                    kind: Some(Kind::arrow_chain(1)),
                }]),
                SignatureGroup::Value(vec![value_param(
                    "value",
                    Some(path_args("F", vec![unit(11)], 7)),
                    5,
                )]),
            ]);
            reset_planning_lexical_work();
            reset_annotation_plan_work();

            let plan = plan_source_lambda_header(&signature, None, tcx)
                .expect("the borrowed signature binder supplies the annotation kind");

            assert_eq!(plan.group_refs().count(), 2);
            let annotation = plan.value_slots[0]
                .annotation
                .as_ref()
                .expect("the value parameter is annotated");
            assert!(annotation.plan.is_closed_borrowed());
            assert_eq!(
                annotation_plan_work(),
                AnnotationPlanWork {
                    classifications: 1,
                    transport_materializations: 0,
                    rejections: 0,
                    committed_materializations: 0,
                    goals_allocated: 0,
                    binder_events_published: 0,
                }
            );
            let lexical_work = planning_lexical_work();
            assert_eq!(lexical_work.promotions, 0);
            assert_eq!(lexical_work.bindings_scanned, 0);
            assert_eq!(lexical_work.persistent_roots_built, 0);
            assert!(lexical_work.lookups > 0);
        });
    }

    #[test]
    fn rejected_direct_forall_hole_is_classified_once_without_precommit_mutation() {
        with_type_ctx(|tcx| {
            let source = Box::leak(Box::new(forall("A", 3, infer(11))));
            let expected_ty = unit(17);
            let expected = tcx.intern_type(&expected_ty);
            let before = tcx.planning_state_fingerprint();
            reset_annotation_plan_work();

            let lexical = PlanningLexicalView::borrowed(tcx);
            let plan = plan_source_annotation(source, tcx, &lexical, false)
                .expect("the source shape is well-kinded");
            let observation = plan.observation();

            assert_eq!(tcx.planning_state_fingerprint(), before);
            assert_eq!(
                annotation_plan_work(),
                AnnotationPlanWork {
                    classifications: 1,
                    transport_materializations: 1,
                    rejections: 1,
                    committed_materializations: 0,
                    goals_allocated: 0,
                    binder_events_published: 0,
                }
            );
            assert_eq!(observation.holes.len(), 1);
            assert_eq!(observation.infer_routes.len(), 1);
            assert!(observation.contributions.is_empty());
            assert!(!observation.root_present);
            assert_eq!(observation.emitted_infer_count, 1);
            assert_eq!(observation.binder_event_count, 2);
            assert_eq!(observation.infer_routes[0].0, span(11));
            assert_eq!(
                observation.infer_routes[0].2.as_ref().map(|binder| (
                    binder.name.as_str(),
                    binder.span,
                    binder.provider_file.as_deref()
                )),
                Some(("A", span(3), None))
            );
            let rejection = observation.rejection.expect("one rejection");
            assert_eq!(
                rejection.message,
                "type placeholder `_` cannot appear beneath a `forall` inside an annotation"
            );
            assert_eq!(rejection.span, span(11));
            assert_eq!(rejection.secondary().len(), 1);
            assert_eq!(rejection.secondary()[0].file, None);

            let error = plan
                .commit_against(source, &expected, span(11), tcx)
                .expect_err("a rejected plan cannot open an annotation equation");
            assert_eq!(error.diagnostic(), &rejection);
            assert_eq!(tcx.planning_state_fingerprint(), before);
            assert_eq!(
                annotation_plan_work(),
                AnnotationPlanWork {
                    classifications: 1,
                    transport_materializations: 1,
                    rejections: 1,
                    committed_materializations: 0,
                    goals_allocated: 0,
                    binder_events_published: 0,
                }
            );
        });
    }

    #[test]
    fn rejected_plan_cannot_enter_goal_materialization() {
        with_type_ctx(|tcx| {
            let source = forall("A", 3, infer(11));
            let lexical = PlanningLexicalView::borrowed(tcx);
            let plan = plan_source_annotation(&source, tcx, &lexical, false)
                .expect("the rejected source remains a pure plan");
            drop(lexical);

            let mut store = GoalStore::new();
            let scope = store.scope_from_type_ctx(tcx);
            let owner = store
                .begin_owner(None, scope, GoalOwnerKind::Annotation, span(11))
                .expect("test annotation owner");
            reset_annotation_plan_work();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = plan.materialize(&mut store, owner);
            }));

            assert!(result.is_err(), "a rejected plan reached materialization");
            assert_eq!(
                annotation_plan_work(),
                AnnotationPlanWork {
                    classifications: 0,
                    transport_materializations: 0,
                    rejections: 0,
                    committed_materializations: 0,
                    goals_allocated: 0,
                    binder_events_published: 0,
                }
            );
        });
    }

    #[test]
    fn imported_and_transitive_alias_binders_keep_the_terminal_provider_file() {
        with_package_type_ctx(
            &[
                (
                    "inner.kio",
                    "module inner; pub type Wrapped[Slot] = [Inner_bound] Slot -> Inner_bound;",
                ),
                (
                    "outer.kio",
                    "module outer; import inner(Wrapped); pub type Outer[Slot] = Wrapped(Slot);",
                ),
                ("caller.kio", "module caller; import outer(Outer);"),
            ],
            "caller",
            |tcx| {
                let source = path_args("Outer", vec![infer(19)], 7);
                let before = tcx.planning_state_fingerprint();
                reset_annotation_plan_work();
                let lexical = PlanningLexicalView::borrowed(tcx);
                let plan = plan_source_annotation(&source, tcx, &lexical, false)
                    .expect("the transitive imported alias is well-kinded");
                let observation = plan.observation();

                assert_eq!(tcx.planning_state_fingerprint(), before);
                assert_eq!(annotation_plan_work().committed_materializations, 0);
                assert_eq!(annotation_plan_work().goals_allocated, 0);
                assert_eq!(observation.infer_routes.len(), 1);
                let binder = observation.infer_routes[0]
                    .2
                    .as_ref()
                    .expect("the terminal provider forall encloses the source occurrence");
                assert_eq!(binder.name, "Inner_bound");
                assert_eq!(
                    binder.provider_file.as_deref(),
                    Some(Path::new("inner.kio"))
                );
                assert!(observation.root_present);
                assert!(observation.contributions.iter().any(|contribution| {
                    matches!(contribution, AliasTransportContribution::Root { .. })
                }));
                assert!(observation.contributions.iter().any(|contribution| {
                    matches!(contribution, AliasTransportContribution::Graft { .. })
                }));
                assert_eq!(observation.emitted_infer_count, 1);
            },
        );

        with_package_type_ctx(
            &[
                (
                    "inner.kio",
                    "module inner; pub type Wrapped[Slot] = [Inner_bound] Slot -> Inner_bound;",
                ),
                ("caller.kio", "module caller; import inner(Wrapped);"),
            ],
            "caller",
            |tcx| {
                let source = path_args("Wrapped", vec![infer(19)], 7);
                let lexical = PlanningLexicalView::borrowed(tcx);
                let plan = plan_source_annotation(&source, tcx, &lexical, false)
                    .expect("the direct imported alias is well-kinded");
                let observation = plan.observation();
                let binder = observation.infer_routes[0]
                    .2
                    .as_ref()
                    .expect("the direct provider forall encloses the source occurrence");

                assert_eq!(binder.name, "Inner_bound");
                assert_eq!(
                    binder.provider_file.as_deref(),
                    Some(Path::new("inner.kio"))
                );
            },
        );
    }

    #[test]
    fn caller_graft_and_alpha_renamed_provider_binder_keep_raw_origins() {
        with_package_type_ctx(
            &[
                (
                    "provider.kio",
                    "module provider; pub type Identity[Slot] = Slot; pub type Wrap[Slot] = [X] Slot -> X;",
                ),
                (
                    "caller.kio",
                    "module caller; import provider(Identity); import provider(Wrap);",
                ),
            ],
            "caller",
            |tcx| {
                let caller_owned =
                    path_args("Identity", vec![forall("Caller_bound", 23, infer(31))], 7);
                let lexical = PlanningLexicalView::borrowed(tcx);
                let plan = plan_source_annotation(&caller_owned, tcx, &lexical, false)
                    .expect("caller-owned forall through an identity alias");
                let observation = plan.observation();
                let binder = observation.infer_routes[0]
                    .2
                    .clone()
                    .expect("caller forall");
                assert_eq!(binder.name, "Caller_bound");
                assert_eq!(
                    binder.provider_file.as_deref(),
                    Some(Path::new("caller.kio"))
                );
                assert!(observation.contributions.iter().any(|contribution| {
                    matches!(
                        contribution,
                        AliasTransportContribution::Graft { provider_file, .. }
                            if provider_file.as_deref() == Some(Path::new("caller.kio"))
                    )
                }));

                tcx.push_type_param_kinded("X", Kind::Star, span(41));
                let alpha = path_args("Wrap", vec![product(path("X", 43), infer(47), 42)], 39);
                let lexical = PlanningLexicalView::borrowed(tcx);
                let plan = plan_source_annotation(&alpha, tcx, &lexical, false)
                    .expect("capture avoidance keeps the exact provider origin");
                let observation = plan.observation();
                let binder = observation.infer_routes[0]
                    .2
                    .as_ref()
                    .expect("provider forall");
                assert_eq!(binder.name, "X", "diagnostics use the raw binder spelling");
                assert_eq!(
                    binder.provider_file.as_deref(),
                    Some(Path::new("provider.kio"))
                );
                assert!(format!("{:?}", plan.ty()).contains("X_n2"));
            },
        );
    }

    #[test]
    fn dropped_alias_occurrence_has_no_route_goal_or_rejection() {
        with_package_type_ctx(
            &[(
                "main.kio",
                "module main; pub type Keep[First][Dropped] = First;",
            )],
            "main",
            |tcx| {
                let source = path_args("Keep", vec![unit(5), infer(11)], 3);
                let before = tcx.planning_state_fingerprint();
                reset_annotation_plan_work();
                let lexical = PlanningLexicalView::borrowed(tcx);
                let plan = plan_source_annotation(&source, tcx, &lexical, false)
                    .expect("the dropped argument is well-kinded");
                let observation = plan.observation();

                assert_eq!(tcx.planning_state_fingerprint(), before);
                assert_eq!(observation.holes, []);
                assert_eq!(observation.rejection, None);
                assert_eq!(observation.emitted_infer_count, 0);
                assert!(matches!(
                    observation.infer_routes.as_slice(),
                    [(_, AliasSourceDisposition::Dropped, None)]
                ));
                assert_eq!(annotation_plan_work().committed_materializations, 0);
                assert_eq!(annotation_plan_work().goals_allocated, 0);
            },
        );
    }

    #[test]
    fn header_readiness_distinguishes_context_required_slots_from_body_results() {
        with_type_ctx(|tcx| {
            let missing = Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
                "value", None, 3,
            )])]);
            let root_return = infer(7);
            let plan = plan_lowered_lambda_header(&missing, Some(&root_return), tcx)
                .expect("missing parameter and root return holes are admitted");
            assert!(plan.requires_immediate_selection());
            assert!(plan.value_slot_needs_type_publication(0));
            assert!(matches!(
                plan.planned_return(tcx),
                PlannedLambdaReturn::BodySynthesized
            ));
            plan.require_admitted_in_source_order()
                .expect("root slots remain admitted");

            let nested = forall("Return_bound", 13, infer(17));
            let concrete = Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
                "value",
                Some(unit(11)),
                11,
            )])]);
            let plan = plan_lowered_lambda_header(&concrete, Some(&nested), tcx)
                .expect("the rejected source remains a pure plan");
            assert!(matches!(
                plan.planned_return(tcx),
                PlannedLambdaReturn::ContextRequired { hole_span } if hole_span == span(17)
            ));
            let error = plan
                .require_admitted_in_source_order()
                .expect_err("a nested return hole is source-inadmissible");
            assert_eq!(
                error.diagnostic().message,
                "type placeholder `_` cannot appear beneath a `forall` inside an annotation"
            );
        });
    }
}
