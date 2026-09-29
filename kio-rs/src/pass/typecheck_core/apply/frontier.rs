//! One-shot ordinary-expression inference for surface Kio.
//!
//! This module is concrete to `Lowered`.  Prime keeps the direct,
//! goal-free application path in the parent module.  An ordinary frontier
//! owns one connected outer inference domain and one publication transaction.
//! Retained applications and lambda scopes introduce child owners only when
//! their unfinished result can constrain the connected outer domain.

use super::*;

use crate::ast::{Lowered, TypeGoalOwner};
use crate::pass::typecheck_core::{ClosedGoalOutput, GoalOwnerKind, ReservedParentHeaderExport};
use crate::pass::typecheck_full::publication::{PublicationBuilder, PublicationChildSlot};

/// The finite, boundary-local schedule used by retained expressions.
///
/// A root advances these modes in this order, while the retained-value driver
/// descends through immediate children left-to-right under the inherited mode.
/// `RelationProbe` is admitted only by an enclosing symmetric relation host;
/// ordinary roots skip it.
/// This is not a flat queue: a nested child's literal
/// fallback can never jump ahead of an earlier child of its parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FrontierMode {
    ExpectedOnly,
    LexicalFallback,
    RelationProbe,
    FinalPreflight,
    Final,
}

impl FrontierMode {
    pub(super) const fn rank(self) -> u8 {
        match self {
            Self::ExpectedOnly => 0,
            Self::LexicalFallback => 1,
            Self::RelationProbe => 2,
            Self::FinalPreflight => 3,
            Self::Final => 4,
        }
    }

    pub(super) fn successor(self, relation_host: bool) -> Option<Self> {
        assert!(
            relation_host || self != Self::RelationProbe,
            "RelationProbe escaped its symmetric relation host"
        );
        match self {
            Self::ExpectedOnly => Some(Self::LexicalFallback),
            Self::LexicalFallback if relation_host => Some(Self::RelationProbe),
            Self::LexicalFallback | Self::RelationProbe => Some(Self::FinalPreflight),
            Self::FinalPreflight => Some(Self::Final),
            Self::Final => None,
        }
    }

    pub(super) fn is_final_check(self) -> bool {
        matches!(self, Self::FinalPreflight | Self::Final)
    }

    pub(super) fn executes_actions(self) -> bool {
        matches!(self, Self::Final)
    }

    pub(super) fn executes_relation_actions(self) -> bool {
        matches!(self, Self::RelationProbe | Self::Final)
    }
}

/// One genuine owner/closure boundary inside an ordinary frontier.
pub(super) struct FrontierFrame<'m> {
    owner: TypeGoalOwner,
    delta: GoalDelta,
    publication: PublicationBuilder,
    rec_order: RecOrderJournal<'m>,
    // Keep the sparse table to one inline pointer in every ordinary frame.
    #[allow(clippy::box_collection)]
    entered_rec_order: Option<Box<Vec<EnteredRecOrderSource<'m>>>>,
    independent_source: bool,
    quote_source: Option<crate::ast::ExpressionOccurrenceId>,
}

struct EnteredRecOrderSource<'m> {
    source: &'m crate::ast::Expr<Lowered>,
    origin: TypeGoalOwner,
    header: ScopedType,
    forwarded: bool,
    #[cfg(test)]
    _counter: SourceRecordCounter,
}

impl<'m> EnteredRecOrderSource<'m> {
    fn new(
        source: &'m crate::ast::Expr<Lowered>,
        origin: TypeGoalOwner,
        header: ScopedType,
    ) -> Self {
        #[cfg(test)]
        super::record_completion_work(|work| {
            work.source_records_created += 1;
            work.source_records_live += 1;
            work.source_records_peak = work.source_records_peak.max(work.source_records_live);
        });
        Self {
            source,
            origin,
            header,
            forwarded: false,
            #[cfg(test)]
            _counter: SourceRecordCounter,
        }
    }
}

#[cfg(test)]
struct SourceRecordCounter;

#[cfg(test)]
impl Drop for SourceRecordCounter {
    fn drop(&mut self) {
        super::record_completion_work(|work| work.source_records_live -= 1);
    }
}

#[cfg(test)]
pub(super) fn retained_source_storage_sizes_for_test() -> [(&'static str, usize); 3] {
    [
        (
            "FrontierFrame",
            std::mem::size_of::<FrontierFrame<'static>>(),
        ),
        (
            "OpenRetainedPremise",
            std::mem::size_of::<OpenRetainedPremise<'static>>(),
        ),
        (
            "EnteredRecOrderSource",
            std::mem::size_of::<EnteredRecOrderSource<'static>>(),
        ),
    ]
}

/// A retained premise whose owner and lexical publication position are fixed,
/// but whose speculative delta has not yet opened.  This lets the call's
/// gather child commit root solutions before the retained premise snapshots
/// them, without moving the premise's publication after later siblings.
pub(super) struct FrontierChildReservation {
    parent: TypeGoalOwner,
    owner: TypeGoalOwner,
    span: Span,
    slot: PublicationChildSlot,
}

/// An opened retained premise.  Closing consumes the frame and its exact
/// lexical slot together, so neither can be replayed or paired with another
/// parent.
pub(super) struct OpenRetainedPremise<'m> {
    parent: TypeGoalOwner,
    // Retained premises nest under the inherited-mode schedule. Keep the
    // publication transaction in the explicit driver frame rather than the
    // Rust call stack.
    frame: Box<FrontierFrame<'m>>,
    slot: PublicationChildSlot,
}

/// Zero-goal connected child that seeds the application's root domain.
/// Keeping the owner and delta together prevents it from being mistaken for
/// a retained value boundary or returning semantic/publication outputs.
pub(super) struct GatherFrame {
    parent: TypeGoalOwner,
    owner: TypeGoalOwner,
    delta: GoalDelta,
    publication: PublicationBuilder,
    slot: PublicationChildSlot,
}

/// The pre-delta lifecycle of an outer ordinary application.
///
/// The root owner establishes the finite domain before any speculative write
/// or publication command exists. The outer delta and publication builder do
/// not open until planning has determined the complete root goal set.
pub(super) struct FrontierPlanning<'m> {
    store: GoalStore,
    root_owner: TypeGoalOwner,
    root_span: Span,
    marker: std::marker::PhantomData<&'m crate::ast::Expr<Lowered>>,
}

pub(super) enum PlanningSelection<T, U> {
    Selected(T),
    Unselected(U),
}

/// One non-final call layer retained below the connected chain root. Its
/// expression/publication owner is the child frame, while every inferred call
/// binder belongs to the still-open chain root.
pub(super) struct ConnectedCallFrontier<'a, 'm> {
    store: &'a mut GoalStore,
    goal_owner: TypeGoalOwner,
    frame: &'a mut FrontierFrame<'m>,
}

type RecOrderProposal<'m> = (&'m crate::ast::Expr<Lowered>, PendingRecOrderState<Lowered>);

enum RecOrderEntry<'m, T> {
    TypeOnly {
        source: &'m crate::ast::Expr<Lowered>,
    },
    Runtime {
        source: &'m crate::ast::Expr<Lowered>,
        payload: T,
        source_checked: bool,
    },
}

struct RecOrderEntries<'m, T> {
    entries: Vec<RecOrderEntry<'m, T>>,
}

type RecOrderJournal<'m> = RecOrderEntries<'m, ScopedType>;
type RecOrderOutputPlan<'m> = RecOrderEntries<'m, ()>;

/// Domain-local recursive-order observations. Runtime types remain scoped to
/// their inference owner and travel only through the goal store's ordinary
/// semantic-output typestates. Duplicate observations are deliberately kept
/// until the successful outer root close, where conflicts can be checked
/// without making a child commit fallible after mutation.
impl<'m> RecOrderJournal<'m> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn stage(
        &mut self,
        owner: TypeGoalOwner,
        store: &GoalStore,
        source: &'m crate::ast::Expr<Lowered>,
        state: PendingRecOrderState<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        if !tcx.is_pending_rec_order_source(source) {
            return Ok(());
        }
        match state {
            PendingRecOrderState::Pending => {}
            PendingRecOrderState::TypeOnly => {
                self.entries.push(RecOrderEntry::TypeOnly { source });
            }
            PendingRecOrderState::Runtime {
                exact_ty,
                source_checked,
            } => {
                let span = exact_ty.span();
                let exact_ty = store.scoped_type(owner, exact_ty, span)?;
                self.entries.push(RecOrderEntry::Runtime {
                    source,
                    payload: exact_ty,
                    source_checked,
                });
            }
        }
        Ok(())
    }

    fn append(&mut self, other: Self) {
        self.entries.extend(other.entries);
    }

    /// Return the exact type from the first completed lexical occurrence
    /// without consuming the carrier observation. Later occurrences reuse
    /// this owner-local evidence; carrier finalization remains the sole
    /// destructive journal operation.
    fn checked_runtime_for(&self, source: &'m crate::ast::Expr<Lowered>) -> Option<ScopedType> {
        self.entries.iter().find_map(|entry| match entry {
            RecOrderEntry::Runtime {
                source: candidate,
                payload,
                source_checked: true,
            } if std::ptr::eq(*candidate, source) => Some(payload.clone()),
            RecOrderEntry::TypeOnly { .. } | RecOrderEntry::Runtime { .. } => None,
        })
    }

    /// Consume one carrier-local observation after its body has completed.
    /// All matching runtime occurrences already live in this frame's owner,
    /// so their equality belongs in the frame's speculative delta. Removing
    /// them here prevents a compiler-private carrier binding from escaping as
    /// an output of the enclosing expression.
    fn take_observation_for(
        &mut self,
        source: &'m crate::ast::Expr<Lowered>,
        store: &GoalStore,
        delta: &mut GoalDelta,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Option<(Option<ScopedType>, bool)>, Error> {
        let mut retained = Vec::with_capacity(self.entries.len());
        let mut found = false;
        let mut exact: Option<ScopedType> = None;
        let mut source_checked = false;
        for entry in std::mem::take(&mut self.entries) {
            let matches_source = match &entry {
                RecOrderEntry::TypeOnly { source: candidate }
                | RecOrderEntry::Runtime {
                    source: candidate, ..
                } => std::ptr::eq(*candidate, source),
            };
            if !matches_source {
                retained.push(entry);
                continue;
            }
            found = true;
            if let RecOrderEntry::Runtime {
                payload,
                source_checked: checked,
                ..
            } = entry
            {
                source_checked |= checked;
                if let Some(existing) = exact.as_ref() {
                    store.constrain(delta, existing.clone(), payload, source.span(), tcx)?;
                } else {
                    exact = Some(payload);
                }
            }
        }
        self.entries = retained;
        Ok(found.then_some((exact, source_checked)))
    }

    fn into_outputs(
        self,
        escape: GoalEscape,
    ) -> (RecOrderOutputPlan<'m>, Vec<(ScopedType, GoalEscape)>) {
        let mut entries = Vec::with_capacity(self.entries.len());
        let mut outputs = Vec::new();
        for entry in self.entries {
            match entry {
                RecOrderEntry::TypeOnly { source } => {
                    entries.push(RecOrderEntry::TypeOnly { source });
                }
                RecOrderEntry::Runtime {
                    source,
                    payload,
                    source_checked,
                } => {
                    entries.push(RecOrderEntry::Runtime {
                        source,
                        payload: (),
                        source_checked,
                    });
                    outputs.push((payload, escape));
                }
            }
        }
        (RecOrderEntries { entries }, outputs)
    }

    fn finish(self, tcx: &TypeCtx<'m, '_, Lowered>) -> Result<Vec<RecOrderProposal<'m>>, Error> {
        let mut proposals: Vec<RecOrderProposal<'m>> = Vec::new();
        let mut by_source: std::collections::HashMap<*const crate::ast::Expr<Lowered>, usize> =
            std::collections::HashMap::new();
        for entry in self.entries {
            let (source, incoming) = match entry {
                RecOrderEntry::TypeOnly { source } => (source, PendingRecOrderState::TypeOnly),
                RecOrderEntry::Runtime {
                    source,
                    payload,
                    source_checked,
                } => (
                    source,
                    PendingRecOrderState::Runtime {
                        exact_ty: payload.into_interned_type(),
                        source_checked,
                    },
                ),
            };
            let key = source as *const crate::ast::Expr<Lowered>;
            if let Some(index) = by_source.get(&key).copied() {
                let (_, existing) = &mut proposals[index];
                *existing = join_rec_order_state(existing.clone(), incoming, source.span(), tcx)?;
            } else {
                by_source.insert(key, proposals.len());
                proposals.push((source, incoming));
            }
        }
        for (source, state) in &mut proposals {
            if let PendingRecOrderState::Runtime { exact_ty, .. } = state {
                *exact_ty = crate::pass::typecheck_core::types::canonicalize_type_annotation_presentation_at(
                    exact_ty,
                    source.span(),
                );
            }
        }
        Ok(proposals)
    }
}

