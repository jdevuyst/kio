//! Owner-scoped, atomic publication of Lowered typechecker results.
//!
//! Planning records only opaque goal-store types. Nested owners close into
//! parent-scoped retained outputs, and the root close is the sole transition
//! to a delta that can update [`Elaborations`]. No publication state is
//! phase-generic, cloneable, or readable as a speculative elaboration table.

use crate::ast::{Lowered, NodeId, TypeGoalOwner};
use crate::error::Error;
use crate::pass::typecheck_core::{
    ClosedGoalOutput, ClosedPublicationOutput, GoalDelta, GoalDeltaAuthority, GoalEscape,
    GoalFreePublicationOutput, GoalStore, GoalTypeContext, PreparedCloseType, PreparedGoalClose,
    PreparedGoalCommit, PreparedRecOrderCommit, PublicationGoalEscape, PublicationGoalInput,
    ScopedType, TypeCtx,
};
#[cfg(test)]
use crate::pass::typecheck_core::{publication_validation_work, reset_publication_validation_work};
use crate::span::Span;

use super::{
    DeferredElaboration, DeferredUserElaboratorElaboration, Elaborations, Expr, InternedType,
    PrimeLocalTypeAlias, RecordedElaboration, ResolvedBinder, Type,
    UserElaboratorTemplateGeneratedImport,
};

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PublicationWorkCounters {
    command_freezes: usize,
    shape_freeze_visits: usize,
    payload_close_inputs: usize,
    payload_close_outputs: usize,
    final_shape_visits: usize,
    command_materializations: usize,
}

#[cfg(test)]
thread_local! {
    static PUBLICATION_WORK: std::cell::Cell<PublicationWorkCounters> =
        const { std::cell::Cell::new(PublicationWorkCounters {
            command_freezes: 0,
            shape_freeze_visits: 0,
            payload_close_inputs: 0,
            payload_close_outputs: 0,
            final_shape_visits: 0,
            command_materializations: 0,
        }) };
}

#[cfg(test)]
fn record_publication_work(update: impl FnOnce(&mut PublicationWorkCounters)) {
    PUBLICATION_WORK.with(|work| {
        let mut counters = work.get();
        update(&mut counters);
        work.set(counters);
    });
}

#[cfg(test)]
fn reset_publication_work() {
    PUBLICATION_WORK.with(|work| work.set(PublicationWorkCounters::default()));
}

#[cfg(test)]
fn publication_work() -> PublicationWorkCounters {
    PUBLICATION_WORK.with(std::cell::Cell::get)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationRootKind {
    GoalFree,
    FunctionScheme,
}

#[derive(Debug)]
struct PublicationValue<T> {
    value: T,
    root_kind: PublicationRootKind,
}

impl<T> PublicationValue<T> {
    fn goal_free(value: T) -> Self {
        Self {
            value,
            root_kind: PublicationRootKind::GoalFree,
        }
    }

    fn function_scheme(value: T) -> Self {
        Self {
            value,
            root_kind: PublicationRootKind::FunctionScheme,
        }
    }
}

/// One destination write whose type payload changes typestate as its inference
/// owner closes. The command shape is independent of that typestate: planning
/// carries pending [`PublicationGoalInput`] values, a nested close carries the
/// same opaque input in its retained typestate, and a root close carries one
/// of the goal-free publication proofs.
#[derive(Debug)]
enum PublicationCommand<T> {
    RecQuoteSourceType {
        source: crate::ast::ExpressionOccurrenceId,
        resolved: T,
    },
    RecQuoteExpansion {
        node_id: NodeId,
        source: crate::ast::ExpressionOccurrenceId,
        continuation: StagedLoweredElaboration<T>,
        public_type: T,
        runtime_type: T,
        ambient_type_names: std::collections::HashSet<String>,
    },
    TypeResolution {
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
        resolved: T,
    },
    InlayTypeArgs {
        call_site: crate::ast::ExpressionSite,
        callee_end: Span,
        slot_offset: usize,
        resolved: Vec<T>,
        binders: super::TypeBinderScope,
    },
    MarkValueAtTypeSlot {
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
    },
    CallSplit {
        call_site: crate::ast::ExpressionSite,
        split_after_slot: usize,
    },
    CallImplicitUnit {
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
    },
    CallResidualEta {
        call_site: crate::ast::ExpressionSite,
        residual: T,
    },
    PositionType {
        expr_span: Span,
        resolved: T,
        binders: super::TypeBinderScope,
    },
    PositionBinder {
        segment_span: Span,
        binder: ResolvedBinder,
    },
    LocalDecl {
        name: String,
        decl_span: Span,
        kind: crate::pass::typecheck_core::ResolvedBinderKind,
    },
    FnParamType {
        fn_site: crate::ast::ExpressionSite,
        value_param_index: usize,
        resolved: T,
    },
    InlayLetType {
        name_span: Span,
        resolved: T,
        binders: super::TypeBinderScope,
    },
    LiteralResolution {
        literal_site: crate::ast::ExpressionSite,
        resolved: T,
    },
    PrimeRequalificationBindings {
        imports: Vec<UserElaboratorTemplateGeneratedImport>,
        aliases: Vec<PrimeLocalTypeAlias>,
    },
    RecOrderBinding {
        node_id: NodeId,
        replacement: StagedLoweredElaboration<T>,
        resolved: T,
    },
    LoweredElaboration {
        node_id: NodeId,
        replacement: StagedLoweredElaboration<T>,
    },
    UserElaborator {
        obligation: DeferredUserElaboratorElaboration,
    },
}

impl<T> PublicationCommand<T> {
    fn map_type_slots<U>(self, map: &mut impl FnMut(T) -> U) -> PublicationCommand<U> {
        match self {
            Self::RecQuoteSourceType { source, resolved } => {
                PublicationCommand::RecQuoteSourceType {
                    source,
                    resolved: map(resolved),
                }
            }
            Self::RecQuoteExpansion {
                node_id,
                source,
                continuation,
                public_type,
                runtime_type,
                ambient_type_names,
            } => PublicationCommand::RecQuoteExpansion {
                node_id,
                source,
                continuation: continuation.map_type_slots(map),
                public_type: map(public_type),
                runtime_type: map(runtime_type),
                ambient_type_names,
            },
            Self::TypeResolution {
                call_site,
                slot_index,
                resolved,
            } => PublicationCommand::TypeResolution {
                call_site,
                slot_index,
                resolved: map(resolved),
            },
            Self::InlayTypeArgs {
                call_site,
                callee_end,
                slot_offset,
                resolved,
                binders,
            } => PublicationCommand::InlayTypeArgs {
                call_site,
                callee_end,
                slot_offset,
                resolved: resolved.into_iter().map(map).collect(),
                binders,
            },
            Self::MarkValueAtTypeSlot {
                call_site,
                slot_index,
            } => PublicationCommand::MarkValueAtTypeSlot {
                call_site,
                slot_index,
            },
            Self::CallSplit {
                call_site,
                split_after_slot,
            } => PublicationCommand::CallSplit {
                call_site,
                split_after_slot,
            },
            Self::CallImplicitUnit {
                call_site,
                slot_index,
            } => PublicationCommand::CallImplicitUnit {
                call_site,
                slot_index,
            },
            Self::CallResidualEta {
                call_site,
                residual,
            } => PublicationCommand::CallResidualEta {
                call_site,
                residual: map(residual),
            },
            Self::PositionType {
                expr_span,
                resolved,
                binders,
            } => PublicationCommand::PositionType {
                expr_span,
                resolved: map(resolved),
                binders,
            },
            Self::PositionBinder {
                segment_span,
                binder,
            } => PublicationCommand::PositionBinder {
                segment_span,
                binder,
            },
            Self::LocalDecl {
                name,
                decl_span,
                kind,
            } => PublicationCommand::LocalDecl {
                name,
                decl_span,
                kind,
            },
            Self::FnParamType {
                fn_site,
                value_param_index,
                resolved,
            } => PublicationCommand::FnParamType {
                fn_site,
                value_param_index,
                resolved: map(resolved),
            },
            Self::InlayLetType {
                name_span,
                resolved,
                binders,
            } => PublicationCommand::InlayLetType {
                name_span,
                resolved: map(resolved),
                binders,
            },
            Self::LiteralResolution {
                literal_site,
                resolved,
            } => PublicationCommand::LiteralResolution {
                literal_site,
                resolved: map(resolved),
            },
            Self::PrimeRequalificationBindings { imports, aliases } => {
                PublicationCommand::PrimeRequalificationBindings { imports, aliases }
            }
            Self::RecOrderBinding {
                node_id,
                replacement,
                resolved,
            } => PublicationCommand::RecOrderBinding {
                node_id,
                replacement: replacement.map_type_slots(map),
                resolved: map(resolved),
            },
            Self::LoweredElaboration {
                node_id,
                replacement,
            } => PublicationCommand::LoweredElaboration {
                node_id,
                replacement: replacement.map_type_slots(map),
            },
            Self::UserElaborator { obligation } => {
                PublicationCommand::UserElaborator { obligation }
            }
        }
    }
}

/// A private Lowered replacement template whose goal-bearing type roots travel
/// through the same publication typestate as scalar metadata. The template is
/// never exposed before every slot closes and is materialized.
#[derive(Debug)]
struct StagedLoweredElaboration<T> {
    template: Expr,
    type_slots: Vec<T>,
}

impl StagedLoweredElaboration<PublicationValue<PublicationGoalInput>> {
    fn capture(store: &GoalStore, owner: TypeGoalOwner, mut template: Expr) -> Self {
        use crate::pass::visit_mut::TypecheckVisitMut;

        struct SlotCapture<'a> {
            store: &'a GoalStore,
            owner: TypeGoalOwner,
            active_binders: usize,
            slots: Vec<PublicationValue<PublicationGoalInput>>,
        }

        impl SlotCapture<'_> {
            fn assert_goal_scope(&self, ty: &mut Type) {
                use crate::pass::visit_mut::{TypecheckVisitMut, walk_type};

                struct GoalScope {
                    active_binders: usize,
                }

                impl TypecheckVisitMut<Lowered> for GoalScope {
                    fn enter_type_binder(
                        &mut self,
                        _param: &mut crate::ast::TypeParam,
                        _scope: Span,
                    ) {
                        self.active_binders += 1;
                    }

                    fn exit_type_binder(
                        &mut self,
                        _param: &mut crate::ast::TypeParam,
                        _scope: Span,
                    ) {
                        self.active_binders = self.active_binders.checked_sub(1).expect(
                            "structural publication type-binder traversal became unbalanced",
                        );
                    }

                    fn visit_type(&mut self, ty: &mut Type) {
                        if matches!(ty, Type::Goal { .. }) {
                            assert_eq!(
                                self.active_binders, 0,
                                "structural publication captured a goal beneath an unproved source binder"
                            );
                        }
                        walk_type(self, ty);
                    }
                }

                let mut scope = GoalScope {
                    active_binders: self.active_binders,
                };
                scope.visit_type(ty);
                assert_eq!(
                    scope.active_binders, self.active_binders,
                    "structural publication type-binder traversal became unbalanced"
                );
            }
        }

        impl TypecheckVisitMut<Lowered> for SlotCapture<'_> {
            fn enter_type_binder(&mut self, _param: &mut crate::ast::TypeParam, _scope: Span) {
                self.active_binders += 1;
            }

            fn exit_type_binder(&mut self, _param: &mut crate::ast::TypeParam, _scope: Span) {
                self.active_binders = self
                    .active_binders
                    .checked_sub(1)
                    .expect("structural publication binder traversal became unbalanced");
            }

            fn visit_type(&mut self, ty: &mut Type) {
                if crate::pass::typecheck_core::type_contains_goal(ty) {
                    self.assert_goal_scope(ty);
                    let scoped = self
                        .store
                        .scoped_type(
                            self.owner,
                            InternedType::fresh(ty.clone()),
                            crate::ast::Type::span(ty),
                        )
                        .expect("structural publication used an invalid inference owner");
                    self.slots
                        .push(PublicationValue::goal_free(PublicationGoalInput::pending(
                            scoped,
                        )));
                }
            }
        }

        let mut capture = SlotCapture {
            store,
            owner,
            active_binders: 0,
            slots: Vec::new(),
        };
        capture.visit_expr(&mut template);
        assert_eq!(
            capture.active_binders, 0,
            "structural publication binder traversal became unbalanced"
        );
        Self {
            template,
            type_slots: capture.slots,
        }
    }
}