impl<'m> RecOrderOutputPlan<'m> {
    fn rebuild_with(
        self,
        outputs: Vec<ClosedGoalOutput>,
        boundary: &str,
        mut take_scoped: impl FnMut(ClosedGoalOutput) -> ScopedType,
    ) -> RecOrderJournal<'m> {
        let mut outputs = outputs.into_iter();
        let mut rebuilt = Vec::with_capacity(self.entries.len());
        for entry in self.entries {
            match entry {
                RecOrderEntry::TypeOnly { source } => {
                    rebuilt.push(RecOrderEntry::TypeOnly { source });
                }
                RecOrderEntry::Runtime {
                    source,
                    payload: (),
                    source_checked,
                } => {
                    let output = outputs.next().unwrap_or_else(|| {
                        panic!("{boundary} recursive-order observation lost its semantic output")
                    });
                    rebuilt.push(RecOrderEntry::Runtime {
                        source,
                        payload: take_scoped(output),
                        source_checked,
                    });
                }
            }
        }
        assert!(
            outputs.next().is_none(),
            "{boundary} recursive-order transfer returned an extra semantic output"
        );
        RecOrderJournal { entries: rebuilt }
    }

    fn rebuild_retained(
        self,
        outputs: Vec<ClosedGoalOutput>,
        destination: TypeGoalOwner,
    ) -> RecOrderJournal<'m> {
        self.rebuild_with(outputs, "child", |output| {
            let ClosedGoalOutput::Retained(output) = output else {
                unreachable!("a child recursive-order observation must retain into its parent")
            };
            assert_eq!(
                output.destination(),
                destination,
                "recursive-order observation retained into another owner"
            );
            output.into_scoped_type()
        })
    }

    fn rebuild_goal_free(
        self,
        outputs: Vec<ClosedGoalOutput>,
        destination: TypeGoalOwner,
    ) -> RecOrderJournal<'m> {
        self.rebuild_with(outputs, "root", |output| {
            let ClosedGoalOutput::GoalFree(output) = output else {
                unreachable!("a root recursive-order observation must be goal-free")
            };
            assert_eq!(
                output.destination(),
                destination,
                "recursive-order observation closed at another root"
            );
            output.into_scoped_type()
        })
    }
}

fn join_rec_order_state(
    existing: PendingRecOrderState<Lowered>,
    incoming: PendingRecOrderState<Lowered>,
    span: Span,
    tcx: &TypeCtx<'_, '_, Lowered>,
) -> Result<PendingRecOrderState<Lowered>, Error> {
    use PendingRecOrderState::{Pending, Runtime, TypeOnly};

    match (existing, incoming) {
        (Pending, state) | (state, Pending) => Ok(state),
        (TypeOnly, TypeOnly) => Ok(TypeOnly),
        (
            Runtime {
                exact_ty,
                source_checked,
            },
            TypeOnly,
        )
        | (
            TypeOnly,
            Runtime {
                exact_ty,
                source_checked,
            },
        ) => Ok(Runtime {
            exact_ty,
            source_checked,
        }),
        (
            Runtime {
                exact_ty: existing,
                source_checked: existing_checked,
            },
            Runtime {
                exact_ty: incoming,
                source_checked: incoming_checked,
            },
        ) => {
            let ambient_binders = tcx.in_scope_type_param_binders();
            let alias_ctx = tcx.binder_alias_ctx(&ambient_binders);
            crate::pass::typecheck_core::aliases::require_type_equiv_state(
                existing.as_type(),
                incoming.as_type(),
                span,
                &alias_ctx,
                existing.identity_is_canonical(),
                incoming.identity_is_canonical(),
            )?;
            let (existing_canonical, existing_identity_canonical) =
                crate::pass::typecheck_core::aliases::canonicalize_deep_for_comparison(
                    existing.as_type(),
                    &alias_ctx,
                    existing.identity_is_canonical(),
                );
            let (incoming_canonical, incoming_identity_canonical) =
                crate::pass::typecheck_core::aliases::canonicalize_deep_for_comparison(
                    incoming.as_type(),
                    &alias_ctx,
                    incoming.identity_is_canonical(),
                );
            assert_eq!(
                existing_identity_canonical, incoming_identity_canonical,
                "equivalent recursive-order types disagreed on canonical identity"
            );
            let existing_handle =
                InternedType::fresh_with_identity(existing_canonical, existing_identity_canonical);
            let incoming_handle =
                InternedType::fresh_with_identity(incoming_canonical, incoming_identity_canonical);
            let exact_ty =
                crate::pass::typecheck_core::types::canonicalize_type_annotation_presentation_at(
                    &existing_handle,
                    span,
                );
            let incoming_presentation =
                crate::pass::typecheck_core::types::canonicalize_type_annotation_presentation_at(
                    &incoming_handle,
                    span,
                );
            if exact_ty.as_type() != incoming_presentation.as_type() {
                return Err(
                    crate::pass::typecheck_core::aliases::type_mismatch_error_state(
                        existing.as_type(),
                        incoming.as_type(),
                        span,
                        &alias_ctx,
                        existing.identity_is_canonical(),
                        incoming.identity_is_canonical(),
                    ),
                );
            }
            Ok(Runtime {
                exact_ty,
                source_checked: existing_checked || incoming_checked,
            })
        }
    }
}

impl GatherFrame {
    pub(super) fn owner(&self) -> TypeGoalOwner {
        self.owner
    }

    pub(super) fn delta_mut(&mut self) -> &mut GoalDelta {
        &mut self.delta
    }

    pub(super) fn parts_mut(&mut self) -> (&mut GoalDelta, &mut PublicationBuilder) {
        (&mut self.delta, &mut self.publication)
    }
}

impl FrontierChildReservation {
    pub(super) fn open<'m>(self, store: &GoalStore) -> Result<OpenRetainedPremise<'m>, Error> {
        let actual_parent = store
            .owner_parent(self.owner, self.span)
            .expect("a retained premise owner disappeared before open");
        assert_eq!(
            actual_parent,
            Some(self.parent),
            "a retained premise owner is attached to another parent"
        );
        let delta = store.begin_delta(self.owner, self.span)?;
        let publication = PublicationBuilder::new(store, &delta);
        Ok(OpenRetainedPremise {
            parent: self.parent,
            frame: Box::new(FrontierFrame {
                owner: self.owner,
                delta,
                publication,
                rec_order: RecOrderJournal::new(),
                entered_rec_order: None,
                independent_source: false,
                quote_source: None,
            }),
            slot: self.slot,
        })
    }
}

impl<'m> OpenRetainedPremise<'m> {
    pub(super) fn retain_quote_source(
        &mut self,
        source: &crate::ast::Expr<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) {
        if !matches!(
            source,
            crate::ast::Expr::Path { .. } | crate::ast::Expr::Unit { .. }
        ) && tcx.elaborations.captures_rec_quote_expression(source)
        {
            assert!(
                self.frame.quote_source.is_none(),
                "a retained source occurrence was entered twice"
            );
            self.frame.quote_source = Some(source.site().id);
        }
    }

    pub(super) fn take_quote_source(&mut self) -> Option<crate::ast::ExpressionOccurrenceId> {
        self.frame.quote_source.take()
    }

    pub(super) fn mark_independent_source(&mut self) {
        self.frame.independent_source = true;
    }

    pub(super) fn is_independent_source(&self) -> bool {
        self.frame.independent_source
    }

    pub(super) fn finish_independent_source(&mut self) {
        self.frame.independent_source = false;
    }

    pub(super) fn parent_owner(&self) -> TypeGoalOwner {
        self.parent
    }

    pub(super) fn owner(&self) -> TypeGoalOwner {
        self.frame.owner
    }

    pub(super) fn preview_goal_free_close_output(
        &self,
        store: &GoalStore,
        output: ScopedType,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<ScopedType, Error> {
        store.preview_goal_free_close_output(&self.frame.delta, output, self.parent, tcx)
    }

    pub(super) fn completion_delta(&self) -> &GoalDelta {
        &self.frame.delta
    }

    pub(super) fn completion_delta_mut(&mut self) -> &mut GoalDelta {
        &mut self.frame.delta
    }

    pub(super) fn publication_mut(&mut self) -> &mut PublicationBuilder {
        self.frame.publication_mut()
    }

    #[cfg(test)]
    pub(super) fn stage_rec_order(
        &mut self,
        store: &GoalStore,
        source: &'m crate::ast::Expr<Lowered>,
        state: PendingRecOrderState<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        self.frame.stage_rec_order(store, source, state, tcx)
    }
}

impl<'m> FrontierFrame<'m> {
    pub(super) fn owner(&self) -> TypeGoalOwner {
        self.owner
    }

    pub(super) fn parts_mut(&mut self) -> (&mut GoalDelta, &mut PublicationBuilder) {
        (&mut self.delta, &mut self.publication)
    }

    /// Stage metadata without exposing the frame's application delta. General
    /// application equations belong to a gather child; `parts_mut` remains
    /// only for the retained-value boundary that owns its own equations.
    pub(super) fn publication_mut(&mut self) -> &mut PublicationBuilder {
        &mut self.publication
    }

    fn begin_gather(
        &mut self,
        store: &mut GoalStore,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<GatherFrame, Error> {
        let scope = store.scope_from_type_ctx(tcx);
        let owner = store.begin_owner(
            Some(self.owner),
            scope,
            GoalOwnerKind::ApplicationGather,
            span,
        )?;
        let delta = store.begin_delta(owner, span)?;
        let publication = PublicationBuilder::new(store, &delta);
        let slot = self.publication.reserve_child(store, owner);
        Ok(GatherFrame {
            parent: self.owner,
            owner,
            delta,
            publication,
            slot,
        })
    }

    fn close_gather(
        &mut self,
        store: &mut GoalStore,
        gather: GatherFrame,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        let GatherFrame {
            parent,
            owner,
            delta,
            publication,
            slot,
        } = gather;
        assert_eq!(
            parent, self.owner,
            "application gather was closed through a different parent frame"
        );
        let actual_parent = store
            .owner_parent(owner, Span::new(0, 0))
            .expect("application gather owner disappeared before close");
        assert_eq!(
            actual_parent,
            Some(self.owner),
            "application gather owner is attached to a different parent frame"
        );
        assert_eq!(
            store
                .owner_kind(owner, Span::new(0, 0))
                .expect("application gather owner disappeared before close"),
            GoalOwnerKind::ApplicationGather,
            "ordinary frontier tried to close a non-gather owner as its seed frame"
        );
        let (publication, outputs) =
            publication
                .finish()
                .close_child(store, delta, self.owner, Vec::new(), tcx)?;
        assert!(
            outputs.is_empty(),
            "application gather child returned an unexpected semantic output"
        );
        self.publication.fill_child(slot, publication);
        Ok(())
    }

    /// Open one retained immediate-premise boundary.  `kind` is
    /// `NestedApplication` or `LambdaFrontier` for those specialized lexical
    /// boundaries and `RetainedValue` for a potentially blocking immediate
    /// premise whose internal expression tree otherwise shares one journal.
    /// All child-owned goals are allocated by `plan` before its exact delta is
    /// opened.
    pub(super) fn reserve_child_with<T>(
        store: &mut GoalStore,
        parent: &mut Self,
        kind: GoalOwnerKind,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
        plan: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<T, Error>,
    ) -> Result<(FrontierChildReservation, T), Error> {
        assert!(
            matches!(
                kind,
                GoalOwnerKind::NestedApplication
                    | GoalOwnerKind::LambdaFrontier
                    | GoalOwnerKind::RetainedValue
            ),
            "ordinary frontier child used a non-retained owner kind"
        );
        let scope = store.scope_from_type_ctx(tcx);
        let owner = store.begin_owner(Some(parent.owner), scope, kind, span)?;
        let planned = plan(store, owner)?;
        let slot = parent.publication.reserve_child(store, owner);
        Ok((
            FrontierChildReservation {
                parent: parent.owner,
                owner,
                span,
                slot,
            },
            planned,
        ))
    }

    /// Reserve a child whose scope is one exact retained-binder extension of
    /// its already-validated parent. Lambda lexical slices use this path so
    /// persistent scope prefixes are shared instead of rebuilt from `TypeCtx`.
    pub(super) fn reserve_lexical_child_with<T>(
        store: &mut GoalStore,
        parent: &mut Self,
        kind: GoalOwnerKind,
        binders: &[super::super::RetainedTypeBinder<'m>],
        span: Span,
        plan: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<T, Error>,
    ) -> Result<(FrontierChildReservation, T), Error> {
        let owner = store.begin_lexical_owner(parent.owner, binders, kind, span)?;
        let planned = plan(store, owner)?;
        let slot = parent.publication.reserve_child(store, owner);
        Ok((
            FrontierChildReservation {
                parent: parent.owner,
                owner,
                span,
                slot,
            },
            planned,
        ))
    }

    /// Plan one optional retained-child route in its real owner.  A declined
    /// route has not opened a delta or reserved a publication position, so
    /// removing that last provisional owner restores the exact parent state
    /// before the caller selects its ordinary fallback.
    pub(super) fn select_child_with<T, U>(
        store: &mut GoalStore,
        parent: &mut Self,
        kind: GoalOwnerKind,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
        select: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<PlanningSelection<T, U>, Error>,
    ) -> Result<PlanningSelection<(FrontierChildReservation, T), U>, Error> {
        let scope = store.scope_from_type_ctx(tcx);
        let owner = store.begin_owner(Some(parent.owner), scope, kind, span)?;
        let planned = match select(store, owner)? {
            PlanningSelection::Selected(planned) => planned,
            PlanningSelection::Unselected(outcome) => {
                store.discard_unopened_leaf_owner(owner, span);
                return Ok(PlanningSelection::Unselected(outcome));
            }
        };
        let slot = parent.publication.reserve_child(store, owner);
        Ok(PlanningSelection::Selected((
            FrontierChildReservation {
                parent: parent.owner,
                owner,
                span,
                slot,
            },
            planned,
        )))
    }

    /// Atomically export a completed retained premise to its direct parent
    /// and fill the publication position reserved before later siblings were
    /// visited.  A blocked or failed frame is simply dropped and exports
    /// neither ancestor equations nor publication commands.
    fn close_into_parent(
        self,
        store: &mut GoalStore,
        parent: &mut Self,
        slot: PublicationChildSlot,
        mut outputs: Vec<(ScopedType, GoalEscape)>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        let Self {
            owner: _,
            delta,
            publication,
            rec_order,
            entered_rec_order: _,
            independent_source: _,
            quote_source: _,
        } = self;
        let caller_output_count = outputs.len();
        let (rec_order_plan, rec_order_outputs) =
            rec_order.into_outputs(GoalEscape::ToOwner(parent.owner));
        outputs.extend(rec_order_outputs);
        let (publication, mut outputs) =
            publication
                .finish()
                .close_child(store, delta, parent.owner, outputs, tcx)?;
        let rec_order_outputs = outputs.split_off(caller_output_count);
        let rec_order = rec_order_plan.rebuild_retained(rec_order_outputs, parent.owner);
        parent.publication.fill_child(slot, publication);
        parent.rec_order.append(rec_order);
        Ok(outputs)
    }

    fn close_retained_premise(
        open: OpenRetainedPremise<'m>,
        store: &mut GoalStore,
        parent: &mut Self,
        outputs: Vec<(ScopedType, GoalEscape)>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        let OpenRetainedPremise {
            parent: expected_parent,
            frame,
            slot,
        } = open;
        assert!(
            frame.quote_source.is_none(),
            "a retained quotation source bypassed public completion capture"
        );
        assert_eq!(
            expected_parent, parent.owner,
            "a retained premise was closed through another parent"
        );
        Self::close_into_parent(*frame, store, parent, slot, outputs, tcx)
    }

    fn close_prepared_retained_premise(
        open: OpenRetainedPremise<'m>,
        store: &mut GoalStore,
        parent: &mut Self,
        outputs: Vec<(crate::pass::typecheck_core::PreparedCloseType, GoalEscape)>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        let OpenRetainedPremise {
            parent: expected_parent,
            frame,
            slot,
        } = open;
        assert!(
            frame.quote_source.is_none(),
            "a prepared quotation source bypassed public completion capture"
        );
        assert_eq!(
            expected_parent, parent.owner,
            "a prepared retained premise was closed through another parent"
        );
        (*frame).close_prepared_into_parent(store, parent, slot, outputs, tcx)
    }

    /// Close one binder-retaining child whose primary semantic output was
    /// prepared from its exact lexical scope. Recursive-order payloads keep
    /// their own scoped identity: they are prepared independently under the
    /// same delta rather than being abstracted beneath the child's `Forall`.
    fn close_prepared_into_parent(
        self,
        store: &mut GoalStore,
        parent: &mut Self,
        slot: PublicationChildSlot,
        mut outputs: Vec<(crate::pass::typecheck_core::PreparedCloseType, GoalEscape)>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        let Self {
            owner: _,
            delta,
            publication,
            rec_order,
            entered_rec_order: _,
            independent_source: _,
            quote_source: _,
        } = self;
        let caller_output_count = outputs.len();
        let (rec_order_plan, rec_order_outputs) =
            rec_order.into_outputs(GoalEscape::ToOwner(parent.owner));
        for (output, escape) in rec_order_outputs {
            outputs.push((store.zonk_for_close(&delta, output, tcx)?, escape));
        }
        let (publication, mut outputs) =
            publication
                .finish()
                .close_prepared_child(store, delta, parent.owner, outputs, tcx)?;
        let rec_order_outputs = outputs.split_off(caller_output_count);
        let rec_order = rec_order_plan.rebuild_retained(rec_order_outputs, parent.owner);
        parent.publication.fill_child(slot, publication);
        parent.rec_order.append(rec_order);
        Ok(outputs)
    }

    fn prepare_structural_for_close(
        store: &mut GoalStore,
        frame: &mut Self,
        ty: &crate::ast::Type<Lowered>,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<PreparedCloseType, Error> {
        Self::prepare_structural_for_close_inner(
            store,
            frame,
            ty,
            tcx,
            &mut crate::pass::typecheck_core::kind_scheme::CompleteSchemeProofs::default(),
        )
    }

    fn prepare_structural_for_close_inner(
        store: &mut GoalStore,
        frame: &mut Self,
        ty: &crate::ast::Type<Lowered>,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
        scheme_proofs: &mut crate::pass::typecheck_core::kind_scheme::CompleteSchemeProofs,
    ) -> Result<PreparedCloseType, Error> {
        match ty {
            crate::ast::Type::Forall { param, body, .. } => {
                if param.effective_kind() != crate::ast::Kind::Star
                    && !store
                        .classify_complete_function_scheme_at_owner(
                            ty,
                            true,
                            frame.owner,
                            &frame.delta,
                            tcx,
                            scheme_proofs,
                        )?
                        .is_complete()
                {
                    return Err(Error::type_(
                        param.span,
                        "a higher-kinded `Forall` binder must bind a complete function scheme",
                    ));
                }
                let mark = tcx.save();
                let binder = tcx.push_owned_retained_type_param(param);
                let reserved = Self::reserve_lexical_child_with(
                    store,
                    frame,
                    GoalOwnerKind::RetainedValue,
                    std::slice::from_ref(&binder),
                    param.span,
                    |_store, _owner| Ok(()),
                );
                tcx.restore(mark);
                let (reservation, ()) = reserved?;
                let mut child = reservation.open(store)?;

                let mark = tcx.save();
                tcx.push_retained_type_binder(&binder);
                let prepared = (|| {
                    let body = Self::prepare_structural_for_close_inner(
                        store,
                        &mut child.frame,
                        body,
                        tcx,
                        scheme_proofs,
                    )?;
                    Ok::<_, Error>(store.abstract_lexical_forall(body, &binder))
                })();
                tcx.restore(mark);
                let prepared = prepared?;

                let destination = frame.owner;
                let mut outputs = Self::close_prepared_retained_premise(
                    child,
                    store,
                    frame,
                    vec![(prepared, GoalEscape::ToOwner(destination))],
                    tcx,
                )?;
                assert_eq!(
                    outputs.len(),
                    1,
                    "lexical structural abstraction returned an unexpected output count"
                );
                match outputs.remove(0) {
                    ClosedGoalOutput::Retained(output) => {
                        assert_eq!(
                            output.destination(),
                            destination,
                            "lexical structural abstraction targeted another owner"
                        );
                        store.zonk_for_close(&frame.delta, output.into_scoped_type(), tcx)
                    }
                    ClosedGoalOutput::RetainedFunctionScheme(output) => {
                        assert_eq!(
                            output.destination(),
                            destination,
                            "lexical function-scheme abstraction targeted another owner"
                        );
                        store.prepare_retained_function_scheme(&frame.delta, output, tcx)
                    }
                    ClosedGoalOutput::GoalFree(_) | ClosedGoalOutput::FunctionScheme(_) => {
                        unreachable!("a retained lexical abstraction closed at its parent boundary")
                    }
                }
            }
            crate::ast::Type::Function {
                param,
                ret,
                abi_arity,
                meta,
                ..
            } => {
                let param = Self::prepare_structural_for_close_inner(
                    store,
                    frame,
                    param,
                    tcx,
                    scheme_proofs,
                )?;
                let ret = Self::prepare_structural_for_close_inner(
                    store,
                    frame,
                    ret,
                    tcx,
                    scheme_proofs,
                )?;
                store.compose_for_close(
                    PreparedTypeShell::Function {
                        abi_arity: *abi_arity,
                        span: meta.span,
                    },
                    vec![param, ret],
                )
            }
            crate::ast::Type::Product { left, right, meta } => {
                let left = Self::prepare_structural_for_close_inner(
                    store,
                    frame,
                    left,
                    tcx,
                    scheme_proofs,
                )?;
                let right = Self::prepare_structural_for_close_inner(
                    store,
                    frame,
                    right,
                    tcx,
                    scheme_proofs,
                )?;
                store.compose_for_close(
                    PreparedTypeShell::Product { span: meta.span },
                    vec![left, right],
                )
            }
            crate::ast::Type::Sum { left, right, meta } => {
                let left = Self::prepare_structural_for_close_inner(
                    store,
                    frame,
                    left,
                    tcx,
                    scheme_proofs,
                )?;
                let right = Self::prepare_structural_for_close_inner(
                    store,
                    frame,
                    right,
                    tcx,
                    scheme_proofs,
                )?;
                store.compose_for_close(
                    PreparedTypeShell::Sum { span: meta.span },
                    vec![left, right],
                )
            }
            crate::ast::Type::Path {
                segments,
                args,
                meta,
            } if !args.is_empty() => {
                let children = args
                    .iter()
                    .map(|arg| {
                        Self::prepare_structural_for_close_inner(
                            store,
                            frame,
                            arg,
                            tcx,
                            scheme_proofs,
                        )
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                store.compose_for_close(
                    PreparedTypeShell::Path {
                        segments: segments.clone(),
                        span: meta.span,
                    },
                    children,
                )
            }
            crate::ast::Type::Goal {
                goal, args, meta, ..
            } if !args.is_empty() => {
                let children = args
                    .iter()
                    .map(|arg| {
                        Self::prepare_structural_for_close_inner(
                            store,
                            frame,
                            arg,
                            tcx,
                            scheme_proofs,
                        )
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                store.compose_for_close(
                    PreparedTypeShell::Goal {
                        goal: *goal,
                        span: meta.span,
                    },
                    children,
                )
            }
            crate::ast::Type::Infer { .. } => {
                unreachable!("a source `_` reached structural close preparation")
            }
            crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
            crate::ast::Type::Path { .. }
            | crate::ast::Type::Goal { .. }
            | crate::ast::Type::Unit { .. }
            | crate::ast::Type::Bottom { .. } => {
                let scoped = store.scoped_type(
                    frame.owner,
                    InternedType::fresh_canonical(ty.clone()),
                    ty.span(),
                )?;
                store.zonk_for_close(&frame.delta, scoped, tcx)
            }
        }
    }

    /// Rebuild one canonical type containing ancestor goals beneath lexical
    /// `Forall`s in an exact retained-owner tree, then return it only to this
    /// call frame. Ordinary goal-free and non-polymorphic types never enter
    /// this path.
    fn retain_structural_foralls(
        store: &mut GoalStore,
        parent: &mut Self,
        ty: InternedType<Lowered>,
        span: Span,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<ScopedType, Error> {
        assert!(
            ty.identity_is_canonical(),
            "structural Forall rebuilding requires canonical nominal identity"
        );
        let (reservation, ()) = Self::reserve_lexical_child_with(
            store,
            parent,
            GoalOwnerKind::RetainedValue,
            &[],
            span,
            |_store, _owner| Ok(()),
        )?;
        let mut child = reservation.open(store)?;
        let prepared =
            Self::prepare_structural_for_close(store, &mut child.frame, ty.as_type(), tcx)?;
        let destination = parent.owner;
        let mut outputs = Self::close_prepared_retained_premise(
            child,
            store,
            parent,
            vec![(prepared, GoalEscape::ToOwner(destination))],
            tcx,
        )?;
        assert_eq!(
            outputs.len(),
            1,
            "structural Forall rebuilding returned an unexpected output count"
        );
        match outputs.remove(0) {
            ClosedGoalOutput::Retained(output) => {
                assert_eq!(
                    output.destination(),
                    destination,
                    "structural Forall rebuilding targeted another call owner"
                );
                Ok(output.into_scoped_type())
            }
            ClosedGoalOutput::RetainedFunctionScheme(output) => {
                assert_eq!(
                    output.destination(),
                    destination,
                    "structural function scheme targeted another call owner"
                );
                Ok(output.into_scoped_type())
            }
            ClosedGoalOutput::GoalFree(_) | ClosedGoalOutput::FunctionScheme(_) => {
                unreachable!("structural Forall rebuilding closed at its call owner")
            }
        }
    }

    pub(super) fn stage_rec_order(
        &mut self,
        store: &GoalStore,
        source: &'m crate::ast::Expr<Lowered>,
        state: PendingRecOrderState<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        self.rec_order.stage(self.owner, store, source, state, tcx)
    }

    fn rec_order_runtime_for_use(
        &self,
        source: &'m crate::ast::Expr<Lowered>,
    ) -> Option<ScopedType> {
        self.rec_order.checked_runtime_for(source).or_else(|| {
            self.entered_rec_order
                .as_deref()?
                .iter()
                .find_map(|entry| std::ptr::eq(entry.source, source).then(|| entry.header.clone()))
        })
    }

    fn take_rec_order_observation(
        &mut self,
        store: &GoalStore,
        source: &'m crate::ast::Expr<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Option<(Option<ScopedType>, bool)>, Error> {
        if let Some(entered) = self.entered_rec_order.as_mut() {
            entered.retain(|entry| !std::ptr::eq(entry.source, source));
        }
        self.rec_order
            .take_observation_for(source, store, &mut self.delta, tcx)
    }
}

impl<'m> FrontierPlanning<'m> {
    pub(super) fn begin(span: Span, tcx: &TypeCtx<'m, '_, Lowered>) -> Result<Self, Error> {
        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(tcx);
        let root_owner = store.begin_owner(None, scope, GoalOwnerKind::Application, span)?;
        Ok(Self {
            store,
            root_owner,
            root_span: span,
            marker: std::marker::PhantomData,
        })
    }

    /// Allocate the complete outer goal set, then open the one exact root
    /// delta and publication builder.
    pub(super) fn finish_root_with<T>(
        mut self,
        plan: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<T, Error>,
    ) -> Result<(OrdinaryFrontier<'m>, T), Error> {
        let planned = plan(&mut self.store, self.root_owner)?;
        let delta = self.store.begin_delta(self.root_owner, self.root_span)?;
        let publication = PublicationBuilder::new(&self.store, &delta);
        Ok((
            OrdinaryFrontier {
                store: self.store,
                root: FrontierFrame {
                    owner: self.root_owner,
                    delta,
                    publication,
                    rec_order: RecOrderJournal::new(),
                    entered_rec_order: None,
                    independent_source: false,
                    quote_source: None,
                },
            },
            planned,
        ))
    }
}

/// The sole domain and eventual visible commit for one connected outer
/// application.  All goals are allocated by `plan` before the root delta is
/// opened, so its authority covers the exact goal set for the derivation.
pub(super) struct OrdinaryFrontier<'m> {
    store: GoalStore,
    root: FrontierFrame<'m>,
}

impl<'m> OrdinaryFrontier<'m> {
    pub(super) fn forward_rec_order_entries(
        &mut self,
        child: &mut OpenRetainedPremise<'m>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<bool, Error> {
        ConnectedCallFrontier {
            store: &mut self.store,
            goal_owner: self.root.owner,
            frame: &mut self.root,
        }
        .forward_rec_order_entries(child, tcx)
    }

    pub(super) fn begin_planning(
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<FrontierPlanning<'m>, Error> {
        FrontierPlanning::begin(span, tcx)
    }

    pub(super) fn begin_root_with<T>(
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
        plan: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<T, Error>,
    ) -> Result<(Self, T), Error> {
        Self::begin_planning(span, tcx)?.finish_root_with(plan)
    }

    pub(super) fn owner(&self) -> TypeGoalOwner {
        self.root.owner()
    }

    pub(super) fn store(&self) -> &GoalStore {
        &self.store
    }

    pub(super) fn root_publication_mut(
        &mut self,
    ) -> (&GoalStore, TypeGoalOwner, &mut PublicationBuilder) {
        let Self { store, root } = self;
        let owner = root.owner;
        (store, owner, &mut root.publication)
    }

    pub(super) fn root_parts_mut(
        &mut self,
    ) -> (
        &GoalStore,
        TypeGoalOwner,
        &mut GoalDelta,
        &mut PublicationBuilder,
    ) {
        let Self { store, root } = self;
        let owner = root.owner;
        let (delta, publication) = root.parts_mut();
        (store, owner, delta, publication)
    }

    pub(super) fn reserve_root_goal_in_live_delta(
        &mut self,
        kind: crate::ast::Kind,
        solution_policy: GoalSolutionPolicy,
        origin: GoalOrigin,
    ) -> Result<crate::ast::TypeGoalRef, Error> {
        let Self { store, root } = self;
        store.reserve_goal_in_live_delta(&mut root.delta, kind, solution_policy, origin)
    }

    pub(super) fn activate_root_reserved_goal_at(
        &mut self,
        reserved: ReservedGoalRef,
        diagnostic_span: Span,
        occurrence_span: Span,
    ) -> Result<(crate::ast::TypeGoalRef, ScopedType), Error> {
        let owner = self.root.owner;
        self.store.activate_reserved_goal_at(
            owner,
            reserved,
            owner,
            diagnostic_span,
            occurrence_span,
        )
    }

    pub(super) fn reserve_root_child_with<T>(
        &mut self,
        kind: GoalOwnerKind,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
        plan: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<T, Error>,
    ) -> Result<(FrontierChildReservation, T), Error> {
        FrontierFrame::reserve_child_with(&mut self.store, &mut self.root, kind, span, tcx, plan)
    }

    pub(super) fn reserve_root_lexical_child_with<T>(
        &mut self,
        kind: GoalOwnerKind,
        binders: &[super::super::RetainedTypeBinder<'m>],
        span: Span,
        plan: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<T, Error>,
    ) -> Result<(FrontierChildReservation, T), Error> {
        FrontierFrame::reserve_lexical_child_with(
            &mut self.store,
            &mut self.root,
            kind,
            binders,
            span,
            plan,
        )
    }

    pub(super) fn select_root_child_with<T, U>(
        &mut self,
        kind: GoalOwnerKind,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
        select: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<PlanningSelection<T, U>, Error>,
    ) -> Result<PlanningSelection<(FrontierChildReservation, T), U>, Error> {
        FrontierFrame::select_child_with(&mut self.store, &mut self.root, kind, span, tcx, select)
    }

    pub(super) fn open_reserved_child(
        &self,
        reservation: FrontierChildReservation,
    ) -> Result<OpenRetainedPremise<'m>, Error> {
        reservation.open(&self.store)
    }

    pub(super) fn connected_call_frontier<'a>(
        &'a mut self,
        open: &'a mut OpenRetainedPremise<'m>,
    ) -> ConnectedCallFrontier<'a, 'm> {
        ConnectedCallFrontier {
            store: &mut self.store,
            goal_owner: self.root.owner,
            frame: &mut open.frame,
        }
    }

    pub(super) fn retained_value_frontier<'a>(
        &'a mut self,
        open: &'a mut OpenRetainedPremise<'m>,
    ) -> ConnectedCallFrontier<'a, 'm> {
        let goal_owner = open.frame.owner;
        ConnectedCallFrontier {
            store: &mut self.store,
            goal_owner,
            frame: &mut open.frame,
        }
    }

    pub(super) fn close_root_child(
        &mut self,
        child: OpenRetainedPremise<'m>,
        outputs: Vec<(ScopedType, GoalEscape)>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        FrontierFrame::close_retained_premise(child, &mut self.store, &mut self.root, outputs, tcx)
    }

    pub(super) fn close_prepared_root_child(
        &mut self,
        child: OpenRetainedPremise<'m>,
        outputs: Vec<(PreparedCloseType, GoalEscape)>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        FrontierFrame::close_prepared_retained_premise(
            child,
            &mut self.store,
            &mut self.root,
            outputs,
            tcx,
        )
    }

    pub(super) fn stage_rec_order(
        &mut self,
        source: &'m crate::ast::Expr<Lowered>,
        state: PendingRecOrderState<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        self.root.stage_rec_order(&self.store, source, state, tcx)
    }

    pub(super) fn rec_order_runtime_for_use(
        &self,
        source: &'m crate::ast::Expr<Lowered>,
    ) -> Option<ScopedType> {
        self.root.rec_order_runtime_for_use(source)
    }

    pub(super) fn take_rec_order_observation(
        &mut self,
        source: &'m crate::ast::Expr<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Option<(Option<ScopedType>, bool)>, Error> {
        self.root
            .take_rec_order_observation(&self.store, source, tcx)
    }

    /// Atomically seed the root from the call's written arguments,
    /// contextual result, and independently completed fresh premises.
    ///
    /// A parent delta's tentative writes are intentionally invisible to its
    /// children.  The seed equations therefore live in this short-lived
    /// zero-local-goal child and become root state only when the child closes.
    /// Later retained premises then close left-to-right against that state.
    pub(super) fn begin_gather(
        &mut self,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<GatherFrame, Error> {
        self.root.begin_gather(&mut self.store, span, tcx)
    }

    pub(super) fn close_gather(
        &mut self,
        gather: GatherFrame,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        self.root.close_gather(&mut self.store, gather, tcx)
    }

    pub(super) fn retain_structural_foralls(
        &mut self,
        ty: InternedType<Lowered>,
        span: Span,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<ScopedType, Error> {
        FrontierFrame::retain_structural_foralls(&mut self.store, &mut self.root, ty, span, tcx)
    }

    pub(super) fn close_root_with<R>(
        self,
        mut outputs: Vec<(ScopedType, GoalEscape)>,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
        finish: impl FnOnce(Vec<ClosedGoalOutput>, &TypeCtx<'m, '_, Lowered>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let Self { mut store, root } = self;
        assert!(
            root.delta.is_empty(),
            "ordinary application root carried writes outside its gather/retained children"
        );
        let caller_output_count = outputs.len();
        let (rec_order_plan, rec_order_outputs) = root
            .rec_order
            .into_outputs(GoalEscape::ClosedAt(root.owner));
        outputs.extend(rec_order_outputs);
        let mut prepared = root
            .publication
            .finish()
            .prepare_root(&store, root.delta, outputs, tcx)?;
        let mut outputs = prepared.take_semantic_outputs();
        let rec_order_outputs = outputs.split_off(caller_output_count);
        let result = finish(outputs, tcx)?;
        let rec_order_updates = rec_order_plan
            .rebuild_goal_free(rec_order_outputs, root.owner)
            .finish(tcx)?;
        let rec_order = tcx.prepare_rec_order_updates(rec_order_updates);
        let unclaimed = prepared.commit_with_rec_order(&mut store, tcx, rec_order)?;
        assert!(
            unclaimed.is_empty(),
            "ordinary frontier committed an unclaimed semantic output"
        );
        Ok(result)
    }
}

impl<'m> ConnectedCallFrontier<'_, 'm> {
    pub(super) fn isolate_producer(
        &mut self,
        boundary: TypeGoalOwner,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        self.store
            .isolate_producer(boundary, &self.frame.delta, span, tcx)
    }

    pub(super) fn accept_producer_wave(
        &mut self,
        wrappers: Vec<&mut GoalDelta>,
        producers: Vec<super::super::goals::ProducerObligations>,
        equations: Vec<(ScopedType, ScopedType, Span)>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        self.store
            .accept_producer_wave(&mut self.frame.delta, wrappers, producers, equations, tcx)
    }

    pub(super) fn owner(&self) -> TypeGoalOwner {
        self.frame.owner
    }

    pub(super) fn goal_owner(&self) -> TypeGoalOwner {
        self.goal_owner
    }

    pub(super) fn store(&self) -> &GoalStore {
        self.store
    }

    pub(super) fn publication_mut(
        &mut self,
    ) -> (&GoalStore, TypeGoalOwner, &mut PublicationBuilder) {
        (self.store, self.frame.owner, &mut self.frame.publication)
    }

    pub(super) fn parts_mut(
        &mut self,
    ) -> (
        &GoalStore,
        TypeGoalOwner,
        &mut GoalDelta,
        &mut PublicationBuilder,
    ) {
        let Self { store, frame, .. } = self;
        let owner = frame.owner;
        let (delta, publication) = frame.parts_mut();
        (store, owner, delta, publication)
    }

    pub(super) fn reserve_parent_header_export(
        &mut self,
        child: &OpenRetainedPremise<'m>,
        header: ScopedType,
        span: Span,
    ) -> Result<ReservedParentHeaderExport, Error> {
        assert_eq!(
            child.parent, self.frame.owner,
            "a projected header was reserved through a non-parent frontier"
        );
        self.store.reserve_parent_header_export(
            &mut self.frame.delta,
            &child.frame.delta,
            header,
            span,
        )
    }

    pub(super) fn record_rec_order_entry(
        &mut self,
        source: &'m crate::ast::Expr<Lowered>,
        origin: TypeGoalOwner,
        header: ScopedType,
    ) {
        assert_eq!(
            self.store
                .owner_parent(origin, source.span())
                .expect("retained source owner"),
            Some(self.frame.owner),
            "an entered source must remain a direct child of its recording frame"
        );
        let entries = self
            .frame
            .entered_rec_order
            .get_or_insert_with(Default::default);
        assert!(
            !entries
                .iter()
                .any(|entry| std::ptr::eq(entry.source, source)),
            "a recursive-order source entered twice in one retained frame"
        );
        entries.push(EnteredRecOrderSource::new(source, origin, header));
    }

    /// Forward only binding expectations. The source cursor and its checked
    /// observation remain in their original publication position.
    pub(super) fn forward_rec_order_entries(
        &mut self,
        child: &mut OpenRetainedPremise<'m>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<bool, Error> {
        assert_eq!(child.parent, self.frame.owner);
        let Some(entries) = child.frame.entered_rec_order.as_ref() else {
            return Ok(false);
        };
        let mut reserved = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            #[cfg(test)]
            super::record_completion_work(|work| work.rec_order_forward_checks += 1);
            if entry.forwarded || !tcx.is_pending_rec_order_source(entry.source) {
                continue;
            }
            if child.frame.independent_source {
                // This rejects a boundary-local blocker before an export can
                // replace it with a parent-owned proxy.
                self.store
                    .zonk_for_close(&child.frame.delta, entry.header.clone(), tcx)?;
            }
            let export = self.store.reserve_parent_header_export(
                &mut self.frame.delta,
                &child.frame.delta,
                entry.header.clone(),
                entry.source.span(),
            )?;
            reserved.push((index, entry.source, entry.origin, export));
        }
        let contributed = !reserved.is_empty();
        for (index, source, origin, export) in reserved {
            let child_header = export.child_header().clone();
            let parent_header = self.store.install_parent_header_export(
                &mut child.frame.delta,
                export,
                source.span(),
                tcx,
            )?;
            let mut gather = child.frame.begin_gather(self.store, source.span(), tcx)?;
            let actual = self.store.use_retained_type_at(
                child.frame.owner,
                gather.owner,
                child_header,
                source.span(),
            )?;
            let expected = self.store.use_retained_type_at(
                self.frame.owner,
                gather.owner,
                parent_header.clone(),
                source.span(),
            )?;
            self.store
                .constrain(&mut gather.delta, actual, expected, source.span(), tcx)?;
            child.frame.close_gather(self.store, gather, tcx)?;
            #[cfg(test)]
            super::record_completion_work(|work| {
                work.rec_order_entry_forwards += 1;
                work.rec_order_forward_gathers += 1;
            });
            let entries = self
                .frame
                .entered_rec_order
                .get_or_insert_with(Default::default);
            if let Some(existing) = entries
                .iter()
                .find(|entry| std::ptr::eq(entry.source, source))
            {
                assert_eq!(
                    existing.origin, origin,
                    "a binding retained two entered sources"
                );
            } else {
                entries.push(EnteredRecOrderSource::new(source, origin, parent_header));
            }
            child
                .frame
                .entered_rec_order
                .as_mut()
                .expect("entered source remains retained")[index]
                .forwarded = true;
        }
        Ok(contributed)
    }

    pub(super) fn reserve_prepared_parent_header_export(
        &mut self,
        child: &OpenRetainedPremise<'m>,
        header: crate::pass::typecheck_core::PreparedCloseType,
        span: Span,
    ) -> Result<ReservedParentHeaderExport, Error> {
        assert_eq!(
            child.parent, self.frame.owner,
            "a prepared projected header was reserved through a non-parent frontier"
        );
        self.store.reserve_prepared_parent_header_export(
            &mut self.frame.delta,
            &child.frame.delta,
            header,
            span,
        )
    }

    pub(super) fn parent_header_export_matches(
        &self,
        child: &OpenRetainedPremise<'m>,
        reserved: &ReservedParentHeaderExport,
    ) -> bool {
        assert_eq!(
            child.parent, self.frame.owner,
            "a projected header replay inspected a non-parent frontier"
        );
        reserved.matches_child_delta(&child.frame.delta)
    }

    pub(super) fn install_parent_header_export(
        &mut self,
        child: &mut OpenRetainedPremise<'m>,
        reserved: ReservedParentHeaderExport,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<ScopedType, Error> {
        assert_eq!(
            child.parent, self.frame.owner,
            "a projected header was installed through a non-parent frontier"
        );
        self.store
            .install_parent_header_export(&mut child.frame.delta, reserved, span, tcx)
    }

    pub(super) fn reserve_projected_header_hole(
        &mut self,
        span: Span,
    ) -> Result<ScopedType, Error> {
        let owner = self.frame.owner;
        let goal = self.reserve_goal_in_live_delta(
            crate::ast::Kind::Star,
            GoalSolutionPolicy::PolytypeAllowed,
            GoalOrigin::named(span, GoalRole::TypeArgument, "projected header"),
        )?;
        self.store.scoped_goal_at(goal, Vec::new(), owner, span)
    }

    pub(super) fn reserve_goal_in_live_delta(
        &mut self,
        kind: crate::ast::Kind,
        solution_policy: GoalSolutionPolicy,
        origin: GoalOrigin,
    ) -> Result<crate::ast::TypeGoalRef, Error> {
        self.store
            .reserve_goal_in_live_delta(&mut self.frame.delta, kind, solution_policy, origin)
    }

    pub(super) fn activate_reserved_goal_at(
        &mut self,
        reserved: ReservedGoalRef,
        diagnostic_span: Span,
        occurrence_span: Span,
    ) -> Result<(crate::ast::TypeGoalRef, ScopedType), Error> {
        self.store.activate_reserved_goal_at(
            self.goal_owner,
            reserved,
            self.frame.owner,
            diagnostic_span,
            occurrence_span,
        )
    }

    pub(super) fn reserve_child_with<T>(
        &mut self,
        kind: GoalOwnerKind,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
        plan: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<T, Error>,
    ) -> Result<(FrontierChildReservation, T), Error> {
        FrontierFrame::reserve_child_with(self.store, self.frame, kind, span, tcx, plan)
    }

    pub(super) fn reserve_lexical_child_with<T>(
        &mut self,
        kind: GoalOwnerKind,
        binders: &[super::super::RetainedTypeBinder<'m>],
        span: Span,
        plan: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<T, Error>,
    ) -> Result<(FrontierChildReservation, T), Error> {
        FrontierFrame::reserve_lexical_child_with(self.store, self.frame, kind, binders, span, plan)
    }

    pub(super) fn select_child_with<T, U>(
        &mut self,
        kind: GoalOwnerKind,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
        select: impl FnOnce(&mut GoalStore, TypeGoalOwner) -> Result<PlanningSelection<T, U>, Error>,
    ) -> Result<PlanningSelection<(FrontierChildReservation, T), U>, Error> {
        FrontierFrame::select_child_with(self.store, self.frame, kind, span, tcx, select)
    }

    pub(super) fn open_reserved_child(
        &self,
        reservation: FrontierChildReservation,
    ) -> Result<OpenRetainedPremise<'m>, Error> {
        reservation.open(self.store)
    }

    pub(super) fn retained_value_frontier<'a>(
        &'a mut self,
        open: &'a mut OpenRetainedPremise<'m>,
    ) -> ConnectedCallFrontier<'a, 'm> {
        let goal_owner = open.frame.owner;
        ConnectedCallFrontier {
            store: self.store,
            goal_owner,
            frame: &mut open.frame,
        }
    }

    pub(super) fn connected_call_frontier<'a>(
        &'a mut self,
        open: &'a mut OpenRetainedPremise<'m>,
    ) -> ConnectedCallFrontier<'a, 'm> {
        ConnectedCallFrontier {
            store: self.store,
            goal_owner: self.goal_owner,
            frame: &mut open.frame,
        }
    }

    pub(super) fn close_child(
        &mut self,
        child: OpenRetainedPremise<'m>,
        outputs: Vec<(ScopedType, GoalEscape)>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        FrontierFrame::close_retained_premise(child, self.store, self.frame, outputs, tcx)
    }

    pub(super) fn close_prepared_child(
        &mut self,
        child: OpenRetainedPremise<'m>,
        outputs: Vec<(PreparedCloseType, GoalEscape)>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        FrontierFrame::close_prepared_retained_premise(child, self.store, self.frame, outputs, tcx)
    }

    pub(super) fn stage_rec_order(
        &mut self,
        source: &'m crate::ast::Expr<Lowered>,
        state: PendingRecOrderState<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        self.frame.stage_rec_order(self.store, source, state, tcx)
    }

    pub(super) fn rec_order_runtime_for_use(
        &self,
        source: &'m crate::ast::Expr<Lowered>,
    ) -> Option<ScopedType> {
        self.frame.rec_order_runtime_for_use(source)
    }

    pub(super) fn take_rec_order_observation(
        &mut self,
        source: &'m crate::ast::Expr<Lowered>,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<Option<(Option<ScopedType>, bool)>, Error> {
        self.frame
            .take_rec_order_observation(self.store, source, tcx)
    }

    pub(super) fn begin_gather(
        &mut self,
        span: Span,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<GatherFrame, Error> {
        self.frame.begin_gather(self.store, span, tcx)
    }

    pub(super) fn close_gather(
        &mut self,
        gather: GatherFrame,
        tcx: &TypeCtx<'m, '_, Lowered>,
    ) -> Result<(), Error> {
        self.frame.close_gather(self.store, gather, tcx)
    }

    pub(super) fn retain_structural_foralls(
        &mut self,
        ty: InternedType<Lowered>,
        span: Span,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
    ) -> Result<ScopedType, Error> {
        FrontierFrame::retain_structural_foralls(self.store, self.frame, ty, span, tcx)
    }
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::{Kind, Meta, Type, TypeParam};
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::parse;
    use crate::pass::resolve::Package;
    use crate::pass::typecheck_full::Elaborations;
    use crate::pipeline::Pipeline;
    use std::path::{Path, PathBuf};

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn package(source: &str) -> Package<Lowered> {
        let module = parse(source).expect("parse test module");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower test module");
        Package::build(Path::new(""), modules, None).expect("build test package")
    }

    fn path_ty(name: &str) -> Type<Lowered> {
        Type::synth_path(vec![name.to_owned()], Vec::new(), sp())
    }

    fn forall_identity(binder: &str) -> Type<Lowered> {
        Type::Forall {
            param: TypeParam {
                name: binder.to_owned(),
                span: sp(),
                kind: None,
            },
            body: Box::new(Type::synth_function(
                vec![path_ty(binder)],
                path_ty(binder),
                sp(),
            )),
            meta: Meta::new(sp()),
        }
    }

    #[test]
    #[should_panic(expected = "RelationProbe escaped its symmetric relation host")]
    fn relation_probe_cannot_advance_outside_a_relation_host() {
        let _ = FrontierMode::RelationProbe.successor(false);
    }

    fn runtime(
        exact_ty: InternedType<Lowered>,
        source_checked: bool,
    ) -> PendingRecOrderState<Lowered> {
        PendingRecOrderState::Runtime {
            exact_ty,
            source_checked,
        }
    }

    fn expect_runtime(state: PendingRecOrderState<Lowered>) -> (InternedType<Lowered>, bool) {
        let PendingRecOrderState::Runtime {
            exact_ty,
            source_checked,
        } = state
        else {
            panic!("expected a runtime recursive-order state")
        };
        (exact_ty, source_checked)
    }

    #[test]
    fn rec_order_join_is_commutative_idempotent_and_runtime_dominant() {
        let package = package(
            "module main;
             host type N;
             type Alias = N;",
        );
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let n = tcx.intern_type(&path_ty("N"));
        let alias = tcx.intern_type(&path_ty("Alias"));

        assert!(matches!(
            join_rec_order_state(
                PendingRecOrderState::Pending,
                PendingRecOrderState::TypeOnly,
                sp(),
                &tcx,
            )
            .expect("pending is the join identity"),
            PendingRecOrderState::TypeOnly
        ));
        assert!(matches!(
            join_rec_order_state(
                PendingRecOrderState::TypeOnly,
                PendingRecOrderState::Pending,
                sp(),
                &tcx,
            )
            .expect("pending is the join identity in either order"),
            PendingRecOrderState::TypeOnly
        ));
        assert!(matches!(
            join_rec_order_state(
                PendingRecOrderState::TypeOnly,
                PendingRecOrderState::TypeOnly,
                sp(),
                &tcx,
            )
            .expect("type-only observations are idempotent"),
            PendingRecOrderState::TypeOnly
        ));

        for state in [
            join_rec_order_state(
                PendingRecOrderState::TypeOnly,
                runtime(n.clone(), false),
                sp(),
                &tcx,
            ),
            join_rec_order_state(
                runtime(n.clone(), false),
                PendingRecOrderState::TypeOnly,
                sp(),
                &tcx,
            ),
            join_rec_order_state(
                PendingRecOrderState::Pending,
                runtime(n.clone(), false),
                sp(),
                &tcx,
            ),
            join_rec_order_state(
                runtime(n.clone(), false),
                PendingRecOrderState::Pending,
                sp(),
                &tcx,
            ),
        ] {
            let (exact_ty, source_checked) =
                expect_runtime(state.expect("runtime observations dominate"));
            assert_eq!(display_type(exact_ty.as_type()), display_type(n.as_type()));
            assert!(!source_checked);
        }

        let (same_ty, same_checked) = expect_runtime(
            join_rec_order_state(
                runtime(n.clone(), false),
                runtime(n.clone(), true),
                sp(),
                &tcx,
            )
            .expect("identical runtime observations are idempotent"),
        );
        let (normalized_n, _) = crate::pass::typecheck_core::aliases::unfold_and_qualify_state(
            n.as_type(),
            &tcx.env.alias_ctx(),
            n.identity_is_canonical(),
        );
        assert_eq!(display_type(same_ty.as_type()), display_type(&normalized_n));
        assert!(same_checked, "one completed source check proves the join");

        let (alias_first, alias_first_checked) = expect_runtime(
            join_rec_order_state(
                runtime(alias.clone(), false),
                runtime(n.clone(), true),
                sp(),
                &tcx,
            )
            .expect("an alias and its terminal type are equivalent"),
        );
        let (terminal_first, terminal_first_checked) = expect_runtime(
            join_rec_order_state(runtime(n.clone(), true), runtime(alias, false), sp(), &tcx)
                .expect("equivalent runtime observations join in either order"),
        );
        assert_eq!(
            display_type(alias_first.as_type()),
            display_type(terminal_first.as_type()),
            "the exact joined representative must not depend on observation order"
        );
        assert_eq!(
            alias_first.identity_is_canonical(),
            terminal_first.identity_is_canonical(),
            "canonical identity must not depend on observation order"
        );
        assert!(alias_first_checked && terminal_first_checked);

        let forall_a = tcx.intern_type(&forall_identity("A"));
        let forall_b = tcx.intern_type(&forall_identity("B"));
        let (a_first, a_first_checked) = expect_runtime(
            join_rec_order_state(
                runtime(forall_a.clone(), false),
                runtime(forall_b.clone(), true),
                sp(),
                &tcx,
            )
            .expect("alpha-renamed function schemes are equivalent"),
        );
        let (b_first, b_first_checked) = expect_runtime(
            join_rec_order_state(
                runtime(forall_b, true),
                runtime(forall_a, false),
                sp(),
                &tcx,
            )
            .expect("alpha-renamed function schemes join in either order"),
        );
        assert_eq!(
            a_first.identity_is_canonical(),
            b_first.identity_is_canonical(),
            "alpha-renamed input order must not change canonical identity"
        );
        assert_eq!(
            display_type(a_first.as_type()),
            display_type(b_first.as_type()),
            "both alpha-renamed orders must produce one exact presentation"
        );
        assert!(a_first_checked && b_first_checked);
    }

    #[test]
    fn rec_order_join_rejects_distinct_unused_forall_binder_kinds() {
        let package = package("module main; host type N;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let phantom_scheme = |name: &str, kind: Kind| {
            InternedType::fresh(Type::Forall {
                param: TypeParam {
                    name: name.to_owned(),
                    span: sp(),
                    kind: Some(kind),
                },
                body: Box::new(Type::synth_function(vec![path_ty("N")], path_ty("N"), sp())),
                meta: Meta::new(sp()),
            })
        };
        let forall_star = phantom_scheme("A", Kind::Star);
        let forall_constructor = phantom_scheme("F", Kind::arrow_chain(1));

        assert!(
            join_rec_order_state(
                runtime(forall_star, false),
                runtime(forall_constructor, true),
                sp(),
                &tcx,
            )
            .is_err(),
            "distinct unused binder kinds must not join as one exact runtime type"
        );
    }

    #[test]
    fn rec_order_join_canonicalizes_aliases_nested_in_function_types() {
        let package = package(
            "module main;
             host type N;
             type Alias = N;",
        );
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let alias_function = InternedType::fresh(Type::synth_function(
            vec![path_ty("Alias")],
            path_ty("N"),
            sp(),
        ));
        let terminal_function =
            InternedType::fresh(Type::synth_function(vec![path_ty("N")], path_ty("N"), sp()));

        let mut presentations = Vec::new();
        for (first, second) in [
            (alias_function.clone(), terminal_function.clone()),
            (terminal_function, alias_function),
        ] {
            let (joined, source_checked) = expect_runtime(
                join_rec_order_state(runtime(first, false), runtime(second, true), sp(), &tcx)
                    .expect("nested aliases must join by semantic type equivalence"),
            );
            assert!(source_checked);
            presentations.push(display_type(joined.as_type()));
        }
        assert_eq!(presentations[0], presentations[1]);
        assert_eq!(presentations[0], "main.N -> main.N");
    }

    #[test]
    fn rec_order_join_preserves_forall_shadowing_while_unfolding_nested_aliases() {
        let package = package(
            "module main;
             host type N;
             type Alias = N;
             type Other = N;",
        );
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let shadowed = InternedType::fresh(Type::Forall {
            param: TypeParam {
                name: "Alias".to_owned(),
                span: sp(),
                kind: None,
            },
            body: Box::new(Type::synth_function(
                vec![Type::Product {
                    left: Box::new(path_ty("Alias")),
                    right: Box::new(path_ty("Other")),
                    meta: Meta::new(sp()),
                }],
                path_ty("Alias"),
                sp(),
            )),
            meta: Meta::new(sp()),
        });
        let terminal = InternedType::fresh(Type::Forall {
            param: TypeParam {
                name: "T".to_owned(),
                span: sp(),
                kind: None,
            },
            body: Box::new(Type::synth_function(
                vec![Type::Product {
                    left: Box::new(path_ty("T")),
                    right: Box::new(path_ty("N")),
                    meta: Meta::new(sp()),
                }],
                path_ty("T"),
                sp(),
            )),
            meta: Meta::new(sp()),
        });

        let mut presentations = Vec::new();
        for (first, second) in [(shadowed.clone(), terminal.clone()), (terminal, shadowed)] {
            let (joined, source_checked) = expect_runtime(
                join_rec_order_state(runtime(first, false), runtime(second, true), sp(), &tcx)
                    .expect("a forall binder must shadow a same-named module alias"),
            );
            assert!(source_checked);
            presentations.push(display_type(joined.as_type()));
        }
        assert_eq!(presentations[0], presentations[1]);
        assert!(
            presentations[0].contains("main.N"),
            "the unrelated nested alias must unfold: {}",
            presentations[0]
        );
        assert!(
            !presentations[0].contains("main.Alias"),
            "the forall binder must not resolve as the same-named module alias: {}",
            presentations[0]
        );
    }

    #[test]
    fn rec_order_join_canonicalizes_aliases_in_applied_type_arguments() {
        let package = package(
            "module main;
             host type N;
             host type Box[A];
             type Alias = N;",
        );
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let applied_alias = InternedType::fresh(Type::synth_path(
            vec!["Box".to_owned()],
            vec![path_ty("Alias")],
            sp(),
        ));
        let applied_terminal = InternedType::fresh(Type::synth_path(
            vec!["Box".to_owned()],
            vec![path_ty("N")],
            sp(),
        ));

        let mut presentations = Vec::new();
        for (first, second) in [
            (applied_alias.clone(), applied_terminal.clone()),
            (applied_terminal, applied_alias),
        ] {
            let (joined, source_checked) = expect_runtime(
                join_rec_order_state(runtime(first, false), runtime(second, true), sp(), &tcx)
                    .expect("an alias nested in a nominal type argument must join"),
            );
            assert!(source_checked);
            presentations.push(display_type(joined.as_type()));
        }
        assert_eq!(presentations[0], presentations[1]);
        assert_eq!(presentations[0], "main.Box(main.N)");
    }

    #[test]
    fn rec_order_join_canonicalizes_function_abi_metadata_in_every_order() {
        let package = package("module main; host type N;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let n = path_ty("N");
        let product = Type::Product {
            left: Box::new(n.clone()),
            right: Box::new(n.clone()),
            meta: Meta::new(sp()),
        };
        let abi_one = InternedType::fresh(Type::synth_function(vec![product], n.clone(), sp()));
        let abi_two =
            InternedType::fresh(Type::synth_function(vec![n.clone(), n], path_ty("N"), sp()));
        assert!(matches!(
            abi_one.as_type(),
            Type::Function { abi_arity: 1, .. }
        ));
        assert!(matches!(
            abi_two.as_type(),
            Type::Function { abi_arity: 2, .. }
        ));

        for existing_checked in [false, true] {
            for incoming_checked in [false, true] {
                for reverse in [false, true] {
                    let (first, second) = if reverse {
                        (
                            runtime(abi_two.clone(), existing_checked),
                            runtime(abi_one.clone(), incoming_checked),
                        )
                    } else {
                        (
                            runtime(abi_one.clone(), existing_checked),
                            runtime(abi_two.clone(), incoming_checked),
                        )
                    };
                    let (joined, source_checked) = expect_runtime(
                        join_rec_order_state(first, second, sp(), &tcx)
                            .expect("ABI metadata does not change semantic type identity"),
                    );
                    assert_eq!(source_checked, existing_checked || incoming_checked);
                    assert!(matches!(
                        joined.as_type(),
                        Type::Function { abi_arity: 1, .. }
                    ));
                }
            }
        }

        let canonical_zero =
            InternedType::fresh(Type::synth_function(Vec::new(), path_ty("N"), sp()));
        let mut noncanonical_one = canonical_zero.clone_type();
        let Type::Function { abi_arity, .. } = &mut noncanonical_one else {
            unreachable!("synthesized zero-domain function was not a Function type")
        };
        *abi_arity = 1;
        let noncanonical_one = InternedType::fresh(noncanonical_one);
        for (first, second) in [
            (canonical_zero.clone(), noncanonical_one.clone()),
            (noncanonical_one, canonical_zero),
        ] {
            let (joined, _) = expect_runtime(
                join_rec_order_state(runtime(first, false), runtime(second, false), sp(), &tcx)
                    .expect("unit-domain ABI metadata is canonicalizable"),
            );
            assert!(matches!(
                joined.as_type(),
                Type::Function { abi_arity: 0, .. }
            ));
        }
    }

    #[test]
    fn rec_order_join_preserves_an_ambient_binder_shadowing_a_module_alias() {
        let package = package(
            "module main;
             host type N;
             type A = N;",
        );
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        tcx.push_type_param("A", sp());
        let binder_a = tcx.intern_type(&path_ty("A"));

        let (first, first_checked) = expect_runtime(
            join_rec_order_state(
                runtime(binder_a.clone(), false),
                runtime(binder_a.clone(), true),
                sp(),
                &tcx,
            )
            .expect("the repeated ambient binder observation joins"),
        );
        let (second, second_checked) = expect_runtime(
            join_rec_order_state(
                runtime(binder_a.clone(), true),
                runtime(binder_a.clone(), false),
                sp(),
                &tcx,
            )
            .expect("the repeated ambient binder observation joins in either order"),
        );

        assert_eq!(display_type(first.as_type()), "A");
        assert_eq!(display_type(second.as_type()), "A");
        assert_eq!(
            first.identity_is_canonical(),
            second.identity_is_canonical(),
            "observation order must not change the binder handle's provenance"
        );
        assert!(first_checked && second_checked);
    }

    #[test]
    fn rec_order_join_canonicalizes_applied_alias_of_an_ambient_binder() {
        let package = package(
            "module main;
             host type N;
             type A = N;
             type Id[X] = X;",
        );
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        tcx.push_type_param("A", sp());
        let binder = tcx.intern_type(&path_ty("A"));
        let identity_of_binder = tcx.intern_type(&Type::synth_path(
            vec!["Id".to_owned()],
            vec![path_ty("A")],
            sp(),
        ));

        let mut observations = Vec::new();
        for (first, second) in [
            (identity_of_binder.clone(), binder.clone()),
            (binder, identity_of_binder),
        ] {
            let (joined, source_checked) = expect_runtime(
                join_rec_order_state(runtime(first, false), runtime(second, true), sp(), &tcx)
                    .expect("an applied identity alias and its ambient binder must join"),
            );
            observations.push((
                display_type(joined.as_type()),
                joined.identity_is_canonical(),
                source_checked,
            ));
        }

        assert_eq!(observations[0], observations[1]);
        assert_eq!(observations[0].0, "A");
        assert!(observations[0].2);
    }

    #[test]
    fn rec_order_join_alpha_presentation_is_independent_of_interner_history() {
        fn join_after_interning(first_binder: &str, second_binder: &str) -> InternedType<Lowered> {
            let package = package("module main;");
            let module = &package.module("main").expect("main module").module;
            let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
                .expect("module environment");
            let mut elaborations = Elaborations::new();
            let tcx = TypeCtx::new(&env, &mut elaborations);
            let first = tcx.intern_type(&forall_identity(first_binder));
            let second = tcx.intern_type(&forall_identity(second_binder));
            expect_runtime(
                join_rec_order_state(runtime(first, false), runtime(second, true), sp(), &tcx)
                    .expect("alpha-equivalent Runtime observations join"),
            )
            .0
        }

        let a_first = join_after_interning("A", "B");
        let b_first = join_after_interning("B", "A");
        assert_eq!(
            display_type(a_first.as_type()),
            display_type(b_first.as_type()),
            "the exact type copied into a recursive-order annotation must not expose the interner's first-writer binder spelling"
        );
        assert_eq!(
            a_first.identity_is_canonical(),
            b_first.identity_is_canonical()
        );
    }

    #[test]
    fn rec_order_forwarding_rejects_distinct_entered_origins() {
        let package = package("module main;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(sp()),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        tcx.push_pending_rec_order("pending".to_owned(), &source, source.span());
        let (mut frontier, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin root");
        let mut children = Vec::new();
        for _ in 0..2 {
            let (reservation, ()) = frontier
                .reserve_root_child_with(
                    GoalOwnerKind::RetainedValue,
                    sp(),
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve entry frame");
            let mut child = frontier
                .open_reserved_child(reservation)
                .expect("open entry frame");
            let mut child_frontier = frontier.retained_value_frontier(&mut child);
            let (origin, ()) = child_frontier
                .reserve_child_with(
                    GoalOwnerKind::RetainedValue,
                    sp(),
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve source origin");
            let origin = child_frontier
                .open_reserved_child(origin)
                .expect("open source origin");
            let header = child_frontier
                .store()
                .scoped_type(
                    child_frontier.owner(),
                    tcx.intern_type(&Type::Unit {
                        meta: Meta::new(sp()),
                    }),
                    sp(),
                )
                .expect("scope entry header");
            child_frontier.record_rec_order_entry(&source, origin.owner(), header);
            children.push((child, origin));
        }
        assert!(
            frontier
                .forward_rec_order_entries(&mut children[0].0, &tcx)
                .expect("forward first source origin")
        );
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            frontier.forward_rec_order_entries(&mut children[1].0, &tcx)
        }))
        .expect_err("distinct source origins must not merge");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .expect("the invariant panic has a string payload");
        assert!(
            message.contains("a binding retained two entered sources"),
            "{message}"
        );
    }

    #[test]
    fn rec_order_root_singleton_uses_a_fresh_alpha_normalized_type() {
        let package = package("module main;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(Span::new(40, 41)),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let pending = tcx.push_pending_rec_order("pending".to_owned(), &source, source.span());
        let observed = tcx.intern_type(&forall_identity("SourceBinder"));
        let (mut frontier, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin root");
        frontier
            .stage_rec_order(&source, runtime(observed.clone(), false), &tcx)
            .expect("stage singleton Runtime observation");
        frontier
            .close_root_with(Vec::new(), &mut tcx, |outputs, _tcx| {
                assert!(outputs.is_empty());
                Ok(())
            })
            .expect("close root");

        let (exact_ty, source_checked) = expect_runtime(tcx.pending_rec_order_state(pending));
        assert!(!source_checked);
        assert!(
            !exact_ty.ptr_eq(&observed),
            "recursive-order output must not reuse an interner-owned presentation handle"
        );
        assert_eq!(exact_ty.span(), source.span());
        assert_eq!(
            display_type(exact_ty.as_type()),
            display_type(&forall_identity("A")),
            "even a singleton observation must receive the canonical bound-name presentation"
        );
    }

    #[test]
    fn rec_order_root_singleton_canonicalizes_function_abi_metadata() {
        let package = package("module main; host type N;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(Span::new(42, 43)),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let pending = tcx.push_pending_rec_order("pending".to_owned(), &source, source.span());
        let n = path_ty("N");
        let observed =
            InternedType::fresh(Type::synth_function(vec![n.clone(), n], path_ty("N"), sp()));
        assert!(matches!(
            observed.as_type(),
            Type::Function { abi_arity: 2, .. }
        ));
        let (mut frontier, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin root");
        frontier
            .stage_rec_order(&source, runtime(observed, true), &tcx)
            .expect("stage singleton Runtime observation");
        frontier
            .close_root_with(Vec::new(), &mut tcx, |outputs, _tcx| {
                assert!(outputs.is_empty());
                Ok(())
            })
            .expect("close root");

        let (exact_ty, source_checked) = expect_runtime(tcx.pending_rec_order_state(pending));
        assert!(source_checked);
        assert!(matches!(
            exact_ty.as_type(),
            Type::Function { abi_arity: 1, .. }
        ));
    }

    #[test]
    fn rec_order_root_singleton_canonicalizes_nested_function_abi_metadata() {
        let package = package("module main; host type N;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(Span::new(44, 45)),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let pending = tcx.push_pending_rec_order("pending".to_owned(), &source, source.span());
        let n = path_ty("N");
        let inner = Type::synth_function(vec![n.clone(), n.clone()], n.clone(), sp());
        let observed = InternedType::fresh(Type::synth_function(vec![n], inner, sp()));
        assert!(matches!(
            observed.as_type(),
            Type::Function {
                abi_arity: 1,
                ret,
                ..
            } if matches!(ret.as_ref(), Type::Function { abi_arity: 2, .. })
        ));
        let (mut frontier, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin root");
        frontier
            .stage_rec_order(&source, runtime(observed, true), &tcx)
            .expect("stage nested Runtime observation");
        frontier
            .close_root_with(Vec::new(), &mut tcx, |outputs, _tcx| {
                assert!(outputs.is_empty());
                Ok(())
            })
            .expect("close root");

        let (exact_ty, source_checked) = expect_runtime(tcx.pending_rec_order_state(pending));
        assert!(source_checked);
        assert!(matches!(
            exact_ty.as_type(),
            Type::Function {
                abi_arity: 1,
                ret,
                ..
            } if matches!(ret.as_ref(), Type::Function { abi_arity: 1, .. })
        ));
    }

    #[test]
    fn rec_order_child_completion_and_observation_orders_have_one_join() {
        fn run(
            reverse_completion: bool,
            swap_observations: bool,
            type_only_first: bool,
        ) -> (InternedType<Lowered>, bool) {
            let package = package("module main;");
            let module = &package.module("main").expect("main module").module;
            let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
                .expect("module environment");
            let source = crate::ast::Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(Span::new(50, 51)),
            };
            let mut elaborations = Elaborations::new();
            let mut tcx = TypeCtx::new(&env, &mut elaborations);
            let pending = tcx.push_pending_rec_order("pending".to_owned(), &source, source.span());
            let forall_a = tcx.intern_type(&forall_identity("First"));
            let forall_b = tcx.intern_type(&forall_identity("Second"));
            let (mut frontier, ()) =
                OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                    .expect("begin root");
            if type_only_first {
                frontier
                    .stage_rec_order(&source, PendingRecOrderState::TypeOnly, &tcx)
                    .expect("stage leading TypeOnly observation");
            }

            let (first_reservation, ()) = frontier
                .reserve_root_child_with(
                    GoalOwnerKind::RetainedValue,
                    Span::new(52, 53),
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve first child");
            let (second_reservation, ()) = frontier
                .reserve_root_child_with(
                    GoalOwnerKind::RetainedValue,
                    Span::new(54, 55),
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve second child");
            let mut first = frontier
                .open_reserved_child(first_reservation)
                .expect("open first child");
            let mut second = frontier
                .open_reserved_child(second_reservation)
                .expect("open second child");
            let (first_ty, first_checked, second_ty, second_checked) = if swap_observations {
                (forall_b, true, forall_a, false)
            } else {
                (forall_a, false, forall_b, true)
            };
            first
                .stage_rec_order(
                    frontier.store(),
                    &source,
                    runtime(first_ty, first_checked),
                    &tcx,
                )
                .expect("stage first child observation");
            second
                .stage_rec_order(
                    frontier.store(),
                    &source,
                    runtime(second_ty, second_checked),
                    &tcx,
                )
                .expect("stage second child observation");

            if reverse_completion {
                assert!(
                    frontier
                        .close_root_child(second, Vec::new(), &tcx)
                        .expect("close second child first")
                        .is_empty()
                );
                assert!(
                    frontier
                        .close_root_child(first, Vec::new(), &tcx)
                        .expect("close first child second")
                        .is_empty()
                );
            } else {
                assert!(
                    frontier
                        .close_root_child(first, Vec::new(), &tcx)
                        .expect("close first child first")
                        .is_empty()
                );
                assert!(
                    frontier
                        .close_root_child(second, Vec::new(), &tcx)
                        .expect("close second child second")
                        .is_empty()
                );
            }
            if !type_only_first {
                frontier
                    .stage_rec_order(&source, PendingRecOrderState::TypeOnly, &tcx)
                    .expect("stage trailing TypeOnly observation");
            }
            frontier
                .close_root_with(Vec::new(), &mut tcx, |outputs, _tcx| {
                    assert!(outputs.is_empty());
                    Ok(())
                })
                .expect("close root");
            expect_runtime(tcx.pending_rec_order_state(pending))
        }

        let mut presentations = Vec::new();
        for reverse_completion in [false, true] {
            for swap_observations in [false, true] {
                let (exact_ty, source_checked) = run(
                    reverse_completion,
                    swap_observations,
                    reverse_completion == swap_observations,
                );
                assert!(source_checked, "source_checked must join with logical OR");
                presentations.push((display_type(exact_ty.as_type()), exact_ty.span()));
            }
        }
        assert!(presentations.iter().all(|presentation| {
            presentation.0 == presentations[0].0 && presentation.1 == presentations[0].1
        }));
        assert_eq!(presentations[0].0, display_type(&forall_identity("A")));
        assert_eq!(presentations[0].1, Span::new(50, 51));
    }

    #[test]
    fn rec_order_child_filters_irrelevant_shadow_before_to_owner_transfer() {
        let package = package("module main; host type N;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(Span::new(60, 61)),
        };
        let irrelevant = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(Span::new(62, 63)),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let pending = tcx.push_pending_rec_order("pending".to_owned(), &source, source.span());
        let parent_mark = tcx.save();
        let (mut frontier, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin root");

        tcx.push_type_param("ChildOnly", Span::new(64, 65));
        let (reservation, ()) = frontier
            .reserve_root_child_with(
                GoalOwnerKind::RetainedValue,
                Span::new(66, 67),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve child");
        let mut child = frontier
            .open_reserved_child(reservation)
            .expect("open child");
        child
            .stage_rec_order(
                frontier.store(),
                &source,
                runtime(tcx.intern_type(&path_ty("N")), true),
                &tcx,
            )
            .expect("stage relevant child observation");
        child
            .stage_rec_order(
                frontier.store(),
                &irrelevant,
                runtime(tcx.intern_type(&path_ty("ChildOnly")), false),
                &tcx,
            )
            .expect("an irrelevant child source is filtered before scoping");
        tcx.restore(parent_mark);

        assert!(
            frontier
                .close_root_child(child, Vec::new(), &tcx)
                .expect("only the relevant binder-free observation crosses ToOwner")
                .is_empty()
        );
        frontier
            .close_root_with(Vec::new(), &mut tcx, |outputs, _tcx| {
                assert!(outputs.is_empty());
                Ok(())
            })
            .expect("close root");
        let (exact_ty, source_checked) = expect_runtime(tcx.pending_rec_order_state(pending));
        assert_eq!(display_type(exact_ty.as_type()), "main.N");
        assert!(source_checked);
    }

    #[test]
    fn rec_order_prepared_child_retains_observation_until_root_commit() {
        let package = package("module main; host type N;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(Span::new(70, 71)),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let pending = tcx.push_pending_rec_order("pending".to_owned(), &source, source.span());
        let n = tcx.intern_type(&path_ty("N"));
        let (mut frontier, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin root");
        let destination = frontier.owner();
        let (reservation, ()) = frontier
            .reserve_root_child_with(
                GoalOwnerKind::RetainedValue,
                Span::new(72, 73),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve prepared child");
        let mut child = frontier
            .open_reserved_child(reservation)
            .expect("open prepared child");
        let primary = frontier
            .store()
            .scoped_type(child.owner(), n.clone(), sp())
            .expect("scope prepared primary output");
        let primary = frontier
            .store()
            .zonk_for_close(&child.frame.delta, primary, &tcx)
            .expect("prepare primary output");
        child
            .stage_rec_order(frontier.store(), &source, runtime(n, true), &tcx)
            .expect("stage prepared-child recursive-order observation");
        let marker = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: Span::new(74, 75),
        };
        child.publication_mut().mark_value_at_type_slot(marker, 0);

        let outputs = frontier
            .close_prepared_root_child(
                child,
                vec![(primary, GoalEscape::ToOwner(destination))],
                &tcx,
            )
            .expect("prepared child retains its observation");
        assert_eq!(
            outputs.len(),
            1,
            "prepared child returns its primary output"
        );
        assert!(matches!(
            tcx.pending_rec_order_state(pending),
            PendingRecOrderState::Pending
        ));
        assert!(
            !tcx.elaborations.is_value_at_type_slot("main", marker.id, 0),
            "child close must not publish before root commit"
        );

        frontier
            .close_root_with(Vec::new(), &mut tcx, |outputs, _tcx| {
                assert!(outputs.is_empty());
                Ok(())
            })
            .expect("commit prepared-child observation at root");
        let (exact_ty, source_checked) = expect_runtime(tcx.pending_rec_order_state(pending));
        assert_eq!(display_type(exact_ty.as_type()), "main.N");
        assert!(source_checked);
        assert!(tcx.elaborations.is_value_at_type_slot("main", marker.id, 0));
    }

    #[test]
    fn rec_order_prepared_child_rejects_child_only_binder_before_commit() {
        let package = package("module main;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(Span::new(76, 77)),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let pending = tcx.push_pending_rec_order("pending".to_owned(), &source, source.span());
        let parent_mark = tcx.save();
        let (mut frontier, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin root");
        let destination = frontier.owner();
        tcx.push_type_param("ChildOnly", Span::new(78, 79));
        let (reservation, ()) = frontier
            .reserve_root_child_with(
                GoalOwnerKind::RetainedValue,
                Span::new(80, 81),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve prepared child");
        let mut child = frontier
            .open_reserved_child(reservation)
            .expect("open prepared child");
        let primary = frontier
            .store()
            .scoped_type(
                child.owner(),
                InternedType::fresh_canonical(Type::Unit {
                    meta: Meta::new(sp()),
                }),
                sp(),
            )
            .expect("scope prepared primary output");
        let primary = frontier
            .store()
            .zonk_for_close(&child.frame.delta, primary, &tcx)
            .expect("prepare primary output");
        child
            .stage_rec_order(
                frontier.store(),
                &source,
                runtime(tcx.intern_type(&path_ty("ChildOnly")), true),
                &tcx,
            )
            .expect("stage child-local prepared observation");
        let marker = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: Span::new(82, 83),
        };
        child.publication_mut().mark_value_at_type_slot(marker, 0);
        tcx.restore(parent_mark);

        let error = frontier
            .close_prepared_root_child(
                child,
                vec![(primary, GoalEscape::ToOwner(destination))],
                &tcx,
            )
            .expect_err("a prepared child-only binder must not escape");
        assert!(
            error
                .diagnostic()
                .message
                .contains("rigid type variable outside its destination scope"),
            "unexpected child-scope diagnostic: {}",
            error.diagnostic().message
        );
        assert!(matches!(
            tcx.pending_rec_order_state(pending),
            PendingRecOrderState::Pending
        ));
        assert!(
            !tcx.elaborations.is_value_at_type_slot("main", marker.id, 0),
            "a rejected prepared child must not publish metadata"
        );
    }

    #[test]
    fn rec_order_connected_child_rejects_a_child_only_binder_before_commit() {
        let package = package("module main;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(sp()),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let pending = tcx.push_pending_rec_order("pending".to_owned(), &source, sp());
        let parent_mark = tcx.save();
        let (mut frontier, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin root");
        tcx.push_type_param("ChildOnly", Span::new(1, 2));
        let (reservation, ()) = frontier
            .reserve_root_child_with(
                GoalOwnerKind::RetainedValue,
                Span::new(3, 4),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve child");
        let mut child = frontier
            .open_reserved_child(reservation)
            .expect("open child");
        child
            .stage_rec_order(
                frontier.store(),
                &source,
                runtime(tcx.intern_type(&path_ty("ChildOnly")), true),
                &tcx,
            )
            .expect("stage child-local recursive-order observation");
        let marker = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: Span::new(5, 6),
        };
        child.publication_mut().mark_value_at_type_slot(marker, 0);
        tcx.restore(parent_mark);

        let error = match frontier.close_root_child(child, Vec::new(), &tcx) {
            Err(error) => error,
            Ok(_) => panic!("a child-only binder escaped through ToOwner"),
        };
        assert!(
            error
                .diagnostic()
                .message
                .contains("rigid type variable outside its destination scope"),
            "unexpected child-scope diagnostic: {}",
            error.diagnostic().message
        );
        assert!(matches!(
            tcx.pending_rec_order_state(pending),
            PendingRecOrderState::Pending
        ));
        assert!(
            !tcx.elaborations.is_value_at_type_slot("main", marker.id, 0),
            "a rejected child close must not publish its metadata"
        );
    }

    #[test]
    fn rec_order_root_callback_and_fold_failures_do_not_commit_side_effects() {
        let package = package("module main; host type N; host type M;");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let conflict_source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(Span::new(90, 91)),
        };
        let callback_source = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(Span::new(92, 93)),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let conflict_pending = tcx.push_pending_rec_order(
            "conflict".to_owned(),
            &conflict_source,
            conflict_source.span(),
        );
        let callback_pending = tcx.push_pending_rec_order(
            "callback".to_owned(),
            &callback_source,
            callback_source.span(),
        );
        let n = tcx.intern_type(&path_ty("N"));
        let m = tcx.intern_type(&path_ty("M"));

        let conflict_marker = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: Span::new(94, 95),
        };
        let (mut conflict, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin conflicting root");
        conflict
            .root_publication_mut()
            .2
            .mark_value_at_type_slot(conflict_marker, 0);
        conflict
            .stage_rec_order(&conflict_source, runtime(n.clone(), false), &tcx)
            .expect("stage first conflicting observation");
        conflict
            .stage_rec_order(&conflict_source, runtime(m, true), &tcx)
            .expect("stage second conflicting observation");
        let rejected: Result<(), Error> =
            conflict.close_root_with(Vec::new(), &mut tcx, |outputs, _tcx| {
                assert!(outputs.is_empty());
                Ok(())
            });
        assert!(
            rejected.is_err(),
            "incompatible Runtime observations must reject"
        );
        assert!(matches!(
            tcx.pending_rec_order_state(conflict_pending),
            PendingRecOrderState::Pending
        ));
        assert!(
            !tcx.elaborations
                .is_value_at_type_slot("main", conflict_marker.id, 0),
            "a fold failure must not commit publication metadata"
        );

        let callback_marker = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: Span::new(96, 97),
        };
        let (mut callback, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin callback-failing root");
        callback
            .root_publication_mut()
            .2
            .mark_value_at_type_slot(callback_marker, 0);
        callback
            .stage_rec_order(&callback_source, runtime(n.clone(), true), &tcx)
            .expect("stage callback observation");
        let rejected: Result<(), Error> =
            callback.close_root_with(Vec::new(), &mut tcx, |_outputs, _tcx| {
                Err(Error::type_(sp(), "intentional test callback failure"))
            });
        assert!(rejected.is_err(), "the test callback must reject");
        assert!(matches!(
            tcx.pending_rec_order_state(callback_pending),
            PendingRecOrderState::Pending
        ));
        assert!(
            !tcx.elaborations
                .is_value_at_type_slot("main", callback_marker.id, 0),
            "a callback failure must not commit publication metadata"
        );

        let success_marker = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: Span::new(98, 99),
        };
        let (mut success, ()) =
            OrdinaryFrontier::begin_root_with(sp(), &tcx, |_store, _owner| Ok(()))
                .expect("begin successful root after failures");
        success
            .root_publication_mut()
            .2
            .mark_value_at_type_slot(success_marker, 0);
        success
            .stage_rec_order(&callback_source, runtime(n, true), &tcx)
            .expect("stage successful observation");
        success
            .close_root_with(Vec::new(), &mut tcx, |outputs, _tcx| {
                assert!(outputs.is_empty());
                Ok(())
            })
            .expect("a fresh root remains usable after both rejected transactions");
        let (_, source_checked) = expect_runtime(tcx.pending_rec_order_state(callback_pending));
        assert!(source_checked);
        assert!(
            tcx.elaborations
                .is_value_at_type_slot("main", success_marker.id, 0)
        );
        assert!(matches!(
            tcx.pending_rec_order_state(conflict_pending),
            PendingRecOrderState::Pending
        ));
    }
}