impl<T> StagedLoweredElaboration<T> {
    fn map_type_slots<U>(self, map: &mut impl FnMut(T) -> U) -> StagedLoweredElaboration<U> {
        StagedLoweredElaboration {
            template: self.template,
            type_slots: self.type_slots.into_iter().map(map).collect(),
        }
    }
}

/// Immutable command syntax paired only at its construction and final
/// materialization boundaries with a lexical vector of type payloads.
#[derive(Debug)]
struct FrozenPublicationCommand {
    template: PublicationCommand<()>,
    type_slot_count: usize,
}

impl FrozenPublicationCommand {
    fn freeze<T>(command: PublicationCommand<T>) -> (Self, Vec<T>) {
        #[cfg(test)]
        record_publication_work(|work| work.command_freezes += 1);
        let mut type_slots = Vec::new();
        let template = command.map_type_slots(&mut |slot| {
            type_slots.push(slot);
        });
        let type_slot_count = type_slots.len();
        (
            Self {
                template,
                type_slot_count,
            },
            type_slots,
        )
    }

    fn materialize<T>(self, type_slots: Vec<T>) -> PublicationCommand<T> {
        #[cfg(test)]
        record_publication_work(|work| work.command_materializations += 1);
        assert_eq!(
            type_slots.len(),
            self.type_slot_count,
            "publication command received the wrong number of type slots"
        );
        let mut type_slots = type_slots.into_iter();
        let command = self.template.map_type_slots(&mut |()| {
            type_slots
                .next()
                .expect("publication command lost a frozen type slot")
        });
        assert!(
            type_slots.next().is_none(),
            "publication command retained an extra frozen type slot"
        );
        command
    }
}

#[derive(Debug)]
struct StagedPublicationCommand<T> {
    frozen: FrozenPublicationCommand,
    type_slots: Vec<T>,
}

impl<T> StagedPublicationCommand<T> {
    fn freeze(command: PublicationCommand<T>) -> Self {
        let (frozen, type_slots) = FrozenPublicationCommand::freeze(command);
        Self { frozen, type_slots }
    }
}

#[derive(Debug)]
struct ClosedLoweredElaboration(Expr);

impl StagedLoweredElaboration<ClosedPublicationValue> {
    fn materialize(mut self) -> ClosedLoweredElaboration {
        use crate::pass::visit_mut::TypecheckVisitMut;

        struct SlotMaterializer {
            slots: std::vec::IntoIter<ClosedPublicationValue>,
        }

        impl TypecheckVisitMut<Lowered> for SlotMaterializer {
            fn visit_type(&mut self, ty: &mut Type) {
                if crate::pass::typecheck_core::type_contains_goal(ty) {
                    *ty = self
                        .slots
                        .next()
                        .expect("structural publication lost a captured type slot")
                        .into_interned_type()
                        .clone_type();
                }
            }
        }

        let mut materializer = SlotMaterializer {
            slots: self.type_slots.into_iter(),
        };
        materializer.visit_expr(&mut self.template);
        assert!(
            materializer.slots.next().is_none(),
            "structural publication returned an extra captured type slot"
        );

        struct GoalFinder {
            found: bool,
        }

        impl TypecheckVisitMut<Lowered> for GoalFinder {
            fn visit_type(&mut self, ty: &mut Type) {
                self.found |= crate::pass::typecheck_core::type_contains_goal(ty);
            }
        }

        let mut finder = GoalFinder { found: false };
        finder.visit_expr(&mut self.template);
        assert!(
            !finder.found,
            "closed structural publication retained an inference goal"
        );
        ClosedLoweredElaboration(self.template)
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // publication commands stay inline until one atomic commit
enum PublicationEntry {
    Command(StagedPublicationCommand<PublicationValue<PublicationGoalInput>>),
    Child {
        owner: TypeGoalOwner,
        plan: Option<PublicationPlan<PublicationValue<PublicationGoalInput>>>,
    },
}

#[derive(Debug)]
struct PublicationShape {
    entries: Vec<PublicationShapeEntry>,
}

/// One source-ordered command shape after nested owner slots have closed.
/// Typed payloads travel through a parallel journal, so attaching or closing a
/// branch never rewrites this tree.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // frozen command payloads are traversed once without per-entry allocation
enum PublicationShapeEntry {
    Command(FrozenPublicationCommand),
    Branch(Box<PublicationShape>),
}

#[derive(Debug)]
struct PublicationPayloads<T> {
    chunks: Vec<Vec<T>>,
    len: usize,
}

impl<T> PublicationPayloads<T> {
    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    fn from_flat(payloads: Vec<T>) -> Self {
        let len = payloads.len();
        Self {
            chunks: vec![payloads],
            len,
        }
    }

    fn push_chunk(&mut self, payloads: Vec<T>) {
        self.len += payloads.len();
        self.chunks.push(payloads);
    }

    fn into_single_chunk(self) -> Vec<T> {
        assert_eq!(
            self.chunks.len(),
            1,
            "a retained child publication must carry exactly one payload chunk"
        );
        let payloads = self
            .chunks
            .into_iter()
            .next()
            .expect("single publication payload chunk disappeared");
        assert_eq!(
            payloads.len(),
            self.len,
            "retained child publication payload length drifted"
        );
        payloads
    }

    fn into_flat(self) -> Vec<T> {
        if self.chunks.len() == 1 {
            return self
                .chunks
                .into_iter()
                .next()
                .expect("single publication payload chunk disappeared");
        }
        let mut flattened = Vec::with_capacity(self.len);
        for chunk in self.chunks {
            flattened.extend(chunk);
        }
        assert_eq!(
            flattened.len(),
            self.len,
            "publication payload journal length drifted"
        );
        flattened
    }
}

#[derive(Debug)]
struct PublicationPlan<T> {
    shape: PublicationShape,
    payloads: PublicationPayloads<T>,
}

/// Opaque, single-use handle for the lexical position reserved for a nested
/// inference owner. Filling the handle attaches the child's already-closed
/// retained plan at this exact position rather than at child completion time.
#[derive(Debug)]
pub(crate) struct PublicationChildSlot {
    parent: TypeGoalOwner,
    child: TypeGoalOwner,
    index: usize,
}

/// Mutable, owner-scoped publication planning. It is concrete to Lowered and
/// intentionally neither cloneable nor readable as an [`Elaborations`] table.
#[derive(Debug)]
pub(crate) struct PublicationBuilder {
    module_path: String,
    owner: TypeGoalOwner,
    delta: GoalDeltaAuthority,
    entries: Vec<PublicationEntry>,
}

impl PublicationBuilder {
    pub(crate) fn new(store: &GoalStore, delta: &GoalDelta) -> Self {
        let owner = delta.owner();
        let module_path = store
            .owner_module_path(owner, Span::new(0, 0))
            .expect("publication builder used an invalid inference owner")
            .to_owned();
        Self {
            module_path,
            owner,
            delta: delta.authority(),
            entries: Vec::new(),
        }
    }

    fn push_command(
        &mut self,
        command: PublicationCommand<PublicationValue<PublicationGoalInput>>,
    ) {
        self.entries
            .push(PublicationEntry::Command(StagedPublicationCommand::freeze(
                command,
            )));
    }

    pub(crate) fn record_type_resolution(
        &mut self,
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
        resolved: ScopedType,
    ) {
        self.push_command(PublicationCommand::TypeResolution {
            call_site,
            slot_index,
            resolved: PublicationValue::goal_free(PublicationGoalInput::pending(resolved)),
        });
    }

    pub(crate) fn record_inlay_type_args(
        &mut self,
        call_site: crate::ast::ExpressionSite,
        callee_end: Span,
        slot_offset: usize,
        resolved: Vec<ScopedType>,
        binders: super::TypeBinderScope,
    ) {
        if resolved.is_empty() {
            return;
        }
        self.push_command(PublicationCommand::InlayTypeArgs {
            call_site,
            callee_end,
            slot_offset,
            binders,
            resolved: resolved
                .into_iter()
                .map(PublicationGoalInput::pending)
                .map(PublicationValue::goal_free)
                .collect(),
        });
    }

    pub(crate) fn mark_value_at_type_slot(
        &mut self,
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
    ) {
        self.push_command(PublicationCommand::MarkValueAtTypeSlot {
            call_site,
            slot_index,
        });
    }

    pub(crate) fn record_call_split(
        &mut self,
        call_site: crate::ast::ExpressionSite,
        split_after_slot: usize,
    ) {
        self.push_command(PublicationCommand::CallSplit {
            call_site,
            split_after_slot,
        });
    }

    pub(crate) fn record_call_implicit_unit(
        &mut self,
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
    ) {
        self.push_command(PublicationCommand::CallImplicitUnit {
            call_site,
            slot_index,
        });
    }

    pub(crate) fn record_call_residual_eta(
        &mut self,
        call_site: crate::ast::ExpressionSite,
        residual: ScopedType,
    ) {
        self.push_command(PublicationCommand::CallResidualEta {
            call_site,
            residual: PublicationValue::goal_free(PublicationGoalInput::pending(residual)),
        });
    }

    pub(crate) fn record_position_type(
        &mut self,
        expr_span: Span,
        resolved: ScopedType,
        binders: impl Into<super::TypeBinderScope>,
    ) {
        self.push_command(PublicationCommand::PositionType {
            expr_span,
            resolved: PublicationValue::goal_free(PublicationGoalInput::pending(resolved)),
            binders: binders.into(),
        });
    }

    pub(crate) fn record_rec_quote_source_type(
        &mut self,
        source: crate::ast::ExpressionOccurrenceId,
        resolved: ScopedType,
        function_scheme: bool,
    ) {
        let resolved = PublicationGoalInput::pending(resolved);
        self.push_command(PublicationCommand::RecQuoteSourceType {
            source,
            resolved: if function_scheme {
                PublicationValue::function_scheme(resolved)
            } else {
                PublicationValue::goal_free(resolved)
            },
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_rec_quote_expansion(
        &mut self,
        store: &GoalStore,
        node_id: NodeId,
        source: crate::ast::ExpressionOccurrenceId,
        continuation: Expr,
        public_type: ScopedType,
        public_is_scheme: bool,
        runtime_type: ScopedType,
        ambient_type_names: std::collections::HashSet<String>,
    ) {
        let public_type = PublicationGoalInput::pending(public_type);
        self.push_command(PublicationCommand::RecQuoteExpansion {
            node_id,
            source,
            continuation: StagedLoweredElaboration::capture(store, self.owner, continuation),
            public_type: if public_is_scheme {
                PublicationValue::function_scheme(public_type)
            } else {
                PublicationValue::goal_free(public_type)
            },
            runtime_type: PublicationValue::goal_free(PublicationGoalInput::pending(runtime_type)),
            ambient_type_names,
        });
    }

    pub(crate) fn record_scoped_function_scheme_position_type(
        &mut self,
        expr_span: Span,
        resolved: ScopedType,
        binders: impl Into<super::TypeBinderScope>,
    ) {
        // `resolved` may still contain a transparent alias at its function
        // frontier. The goal-store close canonicalizes that alias and checks
        // the function-scheme escape before publication becomes observable.
        self.push_command(PublicationCommand::PositionType {
            expr_span,
            resolved: PublicationValue::function_scheme(PublicationGoalInput::pending(resolved)),
            binders: binders.into(),
        });
    }

    pub(crate) fn record_position_binder(&mut self, segment_span: Span, binder: ResolvedBinder) {
        self.push_command(PublicationCommand::PositionBinder {
            segment_span,
            binder,
        });
    }

    pub(crate) fn record_local_decl(
        &mut self,
        name: String,
        decl_span: Span,
        kind: crate::pass::typecheck_core::ResolvedBinderKind,
    ) {
        self.push_command(PublicationCommand::LocalDecl {
            name,
            decl_span,
            kind,
        });
    }

    pub(crate) fn record_prepared_goal_free_position_type(
        &mut self,
        expr_span: Span,
        resolved: PreparedCloseType,
        binders: impl Into<super::TypeBinderScope>,
    ) {
        let (delta, owner, resolved) = resolved.into_goal_free_position_publication();
        assert_eq!(
            delta, self.delta,
            "position-type publication was prepared from a different speculative delta"
        );
        assert_eq!(
            owner, self.owner,
            "position-type publication was prepared for a different inference owner"
        );
        self.push_command(PublicationCommand::PositionType {
            expr_span,
            resolved: PublicationValue::goal_free(resolved),
            binders: binders.into(),
        });
    }

    pub(crate) fn record_fn_param_type(
        &mut self,
        fn_site: crate::ast::ExpressionSite,
        value_param_index: usize,
        resolved: ScopedType,
    ) {
        self.push_command(PublicationCommand::FnParamType {
            fn_site,
            value_param_index,
            resolved: PublicationValue::goal_free(PublicationGoalInput::pending(resolved)),
        });
    }

    pub(crate) fn record_inlay_let_type(
        &mut self,
        name_span: Span,
        resolved: ScopedType,
        binders: super::TypeBinderScope,
    ) {
        self.push_command(PublicationCommand::InlayLetType {
            name_span,
            resolved: PublicationValue::goal_free(PublicationGoalInput::pending(resolved)),
            binders,
        });
    }

    pub(crate) fn record_literal_resolution(
        &mut self,
        literal_site: crate::ast::ExpressionSite,
        resolved: ScopedType,
    ) {
        self.push_command(PublicationCommand::LiteralResolution {
            literal_site,
            resolved: PublicationValue::goal_free(PublicationGoalInput::pending(resolved)),
        });
    }

    pub(crate) fn record_rec_order_binding(
        &mut self,
        store: &GoalStore,
        node_id: NodeId,
        replacement: Expr,
        resolved: ScopedType,
    ) {
        assert!(
            matches!(&replacement, Expr::Let { ty: None, .. }),
            "runtime recursive-order publication must leave its root let type to the paired artifact"
        );
        let replacement = StagedLoweredElaboration::capture(store, self.owner, replacement);
        self.push_command(PublicationCommand::RecOrderBinding {
            node_id,
            replacement,
            resolved: PublicationValue::goal_free(PublicationGoalInput::pending(resolved)),
        });
    }

    pub(crate) fn record_lowered_elaboration(
        &mut self,
        store: &GoalStore,
        node_id: NodeId,
        replacement: Expr,
    ) {
        let replacement = StagedLoweredElaboration::capture(store, self.owner, replacement);
        self.push_command(PublicationCommand::LoweredElaboration {
            node_id,
            replacement,
        });
    }

    pub(crate) fn record_prime_requalification_bindings(
        &mut self,
        imports: Vec<UserElaboratorTemplateGeneratedImport>,
        aliases: Vec<PrimeLocalTypeAlias>,
    ) {
        if imports.is_empty() && aliases.is_empty() {
            return;
        }
        self.push_command(PublicationCommand::PrimeRequalificationBindings { imports, aliases });
    }

    pub(crate) fn record_user_elaborator(&mut self, obligation: DeferredUserElaboratorElaboration) {
        self.push_command(PublicationCommand::UserElaborator { obligation });
    }

    pub(crate) fn reserve_child(
        &mut self,
        store: &GoalStore,
        child: TypeGoalOwner,
    ) -> PublicationChildSlot {
        let actual_parent = store
            .owner_parent(child, Span::new(0, 0))
            .expect("publication child reservation used an invalid inference owner");
        assert_eq!(
            actual_parent,
            Some(self.owner),
            "publication child reservation must name the direct inference parent"
        );
        let index = self.entries.len();
        self.entries.push(PublicationEntry::Child {
            owner: child,
            plan: None,
        });
        PublicationChildSlot {
            parent: self.owner,
            child,
            index,
        }
    }

    pub(crate) fn fill_child(
        &mut self,
        slot: PublicationChildSlot,
        child: RetainedPublicationDelta,
    ) {
        assert_eq!(
            slot.parent, self.owner,
            "publication child slot belongs to a different parent owner"
        );
        assert_eq!(
            child.owner, slot.child,
            "publication child delta does not match its reserved owner"
        );
        assert_eq!(
            child.destination, self.owner,
            "publication child delta retained into a different parent owner"
        );
        assert_eq!(
            child.module_path, self.module_path,
            "publication child crossed a module boundary"
        );
        let entry = self
            .entries
            .get_mut(slot.index)
            .expect("publication child slot index disappeared before fill");
        let PublicationEntry::Child { owner, plan } = entry else {
            panic!("publication child slot no longer points at a child entry")
        };
        assert_eq!(
            *owner, slot.child,
            "publication child slot was replaced by another owner"
        );
        assert!(
            plan.is_none(),
            "publication child slot was filled more than once"
        );
        *plan = Some(child.plan);
    }

    pub(crate) fn finish(self) -> PublicationDelta {
        let mut shape = PublicationShape {
            entries: Vec::with_capacity(self.entries.len()),
        };
        let mut payloads = PublicationPayloads::new();
        for entry in self.entries {
            #[cfg(test)]
            record_publication_work(|work| work.shape_freeze_visits += 1);
            match entry {
                PublicationEntry::Command(command) => {
                    shape
                        .entries
                        .push(PublicationShapeEntry::Command(command.frozen));
                    payloads.push_chunk(command.type_slots);
                }
                PublicationEntry::Child {
                    owner: _,
                    plan: Some(child),
                } => {
                    shape
                        .entries
                        .push(PublicationShapeEntry::Branch(Box::new(child.shape)));
                    payloads.push_chunk(child.payloads.into_single_chunk());
                }
                PublicationEntry::Child { owner, plan: None } => {
                    panic!("publication owner {owner:?} still had an unfilled child slot at freeze")
                }
            }
        }
        PublicationDelta {
            module_path: self.module_path,
            owner: self.owner,
            delta: self.delta,
            plan: PublicationPlan { shape, payloads },
        }
    }
}

/// Frozen, inert publication plan. Only owner close can turn its opaque type
/// payloads into a committable delta.
#[derive(Debug)]
pub(crate) struct PublicationDelta {
    module_path: String,
    owner: TypeGoalOwner,
    delta: GoalDeltaAuthority,
    plan: PublicationPlan<PublicationValue<PublicationGoalInput>>,
}

impl PublicationPlan<PublicationValue<PublicationGoalInput>> {
    fn into_close_inputs(
        self,
        escape: impl Fn(PublicationRootKind) -> PublicationGoalEscape,
    ) -> (
        PublicationShape,
        Vec<PublicationRootKind>,
        Vec<(PublicationGoalInput, PublicationGoalEscape)>,
    ) {
        let payloads = self.payloads.into_flat();
        #[cfg(test)]
        record_publication_work(|work| work.payload_close_inputs += payloads.len());
        let mut root_kinds = Vec::with_capacity(payloads.len());
        let mut outputs = Vec::with_capacity(payloads.len());
        for publication in payloads {
            root_kinds.push(publication.root_kind);
            outputs.push((publication.value, escape(publication.root_kind)));
        }
        (self.shape, root_kinds, outputs)
    }
}

#[derive(Debug)]
pub(crate) struct RetainedPublicationDelta {
    module_path: String,
    owner: TypeGoalOwner,
    destination: TypeGoalOwner,
    plan: PublicationPlan<PublicationValue<PublicationGoalInput>>,
}

#[derive(Debug)]
enum ClosedPublicationValue {
    GoalFree(GoalFreePublicationOutput),
    FunctionScheme(GoalFreePublicationOutput),
}

impl ClosedPublicationValue {
    fn into_interned_type(self) -> InternedType {
        match self {
            Self::GoalFree(output) | Self::FunctionScheme(output) => output.into_interned_type(),
        }
    }
}

/// Root-closed publication state. Construction has already validated every
/// type payload, so committing this delta to [`Elaborations`] is infallible.
#[derive(Debug)]
pub(crate) struct ClosedPublicationDelta {
    module_path: String,
    plan: PublicationPlan<ClosedPublicationValue>,
}

enum PublicationSemanticOutputs {
    Scoped(Vec<(ScopedType, GoalEscape)>),
    Prepared(Vec<(PreparedCloseType, GoalEscape)>),
}

impl PublicationSemanticOutputs {
    fn prepare(
        self,
        store: &GoalStore,
        delta: GoalDelta,
        publication_outputs: Vec<(PublicationGoalInput, PublicationGoalEscape)>,
        ctx: &impl GoalTypeContext,
    ) -> Result<PreparedGoalClose, Error> {
        match self {
            Self::Scoped(outputs) => {
                store.prepare_owner_with_publication(delta, outputs, publication_outputs, ctx)
            }
            Self::Prepared(outputs) => store.prepare_prepared_owner_with_publication(
                delta,
                outputs,
                publication_outputs,
                ctx,
            ),
        }
    }
}

/// Fully validated root publication whose store, elaboration, and local-state
/// mutations have not yet become visible.
#[derive(Debug)]
pub(crate) struct PreparedRootPublicationClose {
    goal_commit: PreparedGoalCommit,
    publication: ClosedPublicationDelta,
    semantic_outputs: Vec<ClosedGoalOutput>,
}

impl PreparedRootPublicationClose {
    pub(crate) fn take_semantic_outputs(&mut self) -> Vec<ClosedGoalOutput> {
        std::mem::take(&mut self.semantic_outputs)
    }

    fn into_parts(
        self,
    ) -> (
        PreparedGoalCommit,
        ClosedPublicationDelta,
        Vec<ClosedGoalOutput>,
    ) {
        (self.goal_commit, self.publication, self.semantic_outputs)
    }

    #[cfg(test)]
    pub(crate) fn commit_without_rec_order(
        self,
        store: &mut GoalStore,
        elaborations: &mut Elaborations,
    ) -> Vec<ClosedGoalOutput> {
        let (goal_commit, publication, outputs) = self.into_parts();
        store.commit_prepared_owner(goal_commit);
        publication.commit(elaborations);
        outputs
    }

    pub(crate) fn commit_with_rec_order<'m>(
        self,
        store: &mut GoalStore,
        tcx: &mut TypeCtx<'m, '_, Lowered>,
        rec_order: PreparedRecOrderCommit<'m, Lowered>,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        let (goal_commit, publication, outputs) = self.into_parts();
        // Cross-component validation must precede the first visible write:
        // neither an exact goal-store snapshot nor an exact local-stack
        // snapshot may be discovered stale after the other has committed.
        store.validate_prepared_owner(&goal_commit);
        tcx.validate_prepared_rec_order(&rec_order);
        store.commit_prepared_owner(goal_commit);
        tcx.commit_prepared_rec_order(rec_order);
        publication.commit(tcx.elaborations);
        Ok(outputs)
    }
}

impl PublicationDelta {
    fn preflight_owner(
        &self,
        store: &GoalStore,
        goal_delta: &GoalDelta,
    ) -> Result<Option<TypeGoalOwner>, Error> {
        assert_eq!(
            goal_delta.owner(),
            self.owner,
            "publication plan and inference delta belong to different owners"
        );
        assert_eq!(
            goal_delta.authority(),
            self.delta,
            "publication plan and inference delta belong to different speculative deltas"
        );
        assert_eq!(
            store.owner_module_path(self.owner, Span::new(0, 0))?,
            self.module_path,
            "publication plan crossed its inference owner's module boundary"
        );
        store.owner_parent(self.owner, Span::new(0, 0))
    }

    pub(crate) fn close_child(
        self,
        store: &mut GoalStore,
        goal_delta: GoalDelta,
        destination: TypeGoalOwner,
        semantic_outputs: Vec<(ScopedType, GoalEscape)>,
        ctx: &impl GoalTypeContext,
    ) -> Result<(RetainedPublicationDelta, Vec<ClosedGoalOutput>), Error> {
        self.close_child_outputs(
            store,
            goal_delta,
            destination,
            PublicationSemanticOutputs::Scoped(semantic_outputs),
            ctx,
        )
    }

    pub(crate) fn close_prepared_child(
        self,
        store: &mut GoalStore,
        goal_delta: GoalDelta,
        destination: TypeGoalOwner,
        semantic_outputs: Vec<(PreparedCloseType, GoalEscape)>,
        ctx: &impl GoalTypeContext,
    ) -> Result<(RetainedPublicationDelta, Vec<ClosedGoalOutput>), Error> {
        self.close_child_outputs(
            store,
            goal_delta,
            destination,
            PublicationSemanticOutputs::Prepared(semantic_outputs),
            ctx,
        )
    }

    fn close_child_outputs(
        self,
        store: &mut GoalStore,
        goal_delta: GoalDelta,
        destination: TypeGoalOwner,
        semantic_outputs: PublicationSemanticOutputs,
        ctx: &impl GoalTypeContext,
    ) -> Result<(RetainedPublicationDelta, Vec<ClosedGoalOutput>), Error> {
        let actual_parent = self.preflight_owner(store, &goal_delta)?;
        assert_eq!(
            actual_parent,
            Some(destination),
            "publication child close must retain into the direct inference parent"
        );
        let (shape, root_kinds, publication_outputs) =
            self.plan.into_close_inputs(|kind| match kind {
                PublicationRootKind::GoalFree => PublicationGoalEscape::ToOwner(destination),
                PublicationRootKind::FunctionScheme => {
                    PublicationGoalEscape::ToOwnerFunctionScheme(destination)
                }
            });
        let prepared = semantic_outputs.prepare(store, goal_delta, publication_outputs, ctx)?;
        let (goal_commit, closed, closed_publication) = prepared.into_parts();
        assert_eq!(
            closed.owner(),
            self.owner,
            "publication child close returned a different inference owner"
        );
        assert_eq!(
            closed_publication.owner(),
            self.owner,
            "publication child close returned publication output for a different inference owner"
        );
        let outputs = closed.into_outputs();
        let publication_outputs = closed_publication.into_outputs();
        assert_eq!(
            publication_outputs.len(),
            root_kinds.len(),
            "publication child close returned the wrong number of typed outputs"
        );
        #[cfg(test)]
        record_publication_work(|work| work.payload_close_outputs += publication_outputs.len());
        let payloads = root_kinds
            .into_iter()
            .zip(publication_outputs)
            .map(|(root_kind, output)| {
                let ClosedPublicationOutput::Retained(output) = output else {
                    unreachable!("a child publication output must remain retained")
                };
                assert_eq!(
                    output.destination(),
                    destination,
                    "publication child output retained into the wrong owner"
                );
                PublicationValue {
                    value: PublicationGoalInput::retained(output),
                    root_kind,
                }
            })
            .collect::<Vec<_>>();
        let retained = RetainedPublicationDelta {
            module_path: self.module_path,
            owner: self.owner,
            destination,
            plan: PublicationPlan {
                shape,
                payloads: PublicationPayloads::from_flat(payloads),
            },
        };
        // All result construction and assertions precede the sole visible
        // mutation. No fallible step remains after the owner lifecycle closes.
        store.commit_prepared_owner(goal_commit);
        Ok((retained, outputs))
    }

    pub(crate) fn prepare_root(
        self,
        store: &GoalStore,
        goal_delta: GoalDelta,
        semantic_outputs: Vec<(ScopedType, GoalEscape)>,
        ctx: &impl GoalTypeContext,
    ) -> Result<PreparedRootPublicationClose, Error> {
        self.prepare_root_outputs(
            store,
            goal_delta,
            PublicationSemanticOutputs::Scoped(semantic_outputs),
            ctx,
        )
    }

    #[cfg(test)]
    pub(crate) fn close_root(
        self,
        store: &mut GoalStore,
        goal_delta: GoalDelta,
        semantic_outputs: Vec<(ScopedType, GoalEscape)>,
        ctx: &impl GoalTypeContext,
        elaborations: &mut Elaborations,
    ) -> Result<Vec<ClosedGoalOutput>, Error> {
        let prepared = self.prepare_root(store, goal_delta, semantic_outputs, ctx)?;
        Ok(prepared.commit_without_rec_order(store, elaborations))
    }

    fn prepare_root_outputs(
        self,
        store: &GoalStore,
        goal_delta: GoalDelta,
        semantic_outputs: PublicationSemanticOutputs,
        ctx: &impl GoalTypeContext,
    ) -> Result<PreparedRootPublicationClose, Error> {
        let owner = self.owner;
        let actual_parent = self.preflight_owner(store, &goal_delta)?;
        assert_eq!(
            actual_parent, None,
            "publication root close requires a root inference owner"
        );
        let (shape, root_kinds, publication_outputs) =
            self.plan.into_close_inputs(|root_kind| match root_kind {
                PublicationRootKind::GoalFree => PublicationGoalEscape::ClosedAt(owner),
                PublicationRootKind::FunctionScheme => {
                    PublicationGoalEscape::ClosedFunctionSchemeAt(owner)
                }
            });
        let prepared = semantic_outputs.prepare(store, goal_delta, publication_outputs, ctx)?;
        let (goal_commit, closed, closed_publication) = prepared.into_parts();
        assert_eq!(
            closed.owner(),
            owner,
            "publication root close returned a different inference owner"
        );
        assert_eq!(
            closed_publication.owner(),
            owner,
            "publication root close returned publication output for a different inference owner"
        );
        let outputs = closed.into_outputs();
        let publication_outputs = closed_publication.into_outputs();
        assert_eq!(
            publication_outputs.len(),
            root_kinds.len(),
            "publication root close returned the wrong number of typed outputs"
        );
        #[cfg(test)]
        record_publication_work(|work| work.payload_close_outputs += publication_outputs.len());
        let payloads = root_kinds
            .into_iter()
            .zip(publication_outputs)
            .map(|(root_kind, output)| match (root_kind, output) {
                (PublicationRootKind::GoalFree, ClosedPublicationOutput::GoalFree(output)) => {
                    assert_eq!(
                        output.destination(),
                        owner,
                        "goal-free publication output targeted a different root owner"
                    );
                    ClosedPublicationValue::GoalFree(output)
                }
                (
                    PublicationRootKind::FunctionScheme,
                    ClosedPublicationOutput::GoalFree(output),
                ) => {
                    assert_eq!(
                        output.destination(),
                        owner,
                        "function-scheme publication output targeted a different root owner"
                    );
                    ClosedPublicationValue::FunctionScheme(output)
                }
                (PublicationRootKind::GoalFree, ClosedPublicationOutput::Retained(_))
                | (PublicationRootKind::FunctionScheme, ClosedPublicationOutput::Retained(_)) => {
                    unreachable!("publication root output did not match its requested close mode")
                }
            })
            .collect::<Vec<_>>();
        Ok(PreparedRootPublicationClose {
            goal_commit,
            publication: ClosedPublicationDelta {
                module_path: self.module_path,
                plan: PublicationPlan {
                    shape,
                    payloads: PublicationPayloads::from_flat(payloads),
                },
            },
            semantic_outputs: outputs,
        })
    }
}

impl ClosedPublicationDelta {
    pub(crate) fn commit(self, elaborations: &mut Elaborations) {
        let Self { module_path, plan } = self;
        let mut payloads = plan.payloads.into_flat().into_iter();
        Self::commit_shape(&module_path, plan.shape, &mut payloads, elaborations);
        assert!(
            payloads.next().is_none(),
            "publication commit retained an extra closed payload"
        );
    }

    fn commit_shape(
        module_path: &str,
        shape: PublicationShape,
        payloads: &mut std::vec::IntoIter<ClosedPublicationValue>,
        elaborations: &mut Elaborations,
    ) {
        for entry in shape.entries {
            #[cfg(test)]
            record_publication_work(|work| work.final_shape_visits += 1);
            let command = match entry {
                PublicationShapeEntry::Command(command) => {
                    let type_slots = (0..command.type_slot_count)
                        .map(|_| {
                            payloads
                                .next()
                                .expect("publication commit lost a closed payload")
                        })
                        .collect();
                    command.materialize(type_slots)
                }
                PublicationShapeEntry::Branch(shape) => {
                    Self::commit_shape(module_path, *shape, payloads, elaborations);
                    continue;
                }
            };
            match command {
                PublicationCommand::RecQuoteSourceType { source, resolved } => {
                    elaborations.record_rec_quote_source_type(source, resolved.into_interned_type())
                }
                PublicationCommand::RecQuoteExpansion {
                    node_id,
                    source,
                    continuation,
                    public_type,
                    runtime_type,
                    ambient_type_names,
                } => elaborations.record_rec_quote_expansion(
                    source,
                    super::rec_quote::ClosedRecQuoteExpansion {
                        node_id,
                        continuation: continuation.materialize().0,
                        public_type: public_type.into_interned_type(),
                        runtime_type: runtime_type.into_interned_type(),
                        ambient_type_names,
                    },
                ),
                PublicationCommand::TypeResolution {
                    call_site,
                    slot_index,
                    resolved,
                } => elaborations.record_type_resolution(
                    module_path,
                    call_site.id,
                    slot_index,
                    resolved.into_interned_type(),
                ),
                PublicationCommand::InlayTypeArgs {
                    call_site,
                    callee_end,
                    slot_offset,
                    resolved,
                    binders,
                } => elaborations.record_inlay_type_args(
                    module_path,
                    call_site,
                    callee_end,
                    slot_offset,
                    resolved
                        .into_iter()
                        .map(|resolved| resolved.into_interned_type().clone_type())
                        .collect(),
                    binders,
                ),
                PublicationCommand::MarkValueAtTypeSlot {
                    call_site,
                    slot_index,
                } => {
                    elaborations.mark_value_at_type_slot(module_path, call_site.id, slot_index);
                    elaborations.position_index.record_value_at_type_slot(
                        module_path,
                        call_site.span,
                        slot_index,
                    );
                }
                PublicationCommand::CallSplit {
                    call_site,
                    split_after_slot,
                } => elaborations.record_call_split(module_path, call_site.id, split_after_slot),
                PublicationCommand::CallImplicitUnit {
                    call_site,
                    slot_index,
                } => elaborations.record_call_implicit_unit_slot(
                    module_path,
                    call_site.id,
                    slot_index,
                ),
                PublicationCommand::CallResidualEta {
                    call_site,
                    residual,
                } => elaborations.record_call_residual_eta_type(
                    module_path,
                    call_site.id,
                    residual.into_interned_type(),
                ),
                PublicationCommand::PositionType {
                    expr_span,
                    resolved,
                    binders,
                } => elaborations.record_position_type(
                    module_path,
                    expr_span,
                    resolved.into_interned_type(),
                    binders,
                ),
                PublicationCommand::PositionBinder {
                    segment_span,
                    binder,
                } => {
                    elaborations.record_position_binder(module_path, segment_span, binder);
                }
                PublicationCommand::LocalDecl {
                    name,
                    decl_span,
                    kind,
                } => {
                    let binder = match kind {
                        crate::pass::typecheck_core::ResolvedBinderKind::Local => {
                            ResolvedBinder::Local {
                                name: name.clone(),
                                decl_span: Some(decl_span),
                            }
                        }
                        crate::pass::typecheck_core::ResolvedBinderKind::TypeParam => {
                            ResolvedBinder::TypeParam {
                                name: name.clone(),
                                decl_span: Some(decl_span),
                            }
                        }
                        _ => unreachable!(
                            "only local and type-parameter binders have lexical declarations"
                        ),
                    };
                    elaborations.record_local_decl(module_path, &name, decl_span, binder);
                }
                PublicationCommand::FnParamType {
                    fn_site,
                    value_param_index,
                    resolved,
                } => elaborations.record_fn_param_type(
                    module_path,
                    fn_site.id,
                    value_param_index,
                    resolved.into_interned_type(),
                ),
                PublicationCommand::InlayLetType {
                    name_span,
                    resolved,
                    binders,
                } => elaborations.record_inlay_let_type(
                    module_path,
                    name_span,
                    resolved.into_interned_type().clone_type(),
                    binders,
                ),
                PublicationCommand::LiteralResolution {
                    literal_site,
                    resolved,
                } => elaborations.record_literal_resolution(
                    module_path,
                    literal_site.id,
                    resolved.into_interned_type(),
                ),
                PublicationCommand::PrimeRequalificationBindings { imports, aliases } => {
                    elaborations.record_prime_requalification_imports(module_path, &imports);
                    elaborations.record_generated_type_aliases(module_path, &aliases);
                }
                PublicationCommand::RecOrderBinding {
                    node_id,
                    replacement,
                    resolved,
                } => {
                    let replacement = replacement.materialize().0;
                    assert!(
                        matches!(&replacement, Expr::Let { ty: None, .. }),
                        "closed recursive-order publication filled its root let annotation"
                    );
                    let binding_type = resolved.into_interned_type();
                    elaborations.record_rec_order_runtime(
                        module_path,
                        node_id,
                        replacement,
                        binding_type,
                    );
                }
                PublicationCommand::LoweredElaboration {
                    node_id,
                    replacement,
                } => {
                    let replacement = replacement.materialize().0;
                    elaborations.entries.insert(
                        (module_path.to_owned(), node_id),
                        RecordedElaboration::Lowered(replacement),
                    );
                }
                PublicationCommand::UserElaborator { obligation } => {
                    elaborations.enqueue_deferred(DeferredElaboration::UserElaborator(Box::new(
                        obligation,
                    )));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Kind, Meta, PathSegment};
    use crate::pass::typecheck_core::{GoalContextCapability, GoalOwnerKind, RigidScope};

    struct TestContext;

    impl GoalTypeContext for TestContext {
        fn capability(&self) -> GoalContextCapability {
            GoalContextCapability::Test(1)
        }

        fn canonicalize(
            &self,
            ty: &Type,
            identity_canonical: bool,
            _scope: &RigidScope,
        ) -> (Type, bool) {
            (ty.clone(), identity_canonical)
        }

        fn canonicalize_alias_frontier(
            &self,
            _ty: &Type,
            _identity_canonical: bool,
            _scope: &RigidScope,
        ) -> Option<(Type, bool)> {
            None
        }

        fn nominal_head_kind(
            &self,
            _segments: &[PathSegment],
            _supplied_args: usize,
            span: Span,
            _scope: &RigidScope,
            _identity_canonical: bool,
        ) -> Result<Kind, Error> {
            Err(Error::type_(
                span,
                "the publication work test admits only structural unit types",
            ))
        }

        fn mismatch(&self, _found: &ScopedType, _expected: &ScopedType, span: Span) -> Error {
            Error::type_(span, "type mismatch")
        }
    }

    fn span(index: usize) -> Span {
        let index = u32::try_from(index).expect("test span index fits in u32");
        Span::new(index, index + 1)
    }

    fn unit(index: usize) -> Type {
        Type::Unit {
            meta: Meta::new(span(index)),
        }
    }

    fn bottom(index: usize) -> Type {
        Type::Bottom {
            meta: Meta::new(span(index)),
        }
    }

    fn fresh_site(span: Span) -> crate::ast::ExpressionSite {
        crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span,
        }
    }

    fn product(index: usize) -> Type {
        Type::Product {
            left: Box::new(unit(index)),
            right: Box::new(unit(index)),
            meta: Meta::new(span(index)),
        }
    }

    fn sum(index: usize) -> Type {
        Type::Sum {
            left: Box::new(unit(index)),
            right: Box::new(unit(index)),
            meta: Meta::new(span(index)),
        }
    }

    fn root_owner(store: &mut GoalStore, index: usize) -> TypeGoalOwner {
        store
            .begin_owner(
                None,
                RigidScope::new(),
                GoalOwnerKind::Application,
                span(index),
            )
            .expect("open test root")
    }

    fn record_unit(
        store: &GoalStore,
        owner: TypeGoalOwner,
        builder: &mut PublicationBuilder,
        index: usize,
    ) {
        let resolved = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(unit(index)),
                span(index),
            )
            .expect("scope test unit");
        builder.record_type_resolution(fresh_site(span(index)), 0, resolved);
    }

    fn scoped_type(store: &GoalStore, owner: TypeGoalOwner, ty: Type) -> ScopedType {
        let ty_span = ty.span();
        store
            .scoped_type(owner, InternedType::fresh_canonical(ty), ty_span)
            .expect("scope test type")
    }

    fn generated_import(module_path: &str, alias: &str) -> UserElaboratorTemplateGeneratedImport {
        UserElaboratorTemplateGeneratedImport {
            module_path: module_path.to_owned(),
            alias: alias.to_owned(),
        }
    }

    fn generated_alias(alias: &str, nominal: &str, index: usize) -> PrimeLocalTypeAlias {
        PrimeLocalTypeAlias {
            alias: alias.to_owned(),
            nominal: nominal.to_owned(),
            params: Vec::new(),
            span: span(index),
        }
    }

    fn record_prime_requalification(
        builder: &mut PublicationBuilder,
        imports: Vec<UserElaboratorTemplateGeneratedImport>,
        aliases: Vec<PrimeLocalTypeAlias>,
    ) {
        builder.push_command(PublicationCommand::PrimeRequalificationBindings { imports, aliases });
    }

    #[test]
    fn frozen_commands_round_trip_zero_and_multiple_type_slots() {
        reset_publication_work();

        let multi = StagedPublicationCommand::freeze(PublicationCommand::InlayTypeArgs {
            call_site: fresh_site(span(0)),
            callee_end: span(0),
            slot_offset: 0,
            binders: Default::default(),
            resolved: vec![10, 20, 30],
        });
        assert_eq!(multi.frozen.type_slot_count, 3);
        let PublicationCommand::InlayTypeArgs { resolved, .. } =
            multi.frozen.materialize(multi.type_slots)
        else {
            panic!("multi-slot command changed shape")
        };
        assert_eq!(resolved, [10, 20, 30]);

        let zero =
            StagedPublicationCommand::<usize>::freeze(PublicationCommand::MarkValueAtTypeSlot {
                call_site: fresh_site(span(1)),
                slot_index: 4,
            });
        assert_eq!(zero.frozen.type_slot_count, 0);
        assert!(matches!(
            zero.frozen.materialize(zero.type_slots),
            PublicationCommand::MarkValueAtTypeSlot { slot_index: 4, .. }
        ));

        assert_eq!(
            publication_work(),
            PublicationWorkCounters {
                command_freezes: 2,
                command_materializations: 2,
                ..PublicationWorkCounters::default()
            }
        );
    }

    #[test]
    fn prime_requalification_command_round_trips_without_type_slots() {
        let imports = vec![generated_import("types/first", "_q_first")];
        let aliases = vec![generated_alias("KioFirst", "First", 0)];
        let command: PublicationCommand<usize> = PublicationCommand::PrimeRequalificationBindings {
            imports: imports.clone(),
            aliases: aliases.clone(),
        };

        let staged = StagedPublicationCommand::freeze(command);
        assert_eq!(staged.frozen.type_slot_count, 0);
        let PublicationCommand::PrimeRequalificationBindings {
            imports: mapped_imports,
            aliases: mapped_aliases,
        } = staged.frozen.materialize(staged.type_slots)
        else {
            panic!("Prime requalification command changed shape")
        };
        assert_eq!(mapped_imports, imports);
        assert_eq!(mapped_aliases, aliases);
    }

    #[test]
    fn prepared_prime_requalification_is_inert_until_root_commit() {
        let context = TestContext;
        let mut store = GoalStore::new();
        let owner = root_owner(&mut store, 0);
        let delta = store.begin_delta(owner, span(0)).expect("open root delta");
        let mut builder = PublicationBuilder::new(&store, &delta);
        record_prime_requalification(
            &mut builder,
            vec![generated_import("types/rolled_back", "_q_rolled_back")],
            vec![generated_alias("KioRolledBack", "RolledBack", 20)],
        );

        let prepared = builder
            .finish()
            .prepare_root(&store, delta, Vec::new(), &context)
            .expect("prepare root publication");
        let elaborations = Elaborations::new();
        assert!(
            elaborations
                .prime_requalification_imports_for("<test>")
                .is_empty()
        );
        assert!(elaborations.generated_type_aliases_for("<test>").is_empty());

        drop(prepared);
        assert!(
            elaborations
                .prime_requalification_imports_for("<test>")
                .is_empty()
        );
        assert!(elaborations.generated_type_aliases_for("<test>").is_empty());
    }

    #[test]
    #[should_panic(expected = "a retained child publication must carry exactly one payload chunk")]
    fn retained_child_publication_rejects_multiple_payload_chunks() {
        let mut payloads = PublicationPayloads::new();
        payloads.push_chunk(vec![1]);
        payloads.push_chunk(vec![2]);
        let _ = payloads.into_single_chunk();
    }

    #[test]
    fn zero_and_multi_slot_commands_keep_lexical_payload_order_across_a_child() {
        let context = TestContext;
        let mut store = GoalStore::new();
        let parent = root_owner(&mut store, 0);
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                span(1),
            )
            .unwrap();
        let parent_delta = store.begin_delta(parent, span(0)).unwrap();
        let child_delta = store.begin_delta(child, span(1)).unwrap();
        let mut parent_builder = PublicationBuilder::new(&store, &parent_delta);
        let inlay_span = span(100);
        let call_site = fresh_site(span(101));

        parent_builder.record_inlay_type_args(
            call_site,
            inlay_span,
            0,
            vec![scoped_type(&store, parent, unit(10))],
            Default::default(),
        );
        parent_builder.mark_value_at_type_slot(call_site, 1);
        let child_slot = parent_builder.reserve_child(&store, child);

        let mut child_builder = PublicationBuilder::new(&store, &child_delta);
        child_builder.record_inlay_type_args(
            call_site,
            inlay_span,
            1,
            vec![
                scoped_type(&store, child, bottom(11)),
                scoped_type(&store, child, product(12)),
            ],
            Default::default(),
        );
        child_builder.record_call_split(call_site, 2);
        let (child_publication, outputs) = child_builder
            .finish()
            .close_child(&mut store, child_delta, parent, Vec::new(), &context)
            .unwrap();
        assert!(outputs.is_empty());
        parent_builder.fill_child(child_slot, child_publication);

        parent_builder.record_inlay_type_args(
            call_site,
            inlay_span,
            3,
            vec![scoped_type(&store, parent, sum(13))],
            Default::default(),
        );
        parent_builder.mark_value_at_type_slot(call_site, 3);
        let mut elaborations = Elaborations::new();
        let outputs = parent_builder
            .finish()
            .close_root(
                &mut store,
                parent_delta,
                Vec::new(),
                &context,
                &mut elaborations,
            )
            .unwrap();
        assert!(outputs.is_empty());

        let types = elaborations
            .position_index()
            .inlay_type_args_iter()
            .filter(|((module, candidate), _)| module == "<test>" && *candidate == inlay_span)
            .flat_map(|(_, types)| types.iter())
            .collect::<Vec<_>>();
        assert_eq!(
            types
                .iter()
                .map(|ty| crate::pass::typecheck_core::display_type(*ty))
                .collect::<Vec<_>>(),
            [".", "!", "(. & .)", "(. | .)"]
        );
        assert!(elaborations.is_value_at_type_slot("<test>", call_site.id, 1));
        assert!(elaborations.is_value_at_type_slot("<test>", call_site.id, 3));
        assert_eq!(
            elaborations.call_splits_for("<test>", call_site.id),
            Some([2].as_slice())
        );
    }

    #[test]
    fn deep_publication_closes_payloads_without_rewalking_command_shape() {
        const DEPTH: usize = 64;

        reset_publication_work();
        reset_publication_validation_work();
        let context = TestContext;
        let mut store = GoalStore::new();
        let mut owners = Vec::with_capacity(DEPTH);
        for index in 0..DEPTH {
            let parent = owners.last().copied();
            let kind = if parent.is_none() {
                GoalOwnerKind::Application
            } else {
                GoalOwnerKind::NestedApplication
            };
            owners.push(
                store
                    .begin_owner(parent, RigidScope::new(), kind, span(index))
                    .expect("open publication owner"),
            );
        }
        let mut deltas = owners
            .iter()
            .map(|owner| Some(store.begin_delta(*owner, span(0)).expect("open delta")))
            .collect::<Vec<_>>();
        let mut builders = owners
            .iter()
            .zip(&deltas)
            .map(|(_, delta)| Some(PublicationBuilder::new(&store, delta.as_ref().unwrap())))
            .collect::<Vec<_>>();
        let mut child_slots = (0..DEPTH).map(|_| None).collect::<Vec<_>>();

        for index in 0..DEPTH {
            let builder = builders[index].as_mut().unwrap();
            record_unit(&store, owners[index], builder, index);
            if index + 1 < DEPTH {
                child_slots[index] = Some(builder.reserve_child(&store, owners[index + 1]));
            }
            builder.mark_value_at_type_slot(fresh_site(span(index)), 0);
        }

        let mut retained = None;
        let mut prepared_root = None;
        for index in (0..DEPTH).rev() {
            let mut builder = builders[index].take().unwrap();
            if let Some(child) = retained.take() {
                builder.fill_child(child_slots[index].take().unwrap(), child);
            }
            let publication = builder.finish();
            let delta = deltas[index].take().unwrap();
            if index == 0 {
                prepared_root = Some(
                    publication
                        .prepare_root(&store, delta, Vec::new(), &context)
                        .expect("prepare root publication"),
                );
            } else {
                let (child, outputs) = publication
                    .close_child(&mut store, delta, owners[index - 1], Vec::new(), &context)
                    .expect("close child publication");
                assert!(outputs.is_empty());
                retained = Some(child);
            }
        }

        let payload_work = DEPTH * (DEPTH + 1) / 2;
        assert_eq!(
            publication_validation_work(),
            (DEPTH, payload_work - DEPTH),
            "each payload needs one full validation; goal-free retained occurrences reuse it"
        );
        assert_eq!(
            publication_work(),
            PublicationWorkCounters {
                command_freezes: DEPTH * 2,
                shape_freeze_visits: DEPTH * 3 - 1,
                payload_close_inputs: payload_work,
                payload_close_outputs: payload_work,
                ..PublicationWorkCounters::default()
            },
            "owner closes may process typed payloads but must not traverse command shape"
        );

        let mut elaborations = Elaborations::new();
        let outputs = prepared_root
            .unwrap()
            .commit_without_rec_order(&mut store, &mut elaborations);
        assert!(outputs.is_empty());
        assert_eq!(
            publication_work(),
            PublicationWorkCounters {
                command_freezes: DEPTH * 2,
                shape_freeze_visits: DEPTH * 3 - 1,
                payload_close_inputs: payload_work,
                payload_close_outputs: payload_work,
                final_shape_visits: DEPTH * 3 - 1,
                command_materializations: DEPTH * 2,
            },
            "the final commit must be the sole recursive command-shape walk"
        );
    }
}
