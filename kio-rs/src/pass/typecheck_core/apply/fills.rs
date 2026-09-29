use std::{collections::HashMap, sync::Arc};

use super::{DeferredTailResolution::*, OrdinaryFrontierReentry};
use crate::ast::{RecOrderTailRequirement, RecOrderTypeFlow};
use crate::normalization::Value;

// Fills intentionally treats the canonical resolved `__pair__` as special: resolved Intrinsic
// kind plus this exact scheme proves ordered one-to-one construction before its wrapper closes.
// An ordinary `Fn` with the same product-shaped scheme may reorder, duplicate, or drop operands,
// so it remains opaque. Only the two operands are staged; the fill transcript still determines
// relation order, and the original wrapper completes exactly once. Neither spelling nor
// declaration provenance grants the certificate.
pub(super) fn canonical_pair_certificate(
    direct: &super::PreparedDirectCallee,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> bool {
    matches!(
        direct.binders.last(),
        Some((
            _,
            crate::pass::typecheck_full::ResolvedBinder::Intrinsic { .. }
        ))
    ) && canonical_pair_scheme(direct, tcx)
}

#[cfg(test)]
type TargetedTestState<T> = std::cell::RefCell<Option<(String, crate::span::Span, T)>>;

#[cfg(test)]
thread_local! {
    static PAIR_TRANSPARENCY_BYPASS: TargetedTestState<std::rc::Rc<std::cell::Cell<usize>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[must_use]
pub(crate) struct PairTransparencyBypass(std::rc::Rc<std::cell::Cell<usize>>);

#[cfg(test)]
pub(crate) fn disable_pair_transparency_at(
    module_path: &str,
    span: crate::span::Span,
) -> PairTransparencyBypass {
    let hits = std::rc::Rc::new(std::cell::Cell::new(0));
    PAIR_TRANSPARENCY_BYPASS.with(|bypass| {
        let mut bypass = bypass.borrow_mut();
        assert!(bypass.is_none(), "pair-transparency bypasses cannot nest");
        *bypass = Some((module_path.to_owned(), span, hits.clone()));
    });
    PairTransparencyBypass(hits)
}

#[cfg(test)]
impl PairTransparencyBypass {
    pub(crate) fn hits(&self) -> usize {
        self.0.get()
    }
}

#[cfg(test)]
impl Drop for PairTransparencyBypass {
    fn drop(&mut self) {
        PAIR_TRANSPARENCY_BYPASS.with(|bypass| bypass.borrow_mut().take());
    }
}

#[cfg(test)]
fn pair_transparency_is_disabled(module_path: &str, span: crate::span::Span) -> bool {
    PAIR_TRANSPARENCY_BYPASS.with(|bypass| {
        let bypass = bypass.borrow();
        let Some((target_module, target_span, hits)) = bypass.as_ref() else {
            return false;
        };
        if target_module != module_path || *target_span != span {
            return false;
        }
        hits.set(
            hits.get()
                .checked_add(1)
                .expect("pair-transparency bypass hit count overflow"),
        );
        true
    })
}

fn canonical_pair_scheme(
    direct: &super::PreparedDirectCallee,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> bool {
    use crate::ast::Type;
    if !direct.synth.complete_scheme.is_complete() {
        return false;
    }
    let Type::Forall {
        param: left_binder,
        body,
        ..
    } = direct.synth.ty.as_type()
    else {
        return false;
    };
    let Type::Forall {
        param: right_binder,
        body,
        ..
    } = body.as_ref()
    else {
        return false;
    };
    let Type::Function {
        param,
        ret,
        abi_arity: 2,
        ..
    } = body.as_ref()
    else {
        return false;
    };
    let Type::Product {
        left: input_left,
        right: input_right,
        ..
    } = param.as_ref()
    else {
        return false;
    };
    let Type::Product {
        left: result_left,
        right: result_right,
        ..
    } = ret.as_ref()
    else {
        return false;
    };
    let binders =
        std::collections::HashSet::from([left_binder.name.clone(), right_binder.name.clone()]);
    let aliases = tcx.binder_alias_ctx(&binders);
    crate::pass::typecheck_core::require_type_equiv_state(
        input_left,
        result_left,
        direct.span,
        &aliases,
        direct.synth.ty.identity_is_canonical(),
        direct.synth.ty.identity_is_canonical(),
    )
    .is_ok()
        && crate::pass::typecheck_core::require_type_equiv_state(
            input_right,
            result_right,
            direct.span,
            &aliases,
            direct.synth.ty.identity_is_canonical(),
            direct.synth.ty.identity_is_canonical(),
        )
        .is_ok()
}

pub(super) struct MarkedCallState<'m> {
    phase: MarkedCallPhase<'m>,
}

pub(super) struct MarkedProjection<'m> {
    seal: InvocationSeal,
    specialization: Option<crate::pass::typecheck_core::IsolatedSpecializationContextBuilder>,
    header_cursor: ProjectedHeaderCursor,
    terminal_result: Option<crate::pass::typecheck_core::ScopedType>,
    transcript: Option<super::UserElaboratorCallTranscript>,
    sources: Vec<RetainedProjectedSource<'m>>,
    next_template: u32,
    next_producer: u32,
    next_close_producer: u32,
    producer_locations: Vec<ProducerLocation>,
    producer_completed: Vec<bool>,
    template_slots: Vec<ProjectedTemplateSlot>,
}

#[derive(Default)]
struct ProjectedTemplateSlot {
    source: Option<ProjectedTemplateSource>,
    lifted_runtime_return: Option<crate::ast::Type<crate::ast::Lowered>>,
}

#[derive(Clone)]
struct ProducerLocation {
    source: usize,
    path: Arc<[u8]>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FillRelationWork {
    pub(crate) batch_attempts: usize,
    pub(crate) batch_commits: usize,
    pub(crate) equations_submitted: usize,
    pub(crate) accepted_producers: usize,
    pub(crate) soft_restarts: usize,
    pub(crate) physical_closes: usize,
    pub(crate) producer_request_visits: usize,
    pub(crate) producer_advance_visits: usize,
    pub(crate) relation_wave_scan_visits: usize,
}

#[cfg(test)]
thread_local! {
    static FILL_RELATION_WORK: std::cell::Cell<FillRelationWork> = const {
        std::cell::Cell::new(FillRelationWork {
            batch_attempts: 0,
            batch_commits: 0,
            equations_submitted: 0,
            accepted_producers: 0,
            soft_restarts: 0,
            physical_closes: 0,
            producer_request_visits: 0,
            producer_advance_visits: 0,
            relation_wave_scan_visits: 0,
        })
    };
}

#[cfg(test)]
pub(crate) fn reset_fill_relation_work() {
    FILL_RELATION_WORK.with(|work| work.set(FillRelationWork::default()));
}

#[cfg(test)]
pub(crate) fn fill_relation_work() -> FillRelationWork {
    FILL_RELATION_WORK.with(std::cell::Cell::get)
}

#[cfg(test)]
fn record_fill_relation_work(update: impl FnOnce(&mut FillRelationWork)) {
    FILL_RELATION_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

impl<'m> MarkedProjection<'m> {
    pub(super) fn capabilities(&self) -> (MarkedComptimeProof, FillContext) {
        capabilities_for(&self.seal)
    }

    pub(super) fn retain_transcript(&mut self, transcript: super::UserElaboratorCallTranscript) {
        assert!(
            self.transcript.replace(transcript).is_none(),
            "a fills projection retained two completion transcripts"
        );
    }

    pub(super) fn advance_declared_headers(
        &mut self,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<Option<ProjectedHeaderRequest>, crate::error::Error> {
        if self.header_cursor.phase == ProjectedHeaderPhase::Prepare
            && self.header_cursor.source == 0
        {
            assert!(self.sources.windows(2).all(|sources| {
                let [left, right] = sources else {
                    unreachable!("a source-order window has exactly two entries")
                };
                left.layer < right.layer
                    || left.layer == right.layer
                        && left.expanded_start + left.expanded_width <= right.expanded_start
            }));
        }
        loop {
            let phase = self.header_cursor.phase;
            let source_index = self.header_cursor.source;
            if source_index == self.sources.len() {
                self.header_cursor.source = 0;
                self.header_cursor.phase = match phase {
                    ProjectedHeaderPhase::Prepare => ProjectedHeaderPhase::Reserve,
                    ProjectedHeaderPhase::Reserve => ProjectedHeaderPhase::Install,
                    ProjectedHeaderPhase::Install => {
                        self.link_declared_headers(span, frontier, tcx)?;
                        ProjectedHeaderPhase::SelectLinkedHeaders
                    }
                    ProjectedHeaderPhase::SelectLinkedHeaders => {
                        self.seal_declared_headers(frontier, tcx)?;
                        ProjectedHeaderPhase::CloseProducerlessPairs
                    }
                    ProjectedHeaderPhase::CloseProducerlessPairs => ProjectedHeaderPhase::Complete,
                    ProjectedHeaderPhase::Complete => return Ok(None),
                };
                continue;
            }

            if phase == ProjectedHeaderPhase::Complete {
                return Ok(None);
            }
            let source = self
                .sources
                .get(source_index)
                .expect("a projected header cursor exceeded its source plan");
            if phase == ProjectedHeaderPhase::SelectLinkedHeaders {
                let state = source
                    .state
                    .as_ref()
                    .expect("a linked source lost its staged state");
                if !has_unselected_projected_lambda(state) {
                    self.header_cursor.source += 1;
                    continue;
                }
                refresh_linked_lambda_context(state, frontier, tcx)?;
            }
            if phase == ProjectedHeaderPhase::CloseProducerlessPairs
                && !matches!(source.state, Some(StagedOperand::Pair(_)))
            {
                self.header_cursor.source += 1;
                continue;
            }
            let parent = source
                .state
                .as_ref()
                .and_then(staged_header_parent)
                .unwrap_or_else(|| frontier.owner());
            let close_expected = if phase == ProjectedHeaderPhase::CloseProducerlessPairs {
                let Some(StagedOperand::Pair(pair)) = source.state.as_ref() else {
                    unreachable!("a producerless-pair cursor crossed another source family")
                };
                if staged_pair_is_initially_completed(pair) {
                    let recipe = source.header.recipe().ty.ty.clone();
                    let (store, _owner, delta, _publication) = frontier.parts_mut();
                    let expected = store.require_goal_free_after_delta(delta, recipe, tcx)?;
                    Some(store.rebase_goal_free_type_to_owner(expected, parent, span, tcx)?)
                } else {
                    None
                }
            } else {
                None
            };
            let request = ProjectedHeaderRequest {
                parent,
                close_expected,
            };
            if parent != frontier.owner() {
                #[cfg(test)]
                record_projected_header_work(ProjectedHeaderWorkEvent::AtAncestor(
                    phase,
                    source_index,
                ));
                return Ok(Some(request));
            }
            self.resume_declared_header(request, span, frontier, tcx)?;
        }
    }

    pub(super) fn resume_declared_header(
        &mut self,
        request: ProjectedHeaderRequest,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<(), crate::error::Error> {
        let phase = self.header_cursor.phase;
        let source_index = self.header_cursor.source;
        assert_eq!(
            request.parent,
            frontier.owner(),
            "a projected header request resumed through another retained parent"
        );
        let retained = self
            .sources
            .get_mut(source_index)
            .expect("a projected header request exceeded its source plan");
        let state = retained
            .state
            .as_mut()
            .expect("a retained projected source lost its header state");
        assert_eq!(
            staged_header_parent(state).unwrap_or(request.parent),
            request.parent,
            "a projected header request changed retained root owners"
        );
        match phase {
            ProjectedHeaderPhase::Prepare => {
                assert!(matches!(retained.header, RetainedProjectedHeader::Open(_)));
                let state = retained
                    .state
                    .take()
                    .expect("a retained projected source lost its header state");
                retained.state = Some(prepare_projected_source_header(
                    state,
                    super::frontier::FrontierMode::LexicalFallback,
                    frontier,
                    tcx,
                )?);
            }
            ProjectedHeaderPhase::Reserve => {
                let RetainedProjectedHeader::Open(public) = &retained.header else {
                    unreachable!("a projected source reserved its header twice")
                };
                let public = public.clone();
                let mut exports = Vec::new();
                let structural = reserve_projected_header(
                    retained
                        .state
                        .as_mut()
                        .expect("a retained projected source lost its prepared header state"),
                    public.clone(),
                    &mut exports,
                    frontier,
                    tcx,
                )?;
                retained.header = RetainedProjectedHeader::Prepared {
                    public,
                    structural,
                    exports: Some(exports),
                };
            }
            ProjectedHeaderPhase::Install => {
                let RetainedProjectedHeader::Prepared { exports, .. } = &mut retained.header else {
                    unreachable!("a projected source installed an unreserved header")
                };
                let mut exports = exports
                    .take()
                    .expect("a projected source installed its header twice")
                    .into_iter();
                install_projected_header_exports(state, &mut exports, frontier, tcx)?;
                assert!(
                    exports.next().is_none(),
                    "a projected header retained an uninstalled owner edge"
                );
            }
            ProjectedHeaderPhase::SelectLinkedHeaders => {
                let state = retained
                    .state
                    .take()
                    .expect("a linked source lost its staged state");
                retained.state = Some(select_linked_lambda_headers(state, frontier, tcx)?);
            }
            ProjectedHeaderPhase::CloseProducerlessPairs => {
                let Some(StagedOperand::Pair(pair)) = retained.state.as_mut() else {
                    unreachable!("a producerless-pair request crossed another source family")
                };
                collapse_initial_completed_pair_children(pair, frontier, tcx)?;
                if let Some(expected) = request.close_expected {
                    assert!(
                        staged_pair_is_closed(pair),
                        "a producerless projected pair did not close postorder"
                    );
                    let state = retained
                        .state
                        .take()
                        .expect("a producerless projected pair lost its staged shell");
                    let StagedOperand::Pair(pair) = state else {
                        unreachable!("a producerless projected pair changed source family")
                    };
                    let source = pair.source;
                    let completed = finalize_staged_pair(*pair, frontier, tcx)?;
                    require_closed_recipe_match(
                        completed.scoped().clone(),
                        expected,
                        span,
                        frontier,
                        tcx,
                    )?;
                    retained.state = Some(StagedOperand::Completed {
                        source,
                        completed,
                        leaf: None,
                    });
                }
            }
            ProjectedHeaderPhase::Complete => {
                unreachable!("a completed projected header phase yielded owner work")
            }
        }
        #[cfg(test)]
        record_projected_header_work(ProjectedHeaderWorkEvent::Work(phase, source_index));
        self.header_cursor.source += 1;
        Ok(())
    }

    #[allow(clippy::unused_enumerate_index)] // indices pin exact link/seal order in cfg(test) causals
    fn link_declared_headers(
        &mut self,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<(), crate::error::Error> {
        for (_source_index, source) in self.sources.iter().enumerate() {
            let RetainedProjectedHeader::Prepared {
                public,
                structural,
                exports: None,
            } = &source.header
            else {
                unreachable!("a projected root linked before every child header installed")
            };
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store.constrain(delta, public.clone(), structural.clone(), span, tcx)?;
            #[cfg(test)]
            record_projected_header_work(ProjectedHeaderWorkEvent::Link(_source_index));
        }
        Ok(())
    }

    #[allow(clippy::unused_enumerate_index)] // indices pin exact link/seal order in cfg(test) causals
    fn seal_declared_headers(
        &mut self,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<(), crate::error::Error> {
        let mut specialization = self.specialization.take().unwrap_or_else(|| {
            crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(tcx)
        });
        for (_source_index, source) in self.sources.iter_mut().enumerate() {
            let RetainedProjectedHeader::Prepared {
                structural,
                exports: None,
                ..
            } = &source.header
            else {
                unreachable!("a projected source sealed before header installation")
            };
            let progressed = {
                let (store, _owner, delta, _publication) = frontier.parts_mut();
                store.zonk_for_progress(delta, structural.clone(), tcx)?
            };
            let recipe = project_operand_recipe_from_header(
                source
                    .state
                    .as_ref()
                    .expect("a retained projected source lost its installed header state"),
                progressed,
                &self.seal,
                &mut specialization,
                tcx,
            )?;
            source.header = RetainedProjectedHeader::Sealed(recipe);
            #[cfg(test)]
            record_projected_header_work(ProjectedHeaderWorkEvent::Seal(_source_index));
        }
        assert!(
            self.specialization.replace(specialization).is_none(),
            "a projected header seal retained two specialization builders"
        );
        Ok(())
    }

    pub(super) fn declared_inputs(
        &mut self,
        action: &crate::pass::typecheck_full::PreparedUserElaboratorAction<'m>,
        transcript: &super::UserElaboratorCallTranscript,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<
        (
            Vec<Value>,
            crate::pass::typecheck_full::FrozenProjectedResidualAbi,
        ),
        crate::error::Error,
    > {
        let mut specialization = self.specialization.take().unwrap_or_else(|| {
            crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(tcx)
        });
        let mut inputs = Vec::new();
        let mut next_result_root = 0u32;
        assert!(
            self.sources
                .iter()
                .all(|source| matches!(source.header, RetainedProjectedHeader::Sealed(_))),
            "declared fills inputs observed an unsealed projected header"
        );
        for source in &mut self.sources {
            snapshot_projected_recipe(
                source.header.recipe_mut(),
                &mut specialization,
                frontier,
                tcx,
            )?;
        }
        for source in &self.sources {
            validate_projected_source_before_action(
                source
                    .state
                    .as_ref()
                    .expect("a retained projected source lost its staged state"),
                source.header.recipe(),
            )?;
        }
        let mut observed_types = Vec::new();
        let mut call_layers = Vec::with_capacity(transcript.layers.len());
        for layer in &transcript.layers {
            let mut type_slots = Vec::with_capacity(layer.type_slots.len());
            for slot in &layer.type_slots {
                let progressed = match slot {
                    Some(slot) => {
                        let (store, _owner, delta, _publication) = frontier.parts_mut();
                        Some(store.zonk_for_progress(delta, slot.clone(), tcx)?)
                    }
                    None => None,
                };
                observed_types.extend(progressed.iter().map(|ty| ty.ty().clone()));
                type_slots.push(progressed.map(|ty| ty.ty().clone()));
            }
            for source in &layer.sources {
                let progressed = {
                    let (store, _owner, delta, _publication) = frontier.parts_mut();
                    store.zonk_for_progress(delta, source.expected.clone(), tcx)?
                };
                observed_types.push(progressed.ty().clone());
            }
            let mut expanded_value_slots = 0usize;
            for source in &layer.sources {
                assert_eq!(
                    source.expanded_start, expanded_value_slots,
                    "projected source ranges must cover their frozen call layer contiguously"
                );
                assert_ne!(
                    source.expanded_width, 0,
                    "a projected source must occupy a frozen call coordinate"
                );
                expanded_value_slots = expanded_value_slots
                    .checked_add(source.expanded_width)
                    .expect("a projected source range overflowed");
            }
            if layer.value_layer_consumed && layer.sources.is_empty() {
                expanded_value_slots = 1;
            }
            call_layers.push(
                crate::pass::typecheck_full::ProjectedUserElaboratorCallLayerShape {
                    type_slots,
                    value_layer_consumed: layer.value_layer_consumed,
                    expanded_value_slots,
                    has_value_sources: !layer.sources.is_empty(),
                },
            );
        }
        let plan = action.projected_abi_plan(
            &crate::pass::typecheck_full::ProjectedUserElaboratorCallShape {
                layers: call_layers,
                observed_types,
            },
            tcx,
        );
        let terminal_result = {
            let (store, owner, _delta, _publication) = frontier.parts_mut();
            store.scoped_type_with_lexical_foralls(
                owner,
                plan.terminal_result.ty.clone(),
                &plan.terminal_result.lexical_binders,
                action.span(),
            )?
        };
        assert!(
            self.terminal_result.replace(terminal_result).is_none(),
            "one fills invocation projected two terminal result types"
        );
        let mut expanded_recipes = Vec::new();
        for source in &self.sources {
            let pieces =
                split_projected_recipe(source.header.recipe().clone(), source.expanded_width)?;
            assert_eq!(pieces.len(), source.expanded_width);
            for (offset, piece) in pieces.into_iter().enumerate() {
                expanded_recipes.push((source.layer, source.expanded_start + offset, piece));
            }
        }
        let mut expanded_recipes = expanded_recipes.into_iter().peekable();
        let mut previous_requested_recipe_end = None;
        for slot in plan.slots {
            match slot {
                crate::pass::typecheck_full::ProjectedUserElaboratorSlot::Type {
                    layer,
                    slot,
                    result_focus,
                    residual,
                } => {
                    let ty = if let Some(residual) = residual {
                        let (store, owner, _delta, _publication) = frontier.parts_mut();
                        store.scoped_type_with_lexical_foralls(
                            owner,
                            residual.ty,
                            &residual.lexical_binders,
                            action.span(),
                        )?
                    } else {
                        transcript.layers[layer].type_slots[slot]
                            .as_ref()
                            .expect("a ready projected type slot remained absent")
                            .clone()
                    };
                    {
                        let (store, _owner, _delta, _publication) = frontier.parts_mut();
                        specialization.record_carrier_goals(&ty, store)?;
                    }
                    let (ty, closed) = {
                        let (store, _owner, delta, _publication) = frontier.parts_mut();
                        let ty = store.zonk_for_progress(delta, ty, tcx)?;
                        let closed = store.try_zonk_goal_free(delta, ty.clone(), tcx)?;
                        (ty, closed)
                    };
                    let closed = closed
                        .as_ref()
                        .map(|closed| {
                            crate::pass::typecheck_full::reflect_projected_type(closed.ty(), tcx)
                        })
                        .transpose()?;
                    let focus = result_focus.then(|| {
                        let root = ProjectedResultRoot(next_result_root);
                        next_result_root = next_result_root
                            .checked_add(1)
                            .expect("projected result-root count exceeded u32");
                        ProjectedFocus {
                            result_root: root,
                            path: Arc::from([]),
                        }
                    });
                    inputs.push(crate::normalization::projected_value(ProjectedValue(
                        ProjectedValueKind::Type(projected_type(
                            &self.seal,
                            ty,
                            closed,
                            focus,
                            ProducerSet::default(),
                            &mut specialization,
                            tcx,
                        )?),
                    )));
                }
                crate::pass::typecheck_full::ProjectedUserElaboratorSlot::Value {
                    layer,
                    expanded_start,
                    expanded_width,
                    implicit_unit,
                    residual,
                } => {
                    if implicit_unit {
                        assert!(
                            residual.is_none(),
                            "an implicit projected Unit cannot remain residual"
                        );
                        let span = action.span();
                        inputs.push(Value::CheckedTerm(crate::normalization::EvalRef::new(
                            crate::normalization::CheckedTerm::new(
                                crate::ast::Expr::Unit {
                                    occurrence: Default::default(),
                                    meta: crate::ast::Meta::new(span),
                                },
                                crate::ast::Type::Unit {
                                    meta: crate::ast::Meta::new(span),
                                },
                            ),
                        )));
                        continue;
                    }
                    let recipe = if let Some(residual) = residual {
                        let ty = {
                            let (store, owner, _delta, _publication) = frontier.parts_mut();
                            let ty = store.scoped_type_with_lexical_foralls(
                                owner,
                                residual.ty,
                                &residual.lexical_binders,
                                action.span(),
                            )?;
                            specialization.record_carrier_goals(&ty, store)?;
                            ty
                        };
                        ProjectedCheckedRecipe {
                            ty: projected_type(
                                &self.seal,
                                ty,
                                None,
                                None,
                                ProducerSet::default(),
                                &mut specialization,
                                tcx,
                            )?,
                            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                                name: residual.name,
                            }),
                        }
                    } else {
                        let requested_start = (layer, expanded_start);
                        if let Some(previous_end) = previous_requested_recipe_end {
                            assert!(
                                previous_end <= requested_start,
                                "declared fills value slots left authored ABI order"
                            );
                        }
                        let expanded_end = expanded_start
                            .checked_add(expanded_width)
                            .expect("declared fills value-slot width overflow");
                        previous_requested_recipe_end = Some((layer, expanded_end));
                        while expanded_recipes.peek().is_some_and(
                            |(recipe_layer, recipe_slot, _)| {
                                (*recipe_layer, *recipe_slot) < requested_start
                            },
                        ) {
                            let (recipe_layer, _recipe_slot, _recipe) = expanded_recipes
                                .next()
                                .expect("a peeked projected recipe remains available");
                            assert!(
                                action.value_layer_forces_unrequested_source(recipe_layer),
                                "a nonzero-ABI projected source escaped its declared fills slot"
                            );
                        }
                        let pieces = (expanded_start..expanded_end)
                            .map(|expanded| {
                                let (recipe_layer, recipe_slot, recipe) = expanded_recipes
                                    .next()
                                    .unwrap_or_else(|| {
                                        unreachable!(
                                            "a declared fills value slot escaped the marked descent gate ({layer}:{expanded_start}+{expanded_width})"
                                        )
                                    });
                                assert_eq!(
                                    (recipe_layer, recipe_slot),
                                    (layer, expanded),
                                    "projected sources left authored ABI order"
                                );
                                recipe
                            })
                            .collect();
                        fold_projected_recipe_packet(pieces)?
                    };
                    inputs.push(crate::normalization::projected_value(ProjectedValue(
                        ProjectedValueKind::CheckedRecipe(recipe),
                    )));
                }
            }
        }
        for (recipe_layer, _recipe_slot, _recipe) in expanded_recipes {
            assert!(
                action.value_layer_forces_unrequested_source(recipe_layer),
                "a nonzero-ABI projected source escaped its declared fills slot"
            );
        }
        self.seal
            .install_specialization_context(specialization.finish());
        Ok((inputs, plan.frozen_residual))
    }

    pub(super) fn resolve_deferred_rec_tails(
        &mut self,
        checked: &ProjectedCheckedResult,
        has_residual_abstraction: bool,
        span: crate::span::Span,
    ) -> Result<(), crate::error::Error> {
        let mut candidates = vec![None; self.next_template as usize];
        for source in &self.sources {
            collect_deferred_tail_candidates(
                source
                    .state
                    .as_ref()
                    .expect("a retained projected source lost its staged state"),
                &mut candidates,
            );
        }
        if candidates.iter().all(Option::is_none) {
            return Ok(());
        }
        assert!(
            candidates
                .iter()
                .flatten()
                .any(|candidate| { candidate.requirement == RecOrderTailRequirement::Required }),
            "a deferred-tail call retained no required recursive source"
        );

        let mut usage = vec![DeferredTailUsage::default(); candidates.len()];
        if let ProjectedCheckedResult::Recipe(recipe) = checked {
            if !recipe.ty.seal.matches(&self.seal) {
                return Err(crate::error::Error::elaborator(
                    span,
                    "elaborator returned a recipe from another invocation",
                ));
            }
            DeferredTailClassifier::new(&candidates, &mut usage)
                .fold(recipe, DeferredTailRole::Tail);
        }

        let mut resolutions = resolve_deferred_tail_decisions(&candidates, &usage)?;
        if has_residual_abstraction {
            for (candidate, resolution) in candidates.iter().zip(&resolutions) {
                if matches!(
                    resolution,
                    Some((_, super::DeferredTailResolution::Lifted(_)))
                ) {
                    return Err(crate::pass::desugar::rec_escape_error(
                        candidate
                            .as_ref()
                            .expect("a deferred-tail resolution lost its source candidate")
                            .span,
                    ));
                }
            }
        }
        self.template_slots
            .resize_with(self.next_template as usize, ProjectedTemplateSlot::default);
        for source in &mut self.sources {
            install_deferred_tail_resolutions(
                source
                    .state
                    .as_mut()
                    .expect("a retained projected source lost its staged state"),
                &mut resolutions,
                &mut self.template_slots,
            );
        }
        assert!(
            resolutions.iter().all(Option::is_none),
            "a classified deferred-tail source lost its staged carrier"
        );
        Ok(())
    }

    pub(super) fn prepare_adoption(
        &self,
        context: &FillContext,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<FillAdoption, crate::error::Error> {
        if !self.seal.matches(&context.seal) {
            return Err(crate::error::Error::elaborator(
                span,
                "fills elaborator returned relations from another invocation",
            ));
        }
        #[cfg(test)]
        let mut returned_relation_observations =
            returned_fill_relation_observer_matches(&tcx.env.module_path, span)
                .then(|| Vec::with_capacity(context.len));
        let mut source_order = Vec::with_capacity(context.len);
        let mut current = context.head.as_deref();
        while let Some(relation) = current {
            source_order.push(relation);
            current = relation.previous.as_deref();
        }
        source_order.reverse();

        // Authenticate the complete immutable transcript before the relation
        // group mutates the live delta.
        let mut relations = Vec::with_capacity(source_order.len());
        let mut visited_producers = std::collections::HashSet::new();
        fn validate_producers(
            node: &crate::normalization::EvalRef<ProducerDag>,
            bound: u32,
            visited: &mut std::collections::HashSet<*const ProducerDag>,
        ) -> bool {
            if !visited.insert(crate::normalization::EvalRef::as_ptr(node)) {
                return true;
            }
            match node.as_ref() {
                ProducerDag::Leaf(ordinal) => *ordinal < bound,
                ProducerDag::Union(left, right) => {
                    validate_producers(left, bound, visited)
                        && validate_producers(right, bound, visited)
                }
            }
        }
        for relation in source_order {
            if relation.producers.0.as_ref().is_some_and(|root| {
                !validate_producers(root, self.next_producer, &mut visited_producers)
            }) {
                return Err(crate::error::Error::elaborator(
                    span,
                    "fills elaborator returned a relation with an unknown producer",
                ));
            }
            let destination = match &relation.destination {
                Value::Projected(value) => match &value.0 {
                    ProjectedValueKind::Type(ty)
                        if ty.seal.matches(&self.seal) && ty.focus.is_some() =>
                    {
                        ty.ty.clone()
                    }
                    ProjectedValueKind::Type(_) | ProjectedValueKind::CheckedRecipe(_) => {
                        return Err(crate::error::Error::elaborator(
                            span,
                            "fills elaborator returned an unauthenticated destination",
                        ));
                    }
                },
                _ => {
                    return Err(crate::error::Error::elaborator(
                        span,
                        "fills elaborator returned a non-projected destination",
                    ));
                }
            };
            let candidate = match &relation.candidate {
                Value::Projected(value) => match &value.0 {
                    ProjectedValueKind::Type(ty) if ty.seal.matches(&self.seal) => ty.ty.clone(),
                    ProjectedValueKind::Type(_) | ProjectedValueKind::CheckedRecipe(_) => {
                        return Err(crate::error::Error::elaborator(
                            span,
                            "fills elaborator returned an unauthenticated type candidate",
                        ));
                    }
                },
                Value::ReflType(ty) => {
                    let ty = crate::ast::convert_type::<
                        crate::ast::UncheckedPrime,
                        crate::ast::Lowered,
                    >(ty.as_type());
                    let (store, owner, _delta, _publication) = frontier.parts_mut();
                    store.scoped_type(
                        owner,
                        crate::pass::typecheck_core::InternedType::fresh_canonical(ty),
                        span,
                    )?
                }
                _ => {
                    return Err(crate::error::Error::elaborator(
                        span,
                        "fills elaborator returned a candidate other than `__Type__`",
                    ));
                }
            };
            #[cfg(test)]
            if let Some(observations) = returned_relation_observations.as_mut() {
                observations.push(observe_authenticated_fill_relation(relation, &candidate));
            }
            relations.push(AdoptedFillRelation {
                destination,
                candidate,
                producers: relation.producers.clone(),
            });
        }

        #[cfg(test)]
        if let Some(observations) = returned_relation_observations {
            record_returned_fill_relations(&tcx.env.module_path, span, observations);
        }

        {
            let equations = relations.iter().map(|relation| {
                (
                    relation.candidate.clone(),
                    relation.destination.clone(),
                    span,
                )
            });
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store.constrain_equations_atomically(delta, equations, tcx)?;
        }

        let (relation_producers, independent_producers) = freeze_relation_producers(
            relations.iter().map(|relation| &relation.producers),
            self.next_producer,
        );
        Ok(FillAdoption {
            relations,
            relation_producers,
            independent_producers,
            producer_mode: super::frontier::FrontierMode::ExpectedOnly,
            producer_index: 0,
            sweep_complete: true,
            barrier_applied: false,
            phase: FillAdoptionPhase::Relations,
        })
    }

    pub(super) fn materialize(
        mut self,
        checked: ProjectedCheckedResult,
        output: crate::pass::typecheck_core::ScopedType,
        residual_binders: Vec<crate::ast::TypeParam>,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<ProjectedMaterialization, crate::error::Error> {
        let transcript = self
            .transcript
            .take()
            .expect("a completed fills projection lost its public call transcript");
        let mut layers = Vec::with_capacity(transcript.layers.len());
        for layer in transcript.layers {
            let mut type_slots = Vec::with_capacity(layer.type_slots.len());
            for slot in layer.type_slots {
                let Some(slot) = slot else {
                    type_slots.push(crate::pass::typecheck_full::TypedUserElaboratorTypeSlot {
                        resolved: None,
                        occurrence: None,
                    });
                    continue;
                };
                let (resolved, occurrence) = {
                    let (store, _owner, delta, _publication) = frontier.parts_mut();
                    let occurrence = store.zonk_for_progress(delta, slot.clone(), tcx)?;
                    let resolved = store.require_goal_free_after_delta(delta, slot, tcx)?;
                    (resolved, occurrence)
                };
                type_slots.push(crate::pass::typecheck_full::TypedUserElaboratorTypeSlot {
                    resolved: Some(resolved.ty().clone()),
                    occurrence: Some(occurrence.ty().clone()),
                });
            }
            let mut sources = Vec::with_capacity(layer.sources.len());
            for source in layer.sources {
                let expected = {
                    let (store, _owner, delta, _publication) = frontier.parts_mut();
                    store.require_goal_free_after_delta(delta, source.expected, tcx)?
                };
                sources.push(crate::pass::typecheck_full::TypedUserElaboratorCallSource {
                    source_index: source.source_index,
                    source: source.source,
                    expanded_start: source.expanded_start,
                    expanded_width: source.expanded_width,
                    expected: expected.ty().clone(),
                });
            }
            layers.push(crate::pass::typecheck_full::TypedUserElaboratorCallLayer {
                type_slots,
                sources,
                value_layer_consumed: layer.value_layer_consumed,
            });
        }
        let public_residual_result = {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store.require_goal_free_after_delta(delta, output, tcx)?
        };
        let typed = crate::pass::typecheck_full::TypedUserElaboratorCall {
            layers,
            result_ty: Some(public_residual_result.ty().clone()),
        };
        assert_eq!(
            self.next_close_producer, self.next_producer,
            "fill adoption completed before every projected producer closed"
        );
        assert!(
            self.producer_completed.iter().all(|completed| *completed),
            "fill adoption completed with an unclosed projected producer"
        );
        let mut template_slots = std::mem::take(&mut self.template_slots);
        assert_eq!(
            template_slots.len(),
            self.next_template as usize,
            "a projected invocation lost its dense template source table"
        );
        for retained in &self.sources {
            let state = retained
                .state
                .as_ref()
                .expect("a retained projected source lost its completed state");
            collect_projected_operand_source(
                state,
                retained.layer,
                &mut template_slots,
                frontier,
                tcx,
            )?;
        }
        let mut sources = Vec::with_capacity(template_slots.len());
        let mut runtime_returns = Vec::with_capacity(template_slots.len());
        for slot in template_slots {
            sources.push(
                slot.source
                    .expect("a dense projected template index lost its source"),
            );
            runtime_returns.push(slot.lifted_runtime_return);
        }
        // The evaluator closes only the body beneath the residual public eta
        // plan, whose terminal type was retained independently when the
        // projected ABI was declared.
        let public_terminal_result = {
            let terminal_result = self
                .terminal_result
                .take()
                .expect("a completed fills projection lost its projected eta-body result");
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store
                .require_goal_free_after_delta(delta, terminal_result, tcx)?
                .into_interned_type()
        };
        let public = match checked {
            ProjectedCheckedResult::Closed(checked) => {
                assert!(
                    runtime_returns.iter().all(Option::is_none),
                    "a closed checked term retained a lifted projected callback"
                );
                checked
            }
            ProjectedCheckedResult::Recipe(recipe) => {
                close_projected_recipe(&recipe, residual_binders, span, frontier, tcx)?
            }
        };
        crate::pass::typecheck_core::require_type_equiv_state(
            &crate::ast::convert_type::<crate::ast::UncheckedPrime, crate::ast::Lowered>(
                public.ty(),
            ),
            public_terminal_result.as_type(),
            span,
            &tcx.env.alias_ctx(),
            true,
            public_terminal_result.identity_is_canonical(),
        )?;
        let quoted_public_recipe = sources
            .iter()
            .any(|source| {
                tcx.elaborations
                    .captures_rec_quote_expression(&source.source)
            })
            .then(|| {
                Box::new(crate::pass::typecheck_full::QuotedPublicRecipe {
                    checked: public.clone(),
                    terminal_adapted: runtime_returns.iter().any(Option::is_some),
                })
            });
        let (checked, checked_result) = if runtime_returns.iter().all(Option::is_none) {
            (public, public_terminal_result.clone())
        } else {
            let (runtime_sources, runtime_result, continuation) = prepare_lifted_callback_types(
                &mut sources,
                &runtime_returns,
                &public_terminal_result,
                frontier,
                tcx,
            )?;
            let continuation_type =
                crate::pass::typecheck_full::reflect_projected_type(&continuation.ty, tcx)?;
            let continuation_term = crate::normalization::EvalRef::new(
                crate::normalization::CheckedTerm::template_value(
                    sources.len(),
                    continuation_type.as_type().clone(),
                ),
            );
            sources.push(continuation);
            let runtime_output =
                crate::pass::typecheck_full::reflect_projected_type(&runtime_result, tcx)?
                    .as_type()
                    .clone();
            let runtime = public.adapt_lifted_terminal_callbacks(
                &runtime_sources,
                runtime_output,
                continuation_term,
            );
            crate::pass::typecheck_core::require_type_equiv_state(
                &crate::ast::convert_type::<crate::ast::UncheckedPrime, crate::ast::Lowered>(
                    runtime.ty(),
                ),
                runtime_result.as_type(),
                span,
                &tcx.env.alias_ctx(),
                true,
                runtime_result.identity_is_canonical(),
            )?;
            (crate::normalization::EvalRef::new(runtime), runtime_result)
        };
        Ok(ProjectedMaterialization {
            typed,
            checked,
            quoted_public_recipe,
            public_terminal_result,
            checked_result,
            sources,
            public_residual_result,
        })
    }

    pub(super) fn advance_producer(
        &mut self,
        producer: u32,
        producer_mode: super::frontier::FrontierMode,
        inherited_mode: super::frontier::FrontierMode,
        relation_probe: bool,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<bool, crate::error::Error> {
        let location = self
            .producer_locations
            .get(producer as usize)
            .expect("a frozen fill phase referenced an unknown producer")
            .clone();
        let retained = self
            .sources
            .get_mut(location.source)
            .expect("a fill producer lost its retained source");
        assert!(
            retained.inherited_mode.rank() <= inherited_mode.rank(),
            "a retained source resumed before its original inherited mode"
        );
        retained.inherited_mode = inherited_mode;
        assert!(
            producer_mode.rank() <= retained.inherited_mode.rank(),
            "a fill producer advanced beyond its enclosing inherited mode"
        );
        advance_projected_source_at_path(
            &mut retained.state,
            &location.path,
            producer_mode,
            relation_probe,
            frontier,
            tcx,
        )
    }

    fn producer_is_complete(&self, producer: u32) -> bool {
        *self
            .producer_completed
            .get(producer as usize)
            .expect("a fills phase referenced an unknown producer completion bit")
    }

    fn producer_parent(&self, producer: u32) -> crate::ast::TypeGoalOwner {
        let location = self
            .producer_locations
            .get(producer as usize)
            .expect("a fills request referenced an unknown producer");
        staged_operand_parent(
            self.sources
                .get(location.source)
                .and_then(|retained| retained.state.as_ref())
                .expect("a fills producer lost its retained source"),
        )
    }

    fn producer_expected_snapshot(
        &self,
        producer: u32,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<Option<crate::pass::typecheck_core::ScopedType>, crate::error::Error> {
        let location = self
            .producer_locations
            .get(producer as usize)
            .expect("a fills request referenced an unknown producer");
        let recipe = projected_recipe_at_path(
            self.sources
                .get(location.source)
                .expect("a fills producer lost its retained source")
                .header
                .recipe(),
            &location.path,
        );
        let parent = self.producer_parent(producer);
        let boundary = staged_operand_owner(self.producer_state(producer));
        frontier.isolate_producer(boundary, span, tcx)?;
        let (store, _owner, delta, _publication) = frontier.parts_mut();
        store
            .try_zonk_goal_free(delta, recipe.ty.ty.clone(), tcx)?
            .map(|expected| store.rebase_goal_free_type_to_owner(expected, parent, span, tcx))
            .transpose()
    }

    pub(super) fn resume_producer(
        &mut self,
        request: FillProducerRequest,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<ResumedFillProducer, crate::error::Error> {
        assert_eq!(
            frontier.owner(),
            request.parent,
            "a fills producer resumed through another retained parent"
        );
        if matches!(
            self.producer_state(request.producer),
            StagedOperand::OpenCompleted { .. }
        ) {
            return Ok(ResumedFillProducer {
                producer: request.producer,
                complete: true,
                newly_complete: false,
            });
        }
        #[cfg(test)]
        record_fill_relation_work(|work| work.producer_advance_visits += 1);
        if let Some(expected) = request.expected {
            self.install_producer_expected(
                request.producer,
                request.parent,
                expected,
                frontier,
                tcx,
            )?;
        }
        let complete = self.advance_producer(
            request.producer,
            request.producer_mode,
            request.inherited_mode,
            request.relation_probe,
            frontier,
            tcx,
        )?;
        Ok(ResumedFillProducer {
            producer: request.producer,
            complete,
            newly_complete: complete,
        })
    }

    pub(super) fn accept_resumed_producer(
        &mut self,
        resumed: &ResumedFillProducer,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<(), crate::error::Error> {
        if !resumed.complete {
            return Ok(());
        }
        if let StagedOperand::OpenCompleted {
            accepted: true,
            #[cfg(test)]
            accepted_by_relation_wave,
            ..
        } = self.producer_state(resumed.producer)
        {
            assert!(
                !resumed.newly_complete,
                "a newly completed fills producer was already accepted"
            );
            #[cfg(test)]
            assert!(
                !*accepted_by_relation_wave,
                "an independent fills producer carried relation-wave acceptance"
            );
            return Ok(());
        }
        let StagedOperand::OpenCompleted { child, preview, .. } =
            self.producer_state(resumed.producer)
        else {
            unreachable!("a completed producer lost its retained child")
        };
        let recipe = self.producer_recipe(resumed.producer).ty.ty.clone();
        let obligations = frontier
            .store()
            .producer_obligations(child.completion_delta(), tcx)?;
        let equations = vec![(preview.clone(), recipe, span)];
        let wrappers = self.pair_wrapper_deltas();
        frontier.accept_producer_wave(wrappers, vec![obligations], equations, tcx)?;
        let StagedOperand::OpenCompleted {
            accepted,
            #[cfg(test)]
            accepted_by_relation_wave,
            ..
        } = self.producer_state_mut(resumed.producer)
        else {
            unreachable!("an accepted fills producer lost its open completion")
        };
        assert!(!*accepted, "a fills producer was accepted twice");
        #[cfg(test)]
        assert!(
            !*accepted_by_relation_wave,
            "an independent fills producer carried relation-wave acceptance"
        );
        *accepted = true;
        Ok(())
    }

    pub(super) fn accept_relation_producer_wave(
        &mut self,
        producers: &[u32],
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<bool, crate::error::Error> {
        #[cfg(test)]
        record_fill_relation_work(|work| work.relation_wave_scan_visits += producers.len());
        let mut newly_completed = Vec::new();
        let mut equations = Vec::new();
        let mut obligations = Vec::new();
        for &producer in producers {
            if self.producer_is_complete(producer) {
                continue;
            }
            let StagedOperand::OpenCompleted {
                child,
                preview,
                accepted,
                ..
            } = self.producer_state(producer)
            else {
                continue;
            };
            if *accepted {
                continue;
            }
            newly_completed.push(producer);
            obligations.push(
                frontier
                    .store()
                    .producer_obligations(child.completion_delta(), tcx)?,
            );
            equations.push((
                preview.clone(),
                self.producer_recipe(producer).ty.ty.clone(),
                span,
            ));
        }
        if equations.is_empty() {
            return Ok(false);
        }
        #[cfg(test)]
        record_fill_relation_work(|work| {
            work.batch_attempts += 1;
            work.equations_submitted += equations.len();
        });
        frontier.accept_producer_wave(self.pair_wrapper_deltas(), obligations, equations, tcx)?;
        #[cfg(test)]
        record_fill_relation_work(|work| work.batch_commits += 1);
        for producer in newly_completed {
            let state = self.producer_state_mut(producer);
            let StagedOperand::OpenCompleted {
                accepted,
                #[cfg(test)]
                accepted_by_relation_wave,
                ..
            } = state
            else {
                unreachable!("an accepted fills producer lost its open completion")
            };
            assert!(!*accepted, "a fills producer was accepted twice");
            *accepted = true;
            #[cfg(test)]
            {
                assert!(
                    !*accepted_by_relation_wave,
                    "a fills producer was relation-wave accepted twice"
                );
                *accepted_by_relation_wave = true;
            }
            #[cfg(test)]
            record_fill_relation_work(|work| work.accepted_producers += 1);
        }
        Ok(true)
    }

    pub(super) fn next_ready_producer_close(
        &self,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<Option<FillProducerCloseRequest>, crate::error::Error> {
        let producer = self.next_close_producer;
        if producer >= self.next_producer {
            return Ok(None);
        }
        assert!(
            !self.producer_is_complete(producer),
            "the fills close cursor stopped on an already closed producer"
        );
        let StagedOperand::OpenCompleted {
            accepted,
            #[cfg(test)]
            accepted_by_relation_wave,
            ..
        } = self.producer_state(producer)
        else {
            return Ok(None);
        };
        if !accepted {
            return Ok(None);
        }
        let parent = self.producer_parent(producer);
        let leaf_expected = {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            let closed = store.require_goal_free_after_delta(
                delta,
                self.producer_recipe(producer).ty.ty.clone(),
                tcx,
            )?;
            store.rebase_goal_free_type_to_owner(closed, parent, span, tcx)?
        };
        let location = self
            .producer_locations
            .get(producer as usize)
            .expect("the fills close cursor referenced an unknown producer");
        let root_expected = if location.path.is_empty() {
            Some(leaf_expected.clone())
        } else {
            let retained = self
                .sources
                .get(location.source)
                .expect("a fills close request lost its retained source");
            let StagedOperand::Pair(root) = retained
                .state
                .as_ref()
                .expect("a closed source retained an open producer bit")
            else {
                unreachable!("a nested fill producer path started outside a pair")
            };
            if !staged_pair_ready_to_close(root) {
                None
            } else {
                let (store, _owner, delta, _publication) = frontier.parts_mut();
                let closed = store.require_goal_free_after_delta(
                    delta,
                    retained.header.recipe().ty.ty.clone(),
                    tcx,
                )?;
                Some(store.rebase_goal_free_type_to_owner(closed, parent, span, tcx)?)
            }
        };
        Ok(Some(FillProducerCloseRequest {
            producer,
            parent,
            leaf_expected,
            root_expected,
            #[cfg(test)]
            count_relation_close: *accepted_by_relation_wave,
        }))
    }

    pub(super) fn close_ready_producer(
        &mut self,
        request: FillProducerCloseRequest,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<(), crate::error::Error> {
        #[cfg(test)]
        let count_relation_close = request.count_relation_close;
        assert_eq!(
            request.producer, self.next_close_producer,
            "a fills producer closed outside authored source order"
        );
        assert_eq!(
            request.parent,
            frontier.owner(),
            "a fills producer closed through another retained parent"
        );
        let location = self
            .producer_locations
            .get(request.producer as usize)
            .expect("a fills close request referenced an unknown producer")
            .clone();
        let retained = self
            .sources
            .get_mut(location.source)
            .expect("a fills close request lost its retained source");
        close_projected_source_at_path(
            &mut retained.state,
            &location.path,
            retained.layer,
            &mut self.template_slots,
            request.leaf_expected,
            span,
            frontier,
            tcx,
        )?;
        if !location.path.is_empty() {
            let StagedOperand::Pair(root) = retained
                .state
                .as_ref()
                .expect("a fills close request targeted a completed source")
            else {
                unreachable!("a nested fill producer path started outside a pair")
            };
            if staged_pair_is_closed(root) {
                let state = retained
                    .state
                    .take()
                    .expect("a ready projected pair lost its open wrapper");
                let StagedOperand::Pair(root) = state else {
                    unreachable!("a ready projected pair changed staged shape")
                };
                let source = root.source;
                let completed = finalize_staged_pair(*root, frontier, tcx)?;
                let expected = request
                    .root_expected
                    .expect("a fully completed projected pair lacked its frozen root expectation");
                require_closed_recipe_match(
                    completed.scoped().clone(),
                    expected,
                    span,
                    frontier,
                    tcx,
                )?;
                retained.state = Some(StagedOperand::Completed {
                    source,
                    completed,
                    leaf: None,
                });
            }
        }
        self.producer_completed[request.producer as usize] = true;
        self.next_close_producer = self
            .next_close_producer
            .checked_add(1)
            .expect("fills producer close count exceeded u32");
        #[cfg(test)]
        if count_relation_close {
            record_fill_relation_work(|work| work.physical_closes += 1);
        }
        Ok(())
    }

    fn install_producer_expected(
        &mut self,
        producer: u32,
        parent: crate::ast::TypeGoalOwner,
        expected: crate::pass::typecheck_core::ScopedType,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<(), crate::error::Error> {
        let location = self
            .producer_locations
            .get(producer as usize)
            .expect("a fills request referenced an unknown producer")
            .clone();
        install_projected_source_expected_at_path(
            &mut self
                .sources
                .get_mut(location.source)
                .expect("a fills producer lost its retained source")
                .state,
            &location.path,
            parent,
            expected,
            frontier,
            tcx,
        )
    }

    fn producer_recipe(&self, producer: u32) -> &ProjectedCheckedRecipe {
        let location = self
            .producer_locations
            .get(producer as usize)
            .expect("a fills request referenced an unknown producer");
        projected_recipe_at_path(
            self.sources
                .get(location.source)
                .expect("a fills producer lost its retained source")
                .header
                .recipe(),
            &location.path,
        )
    }

    fn producer_state(&self, producer: u32) -> &StagedOperand<'m> {
        let location = self
            .producer_locations
            .get(producer as usize)
            .expect("a fills request referenced an unknown producer");
        staged_operand_at_path(
            self.sources
                .get(location.source)
                .and_then(|retained| retained.state.as_ref())
                .expect("a fills producer lost its retained source"),
            &location.path,
        )
    }

    fn producer_state_mut(&mut self, producer: u32) -> &mut StagedOperand<'m> {
        let location = self
            .producer_locations
            .get(producer as usize)
            .expect("a fills request referenced an unknown producer")
            .clone();
        staged_operand_at_path_mut(
            self.sources
                .get_mut(location.source)
                .and_then(|retained| retained.state.as_mut())
                .expect("a fills producer lost its retained source"),
            &location.path,
        )
    }

    fn pair_wrapper_deltas(&mut self) -> Vec<&mut super::GoalDelta> {
        fn collect<'a>(
            state: &'a mut StagedOperand<'_>,
            deltas: &mut Vec<&'a mut super::GoalDelta>,
        ) {
            if let StagedOperand::Pair(pair) = state {
                deltas.push(pair.premise.completion_delta_mut());
                for operand in &mut pair.operands {
                    if let Some(state) = &mut operand.state {
                        collect(state, deltas);
                    }
                }
            }
        }
        let mut deltas = Vec::new();
        for source in &mut self.sources {
            if let Some(state) = &mut source.state {
                collect(state, &mut deltas);
            }
        }
        deltas
    }
}

fn staged_operand_parent(operand: &StagedOperand<'_>) -> crate::ast::TypeGoalOwner {
    match operand {
        StagedOperand::Pair(pair) => pair.premise.parent_owner(),
        StagedOperand::Lambda(lambda, _) => lambda.premise.parent_owner(),
        StagedOperand::DeferredRecLambda(deferred, _) => deferred.lambda.premise.parent_owner(),
        StagedOperand::OpenCompleted { child, .. } => child.parent_owner(),
        StagedOperand::Ordinary(continuation, _) => continuation.child.parent_owner(),
        StagedOperand::Completed { .. } => {
            unreachable!("a completed projected leaf retained no producer parent")
        }
    }
}

fn staged_operand_owner(operand: &StagedOperand<'_>) -> crate::ast::TypeGoalOwner {
    match operand {
        StagedOperand::Pair(pair) => pair.premise.owner(),
        StagedOperand::Lambda(lambda, _) => lambda.premise.owner(),
        StagedOperand::DeferredRecLambda(deferred, _) => deferred.lambda.premise.owner(),
        StagedOperand::OpenCompleted { child, .. } => child.owner(),
        StagedOperand::Ordinary(continuation, _) => continuation.child.owner(),
        StagedOperand::Completed { .. } => unreachable!("a closed producer has no live owner"),
    }
}

fn isolate_staged_producer<'m>(
    state: &StagedOperand<'m>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    if matches!(
        state,
        StagedOperand::Completed { .. } | StagedOperand::Pair(_)
    ) {
        return Ok(());
    }
    let boundary = staged_operand_owner(state);
    frontier.isolate_producer(boundary, crate::span::Span::new(0, 0), tcx)
}

fn staged_header_parent(operand: &StagedOperand<'_>) -> Option<crate::ast::TypeGoalOwner> {
    match operand {
        StagedOperand::Pair(pair) => Some(pair.premise.parent_owner()),
        StagedOperand::Lambda(lambda, _) => Some(lambda.premise.parent_owner()),
        StagedOperand::DeferredRecLambda(deferred, _) => {
            Some(deferred.lambda.premise.parent_owner())
        }
        StagedOperand::Ordinary(continuation, _) => Some(continuation.child.parent_owner()),
        StagedOperand::OpenCompleted { child, .. } => Some(child.parent_owner()),
        StagedOperand::Completed { .. } => None,
    }
}

fn staged_operand_at_path<'a, 'm>(
    operand: &'a StagedOperand<'m>,
    path: &[u8],
) -> &'a StagedOperand<'m> {
    if path.is_empty() {
        return operand;
    }
    let (&index, rest) = path
        .split_first()
        .expect("a fills producer path has one pair operand");
    let StagedOperand::Pair(pair) = operand else {
        unreachable!("a fills producer path crossed a non-pair operand")
    };
    let operand = pair.operands[usize::from(index)]
        .state
        .as_ref()
        .expect("a fills producer path lost its staged operand");
    staged_operand_at_path(operand, rest)
}

fn staged_operand_at_path_mut<'a, 'm>(
    operand: &'a mut StagedOperand<'m>,
    path: &[u8],
) -> &'a mut StagedOperand<'m> {
    if path.is_empty() {
        return operand;
    }
    let (&index, rest) = path
        .split_first()
        .expect("a fills producer path has one pair operand");
    let StagedOperand::Pair(pair) = operand else {
        unreachable!("a fills producer path crossed a non-pair operand")
    };
    let operand = pair.operands[usize::from(index)]
        .state
        .as_mut()
        .expect("a fills producer path lost its staged operand");
    staged_operand_at_path_mut(operand, rest)
}

fn projected_recipe_at_path<'a>(
    recipe: &'a ProjectedCheckedRecipe,
    path: &[u8],
) -> &'a ProjectedCheckedRecipe {
    let mut recipe = recipe;
    for &index in path {
        let ProjectedRecipeNode::Pair(left, right) = recipe.node.as_ref() else {
            unreachable!("a fills producer recipe path crossed a non-pair node")
        };
        recipe = match index {
            0 => left,
            1 => right,
            _ => unreachable!("a canonical pair producer path used another operand"),
        };
    }
    recipe
}

fn install_projected_source_expected_at_path<'m>(
    state: &mut Option<StagedOperand<'m>>,
    path: &[u8],
    parent: crate::ast::TypeGoalOwner,
    expected: crate::pass::typecheck_core::ScopedType,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    if path.is_empty() {
        return install_staged_expected(
            state
                .as_mut()
                .expect("a fills producer lost its retained source"),
            parent,
            expected,
            frontier,
            tcx,
        );
    }
    let (&index, rest) = path
        .split_first()
        .expect("a fills producer path has one pair operand");
    let Some(StagedOperand::Pair(pair)) = state.as_mut() else {
        unreachable!("a fills producer path crossed a non-pair source")
    };
    let mut pair_frontier = frontier.ordinary_child_frontier(&mut pair.premise);
    install_projected_source_expected_at_path(
        &mut pair.operands[usize::from(index)].state,
        rest,
        parent,
        expected,
        &mut pair_frontier,
        tcx,
    )
}

fn install_staged_expected<'m>(
    operand: &mut StagedOperand<'m>,
    parent: crate::ast::TypeGoalOwner,
    expected: crate::pass::typecheck_core::ScopedType,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let retained = || super::OwnerBoundType::Retained {
        owner: parent,
        ty: expected.clone(),
    };
    match operand {
        StagedOperand::Lambda(lambda, _) => {
            install_staged_lambda_expected(lambda, retained(), frontier, tcx)
        }
        StagedOperand::DeferredRecLambda(deferred, _) => {
            install_staged_lambda_expected(&mut deferred.lambda, retained(), frontier, tcx)
        }
        StagedOperand::Ordinary(continuation, _) => {
            if let Some(previous) = continuation.cursor.expected.as_ref() {
                let mut child_frontier = frontier.ordinary_child_frontier(&mut continuation.child);
                let (store, owner, delta, _publication) = child_frontier.parts_mut();
                let previous = previous.clone().into_scoped(
                    owner,
                    store,
                    continuation.cursor.source.span(),
                )?;
                let next =
                    retained().into_scoped(owner, store, continuation.cursor.source.span())?;
                store.constrain(
                    delta,
                    previous,
                    next,
                    continuation.cursor.source.span(),
                    tcx,
                )?;
            }
            continuation.cursor.expected = Some(retained());
            Ok(())
        }
        StagedOperand::Completed { .. } | StagedOperand::OpenCompleted { .. } => {
            unreachable!("a completed fills producer received a new expectation")
        }
        StagedOperand::Pair(_) => {
            unreachable!("a fills expectation targeted a pair shell instead of its leaf")
        }
    }
}

fn install_staged_lambda_expected<'m>(
    lambda: &mut StagedLambda<'m>,
    expected: super::OwnerBoundType,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    assert_eq!(
        frontier.owner(),
        lambda.premise.parent_owner(),
        "a projected lambda expectation entered through another parent"
    );
    if matches!(
        lambda.prepared.selection,
        super::OrdinaryLambdaSelection::Check { .. }
    ) {
        if let Some(previous) = lambda.expected.as_ref() {
            let mut child_frontier = frontier.ordinary_child_frontier(&mut lambda.premise);
            let (store, owner, delta, _publication) = child_frontier.parts_mut();
            let previous = previous
                .clone()
                .into_scoped(owner, store, lambda.source.span())?;
            let next = expected
                .clone()
                .into_scoped(owner, store, lambda.source.span())?;
            store.constrain(delta, previous, next, lambda.source.span(), tcx)?;
        }
        lambda.expected = Some(expected);
        return Ok(());
    }
    let crate::ast::Expr::FnExpr { sig, .. } = lambda.source else {
        unreachable!("a staged lambda changed expression family")
    };
    let mut child_frontier = frontier.ordinary_child_frontier(&mut lambda.premise);
    let Some((selection, _)) = super::select_ordinary_lambda(
        lambda.source,
        &mut lambda.header,
        Some(&expected),
        super::frontier::FrontierMode::ExpectedOnly,
        &mut child_frontier,
        tcx,
    )?
    else {
        return Err(crate::error::Error::elaborator(
            lambda.source.span(),
            "a projected lambda expectation did not expose its complete function spine",
        ));
    };
    let super::OrdinaryLambdaSelection::Check { .. } = selection else {
        unreachable!("an expectation-only projected lambda selected synthesis")
    };
    let (body_type, expected_value_param_types) = lambda
        .header
        .bind_selected_alpha_edges(&lambda.retained_binders);
    let mark = tcx.save();
    super::push_ordinary_lambda_scope(
        sig,
        &lambda.prepared.value_param_types,
        &lambda.retained_binders,
        tcx,
    );
    let prepared = {
        let mut lambda_frontier =
            child_frontier.ordinary_child_frontier(lambda.prepared.lambda_owners.innermost_mut());
        super::prepare_ordinary_lambda_check_equations(
            lambda.source,
            sig,
            &mut lambda.header,
            body_type.clone(),
            expected_value_param_types,
            &lambda.prepared.value_param_types,
            &mut lambda_frontier,
            tcx,
        )
    };
    tcx.restore(mark);
    let body_type = prepared?;
    lambda.expected = Some(expected);
    lambda.prepared.body_expected = Some(body_type.clone());
    lambda.prepared.selection = super::OrdinaryLambdaSelection::Check { body_type };
    Ok(())
}

pub(crate) struct ProjectedTemplateSource {
    pub(crate) source: crate::ast::Expr<crate::ast::Lowered>,
    pub(crate) ty: crate::pass::typecheck_core::InternedType<crate::ast::Lowered>,
    pub(crate) layer: Option<usize>,
}

pub(super) struct ProjectedMaterialization {
    pub(super) typed: crate::pass::typecheck_full::TypedUserElaboratorCall,
    pub(super) checked: crate::normalization::EvalRef<crate::normalization::CheckedTerm>,
    pub(super) quoted_public_recipe: Option<Box<crate::pass::typecheck_full::QuotedPublicRecipe>>,
    pub(super) public_terminal_result:
        crate::pass::typecheck_core::InternedType<crate::ast::Lowered>,
    pub(super) checked_result: crate::pass::typecheck_core::InternedType<crate::ast::Lowered>,
    pub(super) sources: Vec<ProjectedTemplateSource>,
    pub(super) public_residual_result: crate::pass::typecheck_core::ScopedType,
}

fn collect_projected_sources<'m>(
    pair: &StagedPair<'m>,
    layer: usize,
    slots: &mut [ProjectedTemplateSlot],
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    for operand in &pair.operands {
        collect_projected_operand_source(
            operand
                .state
                .as_ref()
                .expect("a retained pair operand lost its completed state"),
            layer,
            slots,
            frontier,
            tcx,
        )?;
    }
    Ok(())
}

fn capture_completed_pair_sources<'m>(
    pair: &mut StagedPair<'m>,
    layer: usize,
    slots: &mut [ProjectedTemplateSlot],
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let mut pair_frontier = frontier.ordinary_child_frontier(&mut pair.premise);
    for operand in &mut pair.operands {
        let state = operand
            .state
            .as_mut()
            .expect("a retained pair operand lost its staged state");
        match state {
            StagedOperand::Pair(pair) => {
                capture_completed_pair_sources(pair, layer, slots, &mut pair_frontier, tcx)?
            }
            StagedOperand::Completed {
                source,
                completed,
                leaf,
            } => {
                let leaf = leaf
                    .as_ref()
                    .expect("a completed projected operand lost its template identity");
                let closed = {
                    let (store, _owner, delta, _publication) = pair_frontier.parts_mut();
                    store.require_goal_free_after_delta(delta, completed.scoped().clone(), tcx)?
                };
                let slot = slots
                    .get_mut(leaf.template_index as usize)
                    .expect("a captured template identity exceeded its dense table");
                assert!(
                    slot.source
                        .replace(ProjectedTemplateSource {
                            source: (**source).clone(),
                            ty: closed.into_interned_type(),
                            layer: Some(layer),
                        })
                        .is_none(),
                    "a projected template source was captured more than once"
                );
            }
            StagedOperand::Lambda(..)
            | StagedOperand::DeferredRecLambda(..)
            | StagedOperand::Ordinary(..)
            | StagedOperand::OpenCompleted { .. } => {}
        }
    }
    Ok(())
}

fn collect_projected_operand_source<'m>(
    operand: &StagedOperand<'m>,
    layer: usize,
    slots: &mut [ProjectedTemplateSlot],
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    match operand {
        StagedOperand::Pair(pair) => collect_projected_sources(pair, layer, slots, frontier, tcx),
        StagedOperand::Completed {
            source,
            completed,
            leaf,
        } => {
            let Some(leaf) = leaf.as_ref() else {
                // Exact-parent prefix closing snapshots the source before it
                // consumes the live retained premise.  Materialization sees
                // only this inert completion marker and the dense side table.
                return Ok(());
            };
            let closed = {
                let (store, _owner, delta, _publication) = frontier.parts_mut();
                store.require_goal_free_after_delta(delta, completed.scoped().clone(), tcx)?
            };
            let slot = slots
                .get_mut(leaf.template_index as usize)
                .expect("a projected template identity exceeded its dense source table");
            assert!(
                slot.source
                    .replace(ProjectedTemplateSource {
                        source: (*source).clone(),
                        ty: closed.into_interned_type(),
                        layer: Some(layer),
                    })
                    .is_none(),
                "two projected leaves shared one template identity"
            );
            Ok(())
        }
        StagedOperand::OpenCompleted {
            source,
            preview,
            leaf,
            ..
        } => {
            let leaf = leaf
                .as_ref()
                .expect("an open completed projected operand lost its template identity");
            let slot = slots
                .get_mut(leaf.template_index as usize)
                .expect("a projected template identity exceeded its dense source table");
            assert!(
                slot.source
                    .replace(ProjectedTemplateSource {
                        source: (*source).clone(),
                        ty: preview.ty().clone(),
                        layer: Some(layer),
                    })
                    .is_none(),
                "two projected leaves shared one template identity"
            );
            Ok(())
        }
        StagedOperand::Lambda(_, _)
        | StagedOperand::DeferredRecLambda(_, _)
        | StagedOperand::Ordinary(_, _) => {
            unreachable!("fill adoption completed while a projected producer remained pending")
        }
    }
}

fn finalize_staged_pair<'m>(
    pair: StagedPair<'m>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<super::CompletedInFrame, crate::error::Error> {
    let StagedPair {
        mut premise,
        source,
        expected,
        selecting,
        operands,
    } = pair;
    let destination = frontier.owner();
    let completed = {
        let mut pair_frontier = frontier.ordinary_child_frontier(&mut premise);
        let mut selecting = *selecting;
        assert!(
            selecting.selected.is_empty()
                && selecting.probe.is_none()
                && selecting.next_candidate == operands.len()
                && selecting.selected_slots == selecting.value_slots,
            "a retained canonical pair wrapper changed its sealed call plan"
        );
        if let Some(direct) = selecting.direct.take() {
            let (store, owner, publication) = pair_frontier.publication_mut();
            direct.stage_type(owner, store, publication, tcx)?;
        }
        if let Some((node_id, replacement)) = selecting.regular_ufcs.take() {
            let (store, _owner, publication) = pair_frontier.publication_mut();
            publication.record_lowered_elaboration(store, node_id, replacement);
        }
        for operand in operands {
            let state = operand
                .state
                .expect("a retained pair operand lost its completed state");
            let (operand_source, completed) =
                finalize_staged_operand(state, &mut pair_frontier, tcx)?;
            selecting.selected.push(super::SelectedOrdinaryCallValue {
                value: super::OrdinaryCallValue::Completed {
                    source: operand_source,
                    output: completed,
                },
                expected: operand.expected,
                immediate_evidence: None,
                expanded_start: operand.expanded_start,
                expanded_width: operand.expanded_width,
            });
        }
        assert!(
            selecting.direct.is_none()
                && selecting.regular_ufcs.is_none()
                && selecting.candidates.len() == selecting.next_candidate,
            "a retained canonical pair wrapper acquired an unsealed continuation"
        );
        let mut advancing = super::begin_general_goal_call(
            &mut pair_frontier,
            selecting.call,
            selecting.selected,
            selecting.result_inferred_type_arg_suffix,
            expected,
            tcx,
        )?;
        assert!(
            advancing.pending.is_empty() && advancing.call.immediate_lambda.is_none(),
            "a canonical pair wrapper retained an opaque call child"
        );
        if advancing.pending_expected_result.is_some() {
            assert!(
                super::try_apply_pending_expected_result(&mut pair_frontier, &mut advancing, tcx)?,
                "a completed canonical pair wrapper left its expected equation deferred"
            );
        }
        let result = {
            let (store, _owner, delta, _publication) = pair_frontier.parts_mut();
            store.require_goal_free_after_delta(delta, advancing.result, tcx)?
        };
        {
            let (_store, _owner, publication) = pair_frontier.publication_mut();
            publication.record_position_type(
                source.span(),
                result.clone(),
                tcx.in_scope_type_param_binders(),
            );
        }
        super::CompletedInFrame::Value(result, advancing.result_inferred_type_arg_suffix)
    };
    let completed =
        super::close_completed_ordinary_child(frontier, premise, completed, destination, tcx)?;
    #[cfg(test)]
    PAIR_WRAPPER_FINALIZATIONS.with(|count| count.set(count.get() + 1));
    Ok(completed)
}

#[cfg(test)]
thread_local! {
    static PAIR_WRAPPER_FINALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_pair_wrapper_finalizations() {
    PAIR_WRAPPER_FINALIZATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn pair_wrapper_finalizations() -> usize {
    PAIR_WRAPPER_FINALIZATIONS.with(std::cell::Cell::get)
}

fn finalize_staged_operand<'m>(
    operand: StagedOperand<'m>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<
    (
        &'m crate::ast::Expr<crate::ast::Lowered>,
        super::CompletedInFrame,
    ),
    crate::error::Error,
> {
    match operand {
        StagedOperand::Pair(pair) => {
            let source = pair.source;
            finalize_staged_pair(*pair, frontier, tcx).map(|completed| (source, completed))
        }
        StagedOperand::Completed {
            source, completed, ..
        } => Ok((source, completed)),
        StagedOperand::OpenCompleted {
            source,
            child,
            completed,
            ..
        } => {
            let destination = frontier.owner();
            super::close_completed_ordinary_child(frontier, child, completed, destination, tcx)
                .map(|completed| (source, completed))
        }
        StagedOperand::Lambda(_, _)
        | StagedOperand::DeferredRecLambda(_, _)
        | StagedOperand::Ordinary(_, _) => {
            unreachable!("a canonical pair wrapper finalized with a pending operand")
        }
    }
}

struct ProjectedRecipeCloser<'a, 'f, 'm, 'e> {
    span: crate::span::Span,
    binders: Vec<crate::ast::TypeParam>,
    scratch: crate::pass::typecheck_core::ProjectedTypeCloseScratch,
    frontier: &'a mut super::frontier::ConnectedCallFrontier<'f, 'm>,
    tcx: &'a super::TypeCtx<'m, 'e, crate::ast::Lowered>,
}

impl<'a, 'f, 'm, 'e> ProjectedRecipeCloser<'a, 'f, 'm, 'e> {
    fn close_type(
        &mut self,
        ty: crate::pass::typecheck_core::ScopedType,
    ) -> Result<crate::pass::typecheck_core::ClosedProjectedType, crate::error::Error> {
        #[cfg(test)]
        PROJECTED_RECIPE_CLOSE_WORK.with(|work| {
            let (closes, reflections) = work.get();
            work.set((closes + 1, reflections));
        });
        let Self {
            binders,
            scratch,
            frontier,
            tcx,
            ..
        } = self;
        let (store, _owner, delta, _publication) = frontier.parts_mut();
        store.close_projected_type_at_lexical_path(delta, ty, binders, scratch, *tcx)
    }

    fn relation(
        &mut self,
        actual: &crate::pass::typecheck_core::ClosedProjectedType,
        expected: &crate::pass::typecheck_core::ClosedProjectedType,
        diagnostic: &'static str,
    ) -> Result<(), crate::error::Error> {
        let Self {
            span,
            frontier,
            tcx,
            ..
        } = self;
        let (store, _owner, delta, _publication) = frontier.parts_mut();
        store.validate_closed_projected_type_relation(
            delta, actual, expected, *span, diagnostic, *tcx,
        )
    }

    fn instantiate_forall(
        &mut self,
        scheme: &crate::pass::typecheck_core::ClosedProjectedType,
        argument: &crate::pass::typecheck_core::ClosedProjectedType,
    ) -> Result<Option<crate::pass::typecheck_core::ClosedProjectedType>, crate::error::Error> {
        let Self { frontier, tcx, .. } = self;
        let (store, _owner, delta, _publication) = frontier.parts_mut();
        store.instantiate_closed_projected_forall(delta, scheme, argument, *tcx)
    }

    fn reflect(
        &self,
        ty: &crate::pass::typecheck_core::ClosedProjectedType,
    ) -> Result<crate::ast::Type<crate::ast::UncheckedPrime>, crate::error::Error> {
        #[cfg(test)]
        PROJECTED_RECIPE_CLOSE_WORK.with(|work| {
            let (closes, reflections) = work.get();
            work.set((closes, reflections + 1));
        });
        crate::pass::typecheck_full::reflect_projected_type(ty.ty(), self.tcx)
            .map(|reflected| reflected.as_type().clone())
    }

    fn child(
        &self,
        ty: &crate::pass::typecheck_core::ClosedProjectedType,
        edge: crate::pass::typecheck_core::ScopedTypeEdge,
        diagnostic: &'static str,
    ) -> Result<crate::pass::typecheck_core::ClosedProjectedType, crate::error::Error> {
        ty.structural_child(edge)
            .ok_or_else(|| crate::error::Error::elaborator(self.span, diagnostic))
    }

    fn close_expected(
        &mut self,
        recipe: &ProjectedCheckedRecipe,
        expected: &crate::pass::typecheck_core::ClosedProjectedType,
        diagnostic: &'static str,
    ) -> Result<crate::normalization::EvalRef<crate::normalization::CheckedTerm>, crate::error::Error>
    {
        let (term, actual) = self.close(recipe)?;
        self.relation(&actual, expected, diagnostic)?;
        Ok(term)
    }

    fn construct(
        &self,
        node: crate::normalization::ProjectedTermConstruction,
        ty: crate::ast::Type<crate::ast::UncheckedPrime>,
    ) -> crate::normalization::EvalRef<crate::normalization::CheckedTerm> {
        crate::normalization::EvalRef::new(crate::normalization::CheckedTerm::from_projected(
            node, ty,
        ))
    }
}

#[cfg(test)]
thread_local! {
    static PROJECTED_RECIPE_CLOSE_WORK: std::cell::Cell<(usize, usize)> = const {
        std::cell::Cell::new((0, 0))
    };
}

fn split_closed_function_params(
    ty: crate::ast::Type<crate::ast::UncheckedPrime>,
    span: crate::span::Span,
    helper: &'static str,
) -> Result<Vec<crate::ast::Type<crate::ast::UncheckedPrime>>, crate::error::Error> {
    let crate::ast::Type::Function {
        param, abi_arity, ..
    } = ty
    else {
        return Err(crate::error::Error::elaborator(
            span,
            format!("{helper} closed with a non-function type"),
        ));
    };
    let mut params = Vec::with_capacity(abi_arity);
    let mut cursor = *param;
    for index in 0..abi_arity {
        if index + 1 == abi_arity {
            params.push(cursor);
            break;
        }
        let crate::ast::Type::Product { left, right, .. } = cursor else {
            return Err(crate::error::Error::elaborator(
                span,
                format!("{helper} ABI arity exceeded its parameter product spine"),
            ));
        };
        params.push(*left);
        cursor = *right;
    }
    Ok(params)
}

fn closed_projected_value_signature(
    names: &[String],
    params: Vec<crate::ast::Type<crate::ast::UncheckedPrime>>,
    span: crate::span::Span,
) -> Result<crate::ast::Signature<crate::ast::UncheckedPrime>, crate::error::Error> {
    if names.len() != params.len() {
        return Err(crate::error::Error::elaborator(
            span,
            "__term_fn__ closed with the wrong number of value binders",
        ));
    }
    let len = params.len();
    Ok(crate::ast::Signature::from_parts(
        names
            .iter()
            .cloned()
            .zip(params)
            .map(|(name, ty)| {
                crate::ast::SignatureParam::Value(crate::ast::Param {
                    name,
                    ty: Some(ty),
                    pattern: Default::default(),
                    meta: crate::ast::Meta::new(span),
                })
            })
            .collect(),
        vec![crate::ast::SignatureGroupKind::Value { len }],
    ))
}

fn close_projected_recipe<'m>(
    recipe: &ProjectedCheckedRecipe,
    binders: Vec<crate::ast::TypeParam>,
    span: crate::span::Span,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<crate::normalization::EvalRef<crate::normalization::CheckedTerm>, crate::error::Error> {
    ProjectedRecipeCloser {
        span,
        binders,
        scratch: crate::pass::typecheck_core::ProjectedTypeCloseScratch::default(),
        frontier,
        tcx,
    }
    .close(recipe)
    .map(|(term, _closed)| term)
}

type LiftedCallbackRuntimeTypes = (
    Vec<Option<crate::ast::Type<crate::ast::UncheckedPrime>>>,
    crate::pass::typecheck_core::InternedType<crate::ast::Lowered>,
    ProjectedTemplateSource,
);

fn prepare_lifted_callback_types<'m>(
    sources: &mut [ProjectedTemplateSource],
    returns: &[Option<crate::ast::Type<crate::ast::Lowered>>],
    public_terminal_result: &crate::pass::typecheck_core::InternedType<crate::ast::Lowered>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<LiftedCallbackRuntimeTypes, crate::error::Error> {
    fn replace_terminal(
        ty: crate::ast::Type<crate::ast::Lowered>,
        layers: &[bool],
        replacement: crate::ast::Type<crate::ast::Lowered>,
        span: crate::span::Span,
    ) -> Result<
        (
            crate::ast::Type<crate::ast::Lowered>,
            crate::ast::Type<crate::ast::Lowered>,
        ),
        crate::error::Error,
    > {
        let Some((&is_type, rest)) = layers.split_first() else {
            return Ok((replacement, ty));
        };
        match (is_type, ty) {
            (true, crate::ast::Type::Forall { param, body, meta }) => {
                let (body, terminal) = replace_terminal(*body, rest, replacement, span)?;
                Ok((
                    crate::ast::Type::Forall {
                        param,
                        body: Box::new(body),
                        meta,
                    },
                    terminal,
                ))
            }
            (
                false,
                crate::ast::Type::Function {
                    param,
                    ret,
                    meta,
                    abi_arity,
                    caps,
                },
            ) => {
                let (ret, terminal) = replace_terminal(*ret, rest, replacement, span)?;
                Ok((
                    crate::ast::Type::Function {
                        param,
                        ret: Box::new(ret),
                        meta,
                        abi_arity,
                        caps,
                    },
                    terminal,
                ))
            }
            _ => Err(crate::error::Error::elaborator(
                span,
                "a lifted callback lost its checked signature shape",
            )),
        }
    }

    let mut reflected = vec![None; sources.len()];
    let binders = tcx.in_scope_type_param_binders();
    let alias_ctx = tcx.binder_alias_ctx(&binders);
    let mut result = None;
    let mut shared_continuation: Option<(usize, ProjectedTemplateSource)> = None;
    for (index, (source, runtime_return)) in sources.iter_mut().zip(returns).enumerate() {
        let Some(runtime_return) = runtime_return else {
            continue;
        };
        let crate::ast::Expr::FnExpr { sig, body, .. } = &source.source else {
            unreachable!("a lifted projected source changed expression family")
        };
        let crate::ast::Expr::RecOrder { plan, .. } = body.as_ref() else {
            unreachable!("a lifted projected source lost its root recursive carrier")
        };
        let (binding_index, continuation) = super::deferred_tail_continuation_binding(plan, tcx);
        let continuation = {
            let (store, owner, delta, _publication) = frontier.parts_mut();
            let continuation = store.scoped_type(owner, continuation, source.source.span())?;
            store
                .require_goal_free_after_delta(delta, continuation, tcx)?
                .into_interned_type()
        };
        if let Some((previous, _)) = &shared_continuation {
            assert_eq!(
                *previous, binding_index,
                "lifted callbacks of one marked call share their exact outer continuation binding"
            );
        } else {
            shared_continuation = Some((
                binding_index,
                ProjectedTemplateSource {
                    source: plan
                        .tail_continuation
                        .as_deref()
                        .expect("validated continuation reference")
                        .clone(),
                    ty: continuation.clone(),
                    layer: None,
                },
            ));
        }
        let (continuation, continuation_canonical) =
            crate::pass::typecheck_core::canonicalize_for_comparison(
                continuation.as_type(),
                &alias_ctx,
                continuation.identity_is_canonical(),
            );
        let crate::ast::Type::Function {
            param: continuation_input,
            ret: continuation_output,
            abi_arity: 1,
            ..
        } = continuation
        else {
            unreachable!("a deferred callback continuation is a unary ordinary function")
        };
        let mut layers = sig
            .canonical_group_refs()
            .flat_map(|group| match group {
                crate::ast::SignatureGroupRef::Type(params) => vec![true; params.len()],
                crate::ast::SignatureGroupRef::Value(_) => vec![false],
            })
            .collect::<Vec<_>>();
        if !sig.has_value_group() {
            layers.push(false);
        }
        if let crate::ast::Expr::FnExpr {
            ret_ty: Some(ret_ty),
            ..
        } = &mut source.source
        {
            *ret_ty = runtime_return.clone();
        }
        // Recursive lowering records its runtime return before typechecking,
        // so that return still uses the caller's lexical type spellings. The
        // checked elaborator term, by contrast, carries identity-canonical
        // types. Canonicalize each side independently before splicing them;
        // otherwise the mixed function type is incorrectly marked canonical
        // and replay treats a qualified-import alias such as `list.List` as
        // the literal module identity `list.List`.
        let (source_ty, source_canonical) =
            crate::pass::typecheck_core::canonicalize_for_comparison(
                source.ty.as_type(),
                &alias_ctx,
                source.ty.identity_is_canonical(),
            );
        let (runtime_return, runtime_return_canonical) =
            crate::pass::typecheck_core::canonicalize_for_comparison(
                runtime_return,
                &alias_ctx,
                false,
            );
        let (runtime_ty, public_return) = replace_terminal(
            source_ty,
            &layers,
            runtime_return.clone(),
            source.source.span(),
        )?;
        let crate::ast::Type::Sum { .. } = &runtime_return else {
            unreachable!("recursive lowering produced a non-sum callback runtime")
        };
        crate::pass::typecheck_core::require_type_equiv_state(
            &continuation_input,
            &public_return,
            source.source.span(),
            &alias_ctx,
            continuation_canonical,
            source_canonical,
        )
        .map_err(|_| {
            crate::error::Error::elaborator(
                source.source.span(),
                "a lifted callback lost its public continuation input",
            )
        })?;
        crate::pass::typecheck_core::require_type_equiv_state(
            &continuation_output,
            &runtime_return,
            source.source.span(),
            &alias_ctx,
            continuation_canonical,
            runtime_return_canonical,
        )?;
        crate::pass::typecheck_core::require_type_equiv_state(
            &continuation_input,
            public_terminal_result.as_type(),
            source.source.span(),
            &alias_ctx,
            continuation_canonical,
            public_terminal_result.identity_is_canonical(),
        )?;
        if let Some((previous, previous_canonical)) = &result {
            crate::pass::typecheck_core::require_type_equiv_state(
                previous,
                &runtime_return,
                source.source.span(),
                &alias_ctx,
                *previous_canonical,
                runtime_return_canonical,
            )
            .map_err(|_| {
                crate::error::Error::elaborator(
                    source.source.span(),
                    "lifted callbacks produced different recursive runtime results",
                )
            })?;
        } else {
            result = Some((runtime_return, runtime_return_canonical));
        }
        let runtime_ty_canonical = source_canonical && runtime_return_canonical;
        source.ty = crate::pass::typecheck_core::InternedType::fresh_with_identity(
            runtime_ty,
            runtime_ty_canonical,
        );
        reflected[index] = Some(
            crate::pass::typecheck_full::reflect_projected_type(&source.ty, tcx)?
                .as_type()
                .clone(),
        );
    }
    let (result, canonical) = result.expect("at least one lifted runtime return");
    let result = crate::pass::typecheck_core::InternedType::fresh_with_identity(result, canonical);
    Ok((
        reflected,
        result,
        shared_continuation
            .expect("at least one lifted continuation")
            .1,
    ))
}

impl<'a, 'f, 'm, 'e> ProjectedRecipeCloser<'a, 'f, 'm, 'e> {
    fn close(
        &mut self,
        recipe: &ProjectedCheckedRecipe,
    ) -> Result<
        (
            crate::normalization::EvalRef<crate::normalization::CheckedTerm>,
            crate::pass::typecheck_core::ClosedProjectedType,
        ),
        crate::error::Error,
    > {
        let closed = self.close_type(recipe.ty.ty.clone())?;
        let reflected = self.reflect(&closed)?;
        let span = self.span;
        match recipe.node.as_ref() {
            ProjectedRecipeNode::Closed(checked) => return Ok((checked.clone(), closed)),
            ProjectedRecipeNode::TemplateValue(index) => {
                return Ok((
                    crate::normalization::EvalRef::new(
                        crate::normalization::CheckedTerm::template_value(
                            *index as usize,
                            reflected,
                        ),
                    ),
                    closed,
                ));
            }
            ProjectedRecipeNode::Local { name } => {
                return Ok((
                    crate::normalization::EvalRef::new(crate::normalization::CheckedTerm::local(
                        name.clone(),
                        reflected,
                    )),
                    closed,
                ));
            }
            _ => {}
        }
        let construction = match recipe.node.as_ref() {
            ProjectedRecipeNode::Closed(_)
            | ProjectedRecipeNode::TemplateValue(_)
            | ProjectedRecipeNode::Local { .. } => {
                unreachable!("projected leaf recipe escaped its direct close path")
            }
            ProjectedRecipeNode::Pair(left, right) => {
                let expected_left = self.child(
                    &closed,
                    crate::pass::typecheck_core::ScopedTypeEdge::ProductLeft,
                    "a projected pair closed with a non-product result type",
                )?;
                let expected_right = self.child(
                    &closed,
                    crate::pass::typecheck_core::ScopedTypeEdge::ProductRight,
                    "a projected pair closed with a non-product result type",
                )?;
                let left = self.close_expected(
                    left,
                    &expected_left,
                    "a projected pair left value closed at another type",
                )?;
                let right = self.close_expected(
                    right,
                    &expected_right,
                    "a projected pair right value closed at another type",
                )?;
                crate::normalization::ProjectedTermConstruction::Pair { left, right }
            }
            ProjectedRecipeNode::ProductProjection {
                product_type,
                product,
                left,
            } => {
                let product_type_closed = self.close_type(product_type.ty.clone())?;
                let product = self.close_expected(
                    product,
                    &product_type_closed,
                    "projected product term did not have the supplied product type",
                )?;
                let selected = self.child(
                    &product_type_closed,
                    if *left {
                        crate::pass::typecheck_core::ScopedTypeEdge::ProductLeft
                    } else {
                        crate::pass::typecheck_core::ScopedTypeEdge::ProductRight
                    },
                    "a projected product type lost its selected child",
                )?;
                self.relation(
                    &closed,
                    &selected,
                    "projected product projection closed at another result type",
                )?;
                let product_type = self.reflect(&product_type_closed)?;
                let crate::ast::Type::Product {
                    left: left_ty,
                    right: right_ty,
                    ..
                } = product_type
                else {
                    return Err(crate::error::Error::elaborator(
                        span,
                        "projected product projection closed with a non-product type",
                    ));
                };
                crate::normalization::ProjectedTermConstruction::ProductProjection {
                    left_ty: *left_ty,
                    right_ty: *right_ty,
                    value: product,
                    left: *left,
                }
            }
            ProjectedRecipeNode::TermCall {
                function_type,
                function,
                argument,
            } => {
                let function_type_closed = self.close_type(function_type.ty.clone())?;
                let function = self.close_expected(
                    function,
                    &function_type_closed,
                    "__term_call__ function term did not have the supplied function type",
                )?;
                let parameter_closed = self.child(
                    &function_type_closed,
                    crate::pass::typecheck_core::ScopedTypeEdge::FunctionParam,
                    "a projected function type lost its parameter",
                )?;
                let argument = self.close_expected(
                    argument,
                    &parameter_closed,
                    "__term_call__ argument packet did not have the function parameter type",
                )?;
                let return_type = self.child(
                    &function_type_closed,
                    crate::pass::typecheck_core::ScopedTypeEdge::FunctionReturn,
                    "a projected function type lost its return",
                )?;
                self.relation(
                    &closed,
                    &return_type,
                    "__term_call__ result did not have the function return type",
                )?;
                let params = split_closed_function_params(
                    self.reflect(&function_type_closed)?,
                    span,
                    "__term_call__",
                )?;
                crate::normalization::ProjectedTermConstruction::Call {
                    fn_value: function,
                    params,
                    arg_packet: argument,
                }
            }
            ProjectedRecipeNode::IntrinsicAbsurd { bottom } => {
                let (bottom, bottom_closed) = self.close(bottom)?;
                if !matches!(
                    bottom_closed.ty().as_type(),
                    crate::ast::Type::Bottom { .. }
                ) {
                    return Err(crate::error::Error::elaborator(
                        span,
                        "__intrinsic_absurd__ expected a checked Bottom term",
                    ));
                }
                crate::normalization::ProjectedTermConstruction::Absurd {
                    bottom_value: bottom,
                }
            }
            ProjectedRecipeNode::IntrinsicEither {
                sum_type,
                value,
                left_name,
                left_body,
                right_name,
                right_body,
            } => {
                let sum_type_closed = self.close_type(sum_type.ty.clone())?;
                let value = self.close_expected(
                    value,
                    &sum_type_closed,
                    "__intrinsic_either__ scrutinee did not have the supplied sum type",
                )?;
                let left_body = self.close_expected(
                    left_body,
                    &closed,
                    "__intrinsic_either__ branch did not have the supplied result type",
                )?;
                let right_body = self.close_expected(
                    right_body,
                    &closed,
                    "__intrinsic_either__ branch did not have the supplied result type",
                )?;
                let sum_type = self.reflect(&sum_type_closed)?;
                let crate::ast::Type::Sum {
                    left: left_ty,
                    right: right_ty,
                    ..
                } = sum_type
                else {
                    return Err(crate::error::Error::elaborator(
                        span,
                        "__intrinsic_either__ closed with a non-sum type",
                    ));
                };
                crate::normalization::ProjectedTermConstruction::Either {
                    left_ty: *left_ty,
                    right_ty: *right_ty,
                    value,
                    left_name: left_name.clone(),
                    left_body,
                    right_name: right_name.clone(),
                    right_body,
                }
            }
            ProjectedRecipeNode::TermLet {
                value_type,
                name,
                value,
                body,
            } => {
                let value_type = self.close_type(value_type.ty.clone())?;
                let value = self.close_expected(
                    value,
                    &value_type,
                    "__term_let__ value did not have the supplied value type",
                )?;
                let body = self.close_expected(
                    body,
                    &closed,
                    "__term_let__ body closed at another result type",
                )?;
                crate::normalization::ProjectedTermConstruction::Let {
                    name: name.clone(),
                    value,
                    body,
                }
            }
            ProjectedRecipeNode::TermFn {
                function_type,
                names,
                body,
            } => {
                let function_type_closed = self.close_type(function_type.ty.clone())?;
                self.relation(
                    &closed,
                    &function_type_closed,
                    "__term_fn__ result did not have the supplied function type",
                )?;
                let expected_body = self.child(
                    &function_type_closed,
                    crate::pass::typecheck_core::ScopedTypeEdge::FunctionReturn,
                    "a projected function type lost its return",
                )?;
                let body = self.close_expected(
                    body,
                    &expected_body,
                    "__term_fn__ body had the wrong return type",
                )?;
                let params = split_closed_function_params(
                    self.reflect(&function_type_closed)?,
                    span,
                    "__term_fn__",
                )?;
                let sig = closed_projected_value_signature(names, params, span)?;
                crate::normalization::ProjectedTermConstruction::Fn { sig, body }
            }
            ProjectedRecipeNode::TermTypeFn { param, body } => {
                let crate::ast::Type::Forall {
                    param: closed_param,
                    ..
                } = closed.ty().as_type()
                else {
                    return Err(crate::error::Error::elaborator(
                        span,
                        "a projected type function closed with a non-forall type",
                    ));
                };
                if closed_param.kind != param.kind {
                    return Err(crate::error::Error::elaborator(
                        span,
                        "a projected type function binder had the wrong kind",
                    ));
                }
                let expected_body = self.child(
                    &closed,
                    crate::pass::typecheck_core::ScopedTypeEdge::ForallBody,
                    "a projected type function lost its body type",
                )?;
                self.binders.push(param.clone());
                let body_result = self.close_expected(
                    body,
                    &expected_body,
                    "__term_type_fn__ body closed at another result type",
                );
                self.binders.pop();
                let body = body_result?;
                crate::normalization::ProjectedTermConstruction::TypeFn {
                    param: param.clone(),
                    body,
                }
            }
            ProjectedRecipeNode::TermTypeApp { term, argument } => {
                let (term, term_closed) = self.close(term)?;
                let argument_closed = self.close_type(argument.clone())?;
                let expected_result = self
                    .instantiate_forall(&term_closed, &argument_closed)?
                    .ok_or_else(|| {
                        crate::error::Error::elaborator(
                            span,
                            "__term_type_app__ closed with an inapplicable type argument",
                        )
                    })?;
                self.relation(
                    &closed,
                    &expected_result,
                    "__term_type_app__ closed at another result type",
                )?;
                let arg = self.reflect(&argument_closed)?;
                crate::normalization::ProjectedTermConstruction::TypeApp { term, arg }
            }
            ProjectedRecipeNode::IntrinsicInjection {
                sum_type,
                value,
                left,
            } => {
                let sum_type_closed = self.close_type(sum_type.ty.clone())?;
                self.relation(
                    &closed,
                    &sum_type_closed,
                    "projected injection closed at another sum type",
                )?;
                let payload = self.child(
                    &sum_type_closed,
                    if *left {
                        crate::pass::typecheck_core::ScopedTypeEdge::SumLeft
                    } else {
                        crate::pass::typecheck_core::ScopedTypeEdge::SumRight
                    },
                    "a projected sum type lost its selected payload",
                )?;
                let value = self.close_expected(
                    value,
                    &payload,
                    "projected injection payload had the wrong type",
                )?;
                let sum_type = self.reflect(&sum_type_closed)?;
                let crate::ast::Type::Sum {
                    left: left_ty,
                    right: right_ty,
                    ..
                } = sum_type
                else {
                    return Err(crate::error::Error::elaborator(
                        span,
                        "projected injection closed with a non-sum type",
                    ));
                };
                crate::normalization::ProjectedTermConstruction::Injection {
                    left_ty: *left_ty,
                    right_ty: *right_ty,
                    value,
                    left: *left,
                }
            }
            ProjectedRecipeNode::IntrinsicIfThenElse {
                condition,
                true_body,
                false_body,
            } => {
                let (condition, condition_closed) = self.close(condition)?;
                let bool_type = {
                    let ty = crate::pass::typecheck_full::bool_role_type(span, self.tcx)?;
                    let (store, owner, _delta, _publication) = self.frontier.parts_mut();
                    store.scoped_type(
                        owner,
                        crate::pass::typecheck_core::InternedType::fresh(ty),
                        span,
                    )?
                };
                let bool_type = self.close_type(bool_type)?;
                self.relation(
                    &condition_closed,
                    &bool_type,
                    "__intrinsic_if_then_else__ condition did not have the unique bool-role type",
                )?;
                let true_body = self.close_expected(
                    true_body,
                    &closed,
                    "__intrinsic_if_then_else__ branch had the wrong result type",
                )?;
                let false_body = self.close_expected(
                    false_body,
                    &closed,
                    "__intrinsic_if_then_else__ branch had the wrong result type",
                )?;
                crate::normalization::ProjectedTermConstruction::IfThenElse {
                    condition,
                    true_body,
                    false_body,
                }
            }
        };
        Ok((self.construct(construction, reflected), closed))
    }
}

fn staged_pair_ready_to_close(pair: &StagedPair<'_>) -> bool {
    pair.operands.iter().all(|operand| {
        let state = operand
            .state
            .as_ref()
            .expect("a retained pair operand lost its staged state");
        match state {
            StagedOperand::Pair(pair) => staged_pair_ready_to_close(pair),
            StagedOperand::Completed { .. } | StagedOperand::OpenCompleted { .. } => true,
            StagedOperand::Lambda(..)
            | StagedOperand::DeferredRecLambda(..)
            | StagedOperand::Ordinary(..) => false,
        }
    })
}

fn staged_pair_is_initially_completed(pair: &StagedPair<'_>) -> bool {
    pair.operands.iter().all(|operand| {
        let state = operand
            .state
            .as_ref()
            .expect("a retained pair operand lost its staged state");
        match state {
            StagedOperand::Pair(pair) => staged_pair_is_initially_completed(pair),
            StagedOperand::Completed { .. } => true,
            StagedOperand::Lambda(..)
            | StagedOperand::DeferredRecLambda(..)
            | StagedOperand::OpenCompleted { .. }
            | StagedOperand::Ordinary(..) => false,
        }
    })
}

fn staged_pair_is_closed(pair: &StagedPair<'_>) -> bool {
    pair.operands.iter().all(|operand| {
        matches!(
            operand
                .state
                .as_ref()
                .expect("a retained pair operand lost its staged state"),
            StagedOperand::Completed { .. }
        )
    })
}

/// Close producer-free nested canonical pairs while their exact retained
/// parent is still current. Their literal/leaf children are already ordinary
/// completions; keeping the wrapper open until the later marked action would
/// lose the parent needed for publication and recursive-order replay.
fn collapse_initial_completed_pair_children<'m>(
    pair: &mut StagedPair<'m>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let mut pair_frontier = frontier.ordinary_child_frontier(&mut pair.premise);
    for operand in &mut pair.operands {
        let ready = {
            let state = operand
                .state
                .as_mut()
                .expect("a retained pair operand lost its staged state");
            let StagedOperand::Pair(nested) = state else {
                continue;
            };
            collapse_initial_completed_pair_children(nested, &mut pair_frontier, tcx)?;
            staged_pair_is_closed(nested)
        };
        if !ready {
            continue;
        }
        let state = operand
            .state
            .take()
            .expect("a ready nested pair lost its staged shell");
        let StagedOperand::Pair(nested) = state else {
            unreachable!("a ready nested pair changed staged shape")
        };
        let source = nested.source;
        let completed = finalize_staged_pair(*nested, &mut pair_frontier, tcx)?;
        operand.state = Some(StagedOperand::Completed {
            source,
            completed,
            leaf: None,
        });
    }
    Ok(())
}

fn capture_open_completed_source(
    state: &StagedOperand<'_>,
    layer: usize,
    slots: &mut [ProjectedTemplateSlot],
) {
    let StagedOperand::OpenCompleted {
        source,
        preview,
        leaf,
        ..
    } = state
    else {
        unreachable!("a fills close request targeted another staged state")
    };
    let leaf = leaf
        .as_ref()
        .expect("a completed fills producer lost its template identity");
    let slot = slots
        .get_mut(leaf.template_index as usize)
        .expect("a completed template identity exceeded its dense table");
    assert!(
        slot.source
            .replace(ProjectedTemplateSource {
                source: (**source).clone(),
                ty: preview.ty().clone(),
                layer: Some(layer),
            })
            .is_none(),
        "a projected producer source was captured more than once"
    );
}

fn require_closed_recipe_match<'m>(
    actual: crate::pass::typecheck_core::ScopedType,
    expected: crate::pass::typecheck_core::ScopedType,
    span: crate::span::Span,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let (store, _owner, delta, _publication) = frontier.parts_mut();
    let actual = store.require_goal_free_after_delta(delta, actual, tcx)?;
    store.validate_goal_free_projected_type_relation(delta, &actual, &expected, span, tcx)
}

#[allow(clippy::too_many_arguments)]
fn close_projected_source_at_path<'m>(
    state: &mut Option<StagedOperand<'m>>,
    path: &[u8],
    layer: usize,
    slots: &mut [ProjectedTemplateSlot],
    leaf_expected: crate::pass::typecheck_core::ScopedType,
    span: crate::span::Span,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    if path.is_empty() {
        let staged = state
            .take()
            .expect("a producer close path lost its staged leaf");
        capture_open_completed_source(&staged, layer, slots);
        let (source, completed) = finalize_staged_operand(staged, frontier, tcx)?;
        require_closed_recipe_match(
            completed.scoped().clone(),
            leaf_expected,
            span,
            frontier,
            tcx,
        )?;
        *state = Some(StagedOperand::Completed {
            source,
            completed,
            leaf: None,
        });
        return Ok(());
    }
    let (&index, rest) = path
        .split_first()
        .expect("a producer close path has one pair operand");
    let Some(StagedOperand::Pair(pair)) = state.as_mut() else {
        unreachable!("a producer close path crossed a non-pair source")
    };
    let mut pair_frontier = frontier.ordinary_child_frontier(&mut pair.premise);
    let operand = &mut pair.operands[usize::from(index)];
    close_projected_source_at_path(
        &mut operand.state,
        rest,
        layer,
        slots,
        leaf_expected,
        span,
        &mut pair_frontier,
        tcx,
    )?;
    if let Some(StagedOperand::Pair(nested)) = operand.state.as_ref()
        && staged_pair_is_closed(nested)
    {
        let state = operand
            .state
            .take()
            .expect("a ready nested pair lost its staged shell");
        let StagedOperand::Pair(nested) = state else {
            unreachable!("a ready nested pair changed staged shape")
        };
        let source = nested.source;
        let completed = finalize_staged_pair(*nested, &mut pair_frontier, tcx)?;
        operand.state = Some(StagedOperand::Completed {
            source,
            completed,
            leaf: None,
        });
    }
    Ok(())
}

fn advance_projected_source_at_path<'m>(
    state: &mut Option<StagedOperand<'m>>,
    path: &[u8],
    mode: super::frontier::FrontierMode,
    relation_probe: bool,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<bool, crate::error::Error> {
    if path.is_empty() {
        let staged = state
            .take()
            .expect("a producer location lost its staged source");
        let (staged, complete) =
            advance_staged_producer(staged, mode, relation_probe, frontier, tcx)?;
        *state = Some(staged);
        return Ok(complete);
    }
    let (&index, rest) = path
        .split_first()
        .expect("a producer location has one pair operand");
    let Some(StagedOperand::Pair(pair)) = state.as_mut() else {
        unreachable!("a producer path crossed a non-pair source")
    };
    let mut pair_frontier = frontier.ordinary_child_frontier(&mut pair.premise);
    let operand = &mut pair.operands[usize::from(index)];
    let complete = advance_projected_source_at_path(
        &mut operand.state,
        rest,
        mode,
        relation_probe,
        &mut pair_frontier,
        tcx,
    )?;
    if complete && rest.is_empty() {
        let Some(StagedOperand::OpenCompleted {
            source,
            preview,
            child,
            ..
        }) = operand.state.as_mut()
        else {
            unreachable!("a completed projected pair producer lost its open preview")
        };
        let span = source.span();
        let expected = operand.expected.clone().into_expected_equation_operand(
            pair_frontier.owner(),
            pair_frontier.store(),
            span,
        )?;
        let mut producer_frontier = pair_frontier.ordinary_child_frontier(child);
        let (store, _owner, delta, _publication) = producer_frontier.parts_mut();
        store.constrain_equation(delta, preview.clone(), expected, span, tcx)?;
    }
    Ok(complete)
}

fn advance_staged_producer<'m>(
    state: StagedOperand<'m>,
    mode: super::frontier::FrontierMode,
    relation_probe: bool,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(StagedOperand<'m>, bool), crate::error::Error> {
    isolate_staged_producer(&state, frontier, tcx)?;
    let (continuation, leaf) = match state {
        StagedOperand::Lambda(lambda, leaf) => {
            (resume_staged_lambda(lambda, None, frontier, tcx)?, leaf)
        }
        StagedOperand::DeferredRecLambda(deferred, leaf) => {
            let mut deferred = *deferred;
            let resolution = deferred
                .resolution
                .expect("a deferred recursive lambda resumed before recipe classification");
            // Header reservation may retain a complete function spine whose
            // result is still a goal in the sealed recipe. A lifted body needs
            // that public header installed before selecting its distinct
            // recursive runtime result.
            if matches!(resolution, super::DeferredTailResolution::Lifted(_))
                && !matches!(
                    deferred.lambda.prepared.selection,
                    super::OrdinaryLambdaSelection::Check { .. }
                )
            {
                require_staged_lambda_result_anchor(
                    deferred.lambda.source,
                    &deferred.lambda.prepared.selection,
                )?;
                let expected = deferred
                    .public_header
                    .take()
                    .expect("a lifted deferred recursive lambda lost its reserved public header");
                install_staged_lambda_expected(&mut deferred.lambda, expected, frontier, tcx)?;
            }
            (
                resume_staged_lambda(deferred.lambda, Some(resolution), frontier, tcx)?,
                leaf,
            )
        }
        StagedOperand::Ordinary(continuation, leaf) => (continuation, leaf),
        completed @ (StagedOperand::Completed { .. } | StagedOperand::OpenCompleted { .. }) => {
            return Ok((completed, true));
        }
        StagedOperand::Pair(_) => unreachable!("a producer path stopped at a pair shell"),
    };
    let source = continuation.cursor.source;
    match super::OrdinaryValueCursor::advance_open_child_iterative(
        continuation,
        mode,
        super::OrdinaryCompletionDemand::GoalFreeValue,
        relation_probe,
        frontier,
        tcx,
    )? {
        super::OpenOrdinaryChildAdvance::Pending(continuation) => {
            Ok((StagedOperand::Ordinary(continuation, leaf), false))
        }
        super::OpenOrdinaryChildAdvance::Complete { child, completed } => {
            let preview = child.preview_goal_free_close_output(
                frontier.store(),
                completed.scoped().clone(),
                tcx,
            )?;
            Ok((
                StagedOperand::OpenCompleted {
                    source,
                    child,
                    completed,
                    preview,
                    accepted: false,
                    #[cfg(test)]
                    accepted_by_relation_wave: false,
                    leaf,
                },
                true,
            ))
        }
    }
}

fn require_staged_lambda_result_anchor(
    source: &crate::ast::Expr<crate::ast::Lowered>,
    selection: &super::OrdinaryLambdaSelection,
) -> Result<(), crate::error::Error> {
    if matches!(source, crate::ast::Expr::FnExpr { sig, .. } if sig.has_type_params())
        && matches!(
            selection,
            super::OrdinaryLambdaSelection::Synthesizing {
                annotated_return: None
            }
        )
    {
        return Err(crate::error::Error::type_(
            source.span(),
            "this polymorphic `fn` needs an independently known result type before its body is checked",
        )
        .with_help("write a concrete return annotation or provide an enclosing expected result"));
    }
    Ok(())
}

fn resume_staged_lambda<'m>(
    lambda: StagedLambda<'m>,
    deferred_tail: Option<super::DeferredTailResolution>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<super::OrdinaryChildContinuation<'m>, crate::error::Error> {
    let StagedLambda {
        source,
        expected,
        mut premise,
        header,
        retained_binders,
        prepared,
        header_mode,
        body_next_mode,
    } = lambda;
    assert!(
        header_mode.rank() <= super::frontier::FrontierMode::LexicalFallback.rank(),
        "a projected lambda body followed an unsealed header phase"
    );
    assert_eq!(
        body_next_mode,
        super::frontier::FrontierMode::ExpectedOnly,
        "a projected lambda body skipped its fresh expectation-only pass"
    );
    require_staged_lambda_result_anchor(source, &prepared.selection)?;
    let state = {
        let mut child_frontier = frontier.ordinary_child_frontier(&mut premise);
        super::OrdinaryValueCursor::enter_prepared_lambda_body(
            source,
            &retained_binders,
            prepared,
            deferred_tail,
            &mut child_frontier,
            tcx,
        )?
    };
    Ok(super::OrdinaryChildContinuation {
        child: premise,
        cursor: Box::new(super::OrdinaryValueCursor {
            source,
            expected,
            planned: super::PlannedOrdinaryValue::Lambda(super::PlannedOrdinaryLambda {
                header,
                retained_binders,
                state,
            }),
        }),
        next_mode: body_next_mode,
        suspension: super::OrdinarySuspension::Mode,
    })
}

#[allow(clippy::large_enum_variant)] // the owning fills state is already boxed, so its phase payload stays in that single allocation
pub(super) enum MarkedCallPhase<'m> {
    Project {
        action: crate::pass::typecheck_full::PreparedUserElaboratorAction<'m>,
        call: super::OrdinaryCallContinuation<'m>,
        projection: MarkedProjection<'m>,
    },
    Completed {
        action: crate::pass::typecheck_full::PreparedUserElaboratorAction<'m>,
        output: crate::pass::typecheck_core::ScopedType,
        inferred_type_arg_suffix: usize,
        transcript: super::UserElaboratorCallTranscript,
        expected_application: super::UserElaboratorExpectedApplication,
        projection: MarkedProjection<'m>,
    },
    AdoptingFills {
        ready: crate::pass::typecheck_full::ReadyProjectedUserElaboratorAction<'m>,
        checked: ProjectedCheckedResult,
        adoption: FillAdoption,
        projection: MarkedProjection<'m>,
        output: crate::pass::typecheck_core::ScopedType,
        inferred_type_arg_suffix: usize,
        expected_application: super::UserElaboratorExpectedApplication,
    },
    Ready {
        ready: crate::pass::typecheck_full::ReadyUserElaboratorAction,
        output: crate::pass::typecheck_core::ScopedType,
        inferred_type_arg_suffix: usize,
        expected_application: super::UserElaboratorExpectedApplication,
    },
}

impl<'m> MarkedCallState<'m> {
    pub(super) fn awaits_expected(&self) -> bool {
        match &self.phase {
            MarkedCallPhase::Project { call, .. } => matches!(
                call,
                super::OrdinaryCallContinuation::AwaitingExpected(_)
                    | super::OrdinaryCallContinuation::AwaitingReturnedForall { .. }
            ),
            MarkedCallPhase::Completed {
                expected_application,
                ..
            }
            | MarkedCallPhase::AdoptingFills {
                expected_application,
                ..
            }
            | MarkedCallPhase::Ready {
                expected_application,
                ..
            } => *expected_application == super::UserElaboratorExpectedApplication::Absent,
        }
    }

    pub(super) fn supply_expected(&mut self) {
        assert!(
            self.awaits_expected(),
            "a marked call accepted a second expectation"
        );
        match &mut self.phase {
            MarkedCallPhase::Project { .. } => {}
            MarkedCallPhase::Completed {
                expected_application,
                ..
            }
            | MarkedCallPhase::AdoptingFills {
                expected_application,
                ..
            }
            | MarkedCallPhase::Ready {
                expected_application,
                ..
            } => *expected_application = super::UserElaboratorExpectedApplication::Deferred,
        }
    }

    pub(super) fn push_completion_nodes<'a>(
        &'a self,
        nodes: &mut Vec<super::CompletionNode<'a, 'm>>,
    ) {
        let projection = match &self.phase {
            MarkedCallPhase::Project {
                call, projection, ..
            } => {
                nodes.push(super::CompletionNode::Call(call));
                projection
            }
            MarkedCallPhase::Completed { projection, .. }
            | MarkedCallPhase::AdoptingFills { projection, .. } => projection,
            MarkedCallPhase::Ready { .. } => return,
        };
        for producer in 0..projection.next_producer {
            if let StagedOperand::Ordinary(child, _) = projection.producer_state(producer) {
                nodes.push(super::CompletionNode::Value(&child.cursor));
            }
        }
    }

    pub(super) fn project(
        action: crate::pass::typecheck_full::PreparedUserElaboratorAction<'m>,
        call: super::OrdinaryCallContinuation<'m>,
    ) -> Self {
        Self::project_with(
            action,
            call,
            MarkedProjection {
                seal: InvocationSeal::fresh(),
                specialization: None,
                header_cursor: ProjectedHeaderCursor::default(),
                terminal_result: None,
                transcript: None,
                sources: Vec::new(),
                next_template: 0,
                next_producer: 0,
                next_close_producer: 0,
                producer_locations: Vec::new(),
                producer_completed: Vec::new(),
                template_slots: Vec::new(),
            },
        )
    }

    pub(super) fn project_with(
        action: crate::pass::typecheck_full::PreparedUserElaboratorAction<'m>,
        call: super::OrdinaryCallContinuation<'m>,
        projection: MarkedProjection<'m>,
    ) -> Self {
        Self {
            phase: MarkedCallPhase::Project {
                action,
                call,
                projection,
            },
        }
    }

    pub(super) fn ready(
        ready: crate::pass::typecheck_full::ReadyUserElaboratorAction,
        output: crate::pass::typecheck_core::ScopedType,
        inferred_type_arg_suffix: usize,
        expected_application: super::UserElaboratorExpectedApplication,
    ) -> Self {
        Self {
            phase: MarkedCallPhase::Ready {
                ready,
                output,
                inferred_type_arg_suffix,
                expected_application,
            },
        }
    }

    pub(super) fn completed(
        action: crate::pass::typecheck_full::PreparedUserElaboratorAction<'m>,
        output: crate::pass::typecheck_core::ScopedType,
        inferred_type_arg_suffix: usize,
        transcript: super::UserElaboratorCallTranscript,
        expected_application: super::UserElaboratorExpectedApplication,
        projection: MarkedProjection<'m>,
    ) -> Self {
        Self {
            phase: MarkedCallPhase::Completed {
                action,
                output,
                inferred_type_arg_suffix,
                transcript,
                expected_application,
                projection,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn adopting_fills(
        ready: crate::pass::typecheck_full::ReadyProjectedUserElaboratorAction<'m>,
        checked: ProjectedCheckedResult,
        adoption: FillAdoption,
        projection: MarkedProjection<'m>,
        output: crate::pass::typecheck_core::ScopedType,
        inferred_type_arg_suffix: usize,
        expected_application: super::UserElaboratorExpectedApplication,
    ) -> Self {
        Self {
            phase: MarkedCallPhase::AdoptingFills {
                ready,
                checked,
                adoption,
                projection,
                output,
                inferred_type_arg_suffix,
                expected_application,
            },
        }
    }

    pub(super) fn into_advance(self) -> MarkedCallPhase<'m> {
        self.phase
    }
}

struct RetainedProjectedSource<'m> {
    state: Option<StagedOperand<'m>>,
    header: RetainedProjectedHeader,
    inherited_mode: super::frontier::FrontierMode,
    layer: usize,
    expanded_start: usize,
    expanded_width: usize,
}

enum RetainedProjectedHeader {
    Open(crate::pass::typecheck_core::ScopedType),
    Prepared {
        public: crate::pass::typecheck_core::ScopedType,
        structural: crate::pass::typecheck_core::ScopedType,
        exports: Option<Vec<crate::pass::typecheck_core::ReservedParentHeaderExport>>,
    },
    Sealed(ProjectedCheckedRecipe),
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(
    std::mem::size_of::<RetainedProjectedHeader>() <= std::mem::size_of::<ProjectedCheckedRecipe>()
);

impl RetainedProjectedHeader {
    fn recipe(&self) -> &ProjectedCheckedRecipe {
        let Self::Sealed(recipe) = self else {
            unreachable!("a projected source recipe was observed before header sealing")
        };
        recipe
    }

    fn recipe_mut(&mut self) -> &mut ProjectedCheckedRecipe {
        let Self::Sealed(recipe) = self else {
            unreachable!("a projected source recipe was mutated before header sealing")
        };
        recipe
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectedHeaderPhase {
    Prepare,
    Reserve,
    Install,
    SelectLinkedHeaders,
    CloseProducerlessPairs,
    Complete,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectedHeaderWorkEvent {
    AtAncestor(ProjectedHeaderPhase, usize),
    Work(ProjectedHeaderPhase, usize),
    Link(usize),
    Seal(usize),
}

#[cfg(test)]
thread_local! {
    static PROJECTED_HEADER_WORK: std::cell::RefCell<Vec<ProjectedHeaderWorkEvent>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn reset_projected_header_work() {
    PROJECTED_HEADER_WORK.with(|work| work.borrow_mut().clear());
}

#[cfg(test)]
pub(crate) fn projected_header_work() -> Vec<ProjectedHeaderWorkEvent> {
    PROJECTED_HEADER_WORK.with(|work| work.borrow().clone())
}

#[cfg(test)]
fn record_projected_header_work(event: ProjectedHeaderWorkEvent) {
    PROJECTED_HEADER_WORK.with(|work| work.borrow_mut().push(event));
}

#[derive(Clone, Copy, Debug)]
struct ProjectedHeaderCursor {
    phase: ProjectedHeaderPhase,
    source: usize,
}

impl Default for ProjectedHeaderCursor {
    fn default() -> Self {
        Self {
            phase: ProjectedHeaderPhase::Prepare,
            source: 0,
        }
    }
}

pub(super) struct ProjectedHeaderRequest {
    pub(super) parent: crate::ast::TypeGoalOwner,
    close_expected: Option<crate::pass::typecheck_core::ScopedType>,
}

fn projected_source_range(
    resume: &super::SelectingOrdinaryCallResume<'_>,
) -> (usize, usize, usize) {
    match resume {
        super::SelectingOrdinaryCallResume::Preselected {
            selecting,
            expanded_start,
            selected_expanded_slots_after,
            ..
        } => (
            selecting
                .call
                .user_elaborator_transcript
                .as_ref()
                .expect("a projected source lost its elaborator transcript")
                .layers
                .len(),
            *expanded_start,
            selected_expanded_slots_after - expanded_start,
        ),
        super::SelectingOrdinaryCallResume::Probe {
            selecting,
            preselected: Some(preselected),
            ..
        } => (
            selecting
                .call
                .user_elaborator_transcript
                .as_ref()
                .expect("a projected source lost its elaborator transcript")
                .layers
                .len(),
            preselected.expanded_start,
            preselected.selected_expanded_slots_after - preselected.expanded_start,
        ),
        super::SelectingOrdinaryCallResume::Probe {
            preselected: None, ..
        } => unreachable!("a certified pair source reached projection before semantic selection"),
    }
}

fn projected_source_expected<'a, 'm>(
    resume: &'a super::SelectingOrdinaryCallResume<'m>,
) -> &'a super::OwnerBoundType {
    match resume {
        super::SelectingOrdinaryCallResume::Preselected { expected, .. } => expected,
        super::SelectingOrdinaryCallResume::Probe {
            preselected: Some(preselected),
            ..
        } => &preselected.expected,
        super::SelectingOrdinaryCallResume::Probe {
            preselected: None, ..
        } => unreachable!("a projected source reached staging before semantic selection"),
    }
}

struct StagedPair<'m> {
    premise: super::frontier::OpenRetainedPremise<'m>,
    source: &'m crate::ast::Expr<crate::ast::Lowered>,
    expected: Option<super::OwnerBoundType>,
    selecting: Box<super::SelectingOrdinaryCall<'m>>,
    operands: [StagedPairOperand<'m>; 2],
}

struct StagedPairOperand<'m> {
    expected: super::OwnerBoundType,
    expanded_start: usize,
    expanded_width: usize,
    state: Option<StagedOperand<'m>>,
}

#[allow(clippy::large_enum_variant)] // recursive pairs are boxed; boxing Lambda would allocate for every directly staged lambda
enum StagedOperand<'m> {
    Pair(Box<StagedPair<'m>>),
    Lambda(StagedLambda<'m>, Option<ProjectedLeaf>),
    DeferredRecLambda(Box<StagedDeferredRecLambda<'m>>, Option<ProjectedLeaf>),
    Completed {
        source: &'m crate::ast::Expr<crate::ast::Lowered>,
        completed: super::CompletedInFrame,
        leaf: Option<ProjectedLeaf>,
    },
    OpenCompleted {
        source: &'m crate::ast::Expr<crate::ast::Lowered>,
        child: super::frontier::OpenRetainedPremise<'m>,
        completed: super::CompletedInFrame,
        preview: crate::pass::typecheck_core::ScopedType,
        accepted: bool,
        #[cfg(test)]
        accepted_by_relation_wave: bool,
        leaf: Option<ProjectedLeaf>,
    },
    Ordinary(super::OrdinaryChildContinuation<'m>, Option<ProjectedLeaf>),
}

fn staged_operand_inferred_type_arg_suffix(operand: &StagedOperand<'_>) -> usize {
    match operand {
        StagedOperand::Pair(pair) => pair.selecting.result_inferred_type_arg_suffix,
        StagedOperand::Lambda(_, _) | StagedOperand::DeferredRecLambda(_, _) => 0,
        StagedOperand::Completed { completed, .. }
        | StagedOperand::OpenCompleted { completed, .. } => completed.inferred_type_arg_suffix(),
        StagedOperand::Ordinary(continuation, _) => {
            continuation.cursor.result_inferred_type_arg_suffix()
        }
    }
}

struct ProjectedLeaf {
    template_index: u32,
    producer: Option<u32>,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProjectedLeafObservation {
    pub(crate) path: Vec<u8>,
    pub(crate) template_index: u32,
    pub(crate) producer: Option<u32>,
}

#[cfg(test)]
thread_local! {
    static PROJECTED_SOURCE_OBSERVATIONS: TargetedTestState<Vec<Vec<ProjectedLeafObservation>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn reset_projected_source_observations(module_path: &str, span: crate::span::Span) {
    PROJECTED_SOURCE_OBSERVATIONS.with(|observations| {
        *observations.borrow_mut() = Some((module_path.to_owned(), span, Vec::new()));
    });
}

#[cfg(test)]
pub(crate) fn take_projected_source_observations() -> Vec<Vec<ProjectedLeafObservation>> {
    PROJECTED_SOURCE_OBSERVATIONS.with(|observations| {
        observations
            .borrow_mut()
            .take()
            .map(|(_, _, observations)| observations)
            .unwrap_or_default()
    })
}

struct StagedLambda<'m> {
    source: &'m crate::ast::Expr<crate::ast::Lowered>,
    expected: Option<super::OwnerBoundType>,
    premise: super::frontier::OpenRetainedPremise<'m>,
    header: super::super::annotation_plan::LambdaHeaderPlan<'m>,
    retained_binders: Vec<super::super::RetainedTypeBinder<'m>>,
    prepared: super::PreparedOrdinaryLambdaBody<'m>,
    header_mode: super::frontier::FrontierMode,
    body_next_mode: super::frontier::FrontierMode,
}

struct StagedDeferredRecLambda<'m> {
    lambda: StagedLambda<'m>,
    public_header: Option<super::OwnerBoundType>,
    token: crate::ast::NodeId,
    requirement: RecOrderTailRequirement,
    lifted_flow: RecOrderTypeFlow,
    resolution: Option<super::DeferredTailResolution>,
}

#[derive(Clone, Copy)]
struct DeferredTailCandidate {
    token: crate::ast::NodeId,
    requirement: RecOrderTailRequirement,
    lifted_flow: RecOrderTypeFlow,
    span: crate::span::Span,
}

#[cfg(all(target_pointer_width = "64", not(test)))]
const _: () = {
    assert!(std::mem::size_of::<StagedLambda<'static>>() == 1000);
    assert!(std::mem::size_of::<StagedOperand<'static>>() == 1016);
    assert!(std::mem::size_of::<Option<StagedOperand<'static>>>() == 1016);
};
#[cfg(all(target_pointer_width = "64", test))]
const _: () = {
    assert!(std::mem::size_of::<StagedLambda<'static>>() == 1056);
    assert!(std::mem::size_of::<StagedOperand<'static>>() == 1072);
    assert!(std::mem::size_of::<Option<StagedOperand<'static>>>() == 1072);
};

fn staged_lambda_operand<'m>(
    lambda: StagedLambda<'m>,
    leaf: Option<ProjectedLeaf>,
) -> StagedOperand<'m> {
    let crate::ast::Expr::FnExpr { body, .. } = lambda.source else {
        unreachable!("a staged lambda changed expression family")
    };
    let crate::ast::Expr::RecOrder { plan, ext, .. } = body.as_ref() else {
        return StagedOperand::Lambda(lambda, leaf);
    };
    let crate::ast::RecOrderDisposition::DeferredTail {
        requirement,
        lifted_flow,
    } = plan.disposition
    else {
        return StagedOperand::Lambda(lambda, leaf);
    };
    StagedOperand::DeferredRecLambda(
        Box::new(StagedDeferredRecLambda {
            lambda,
            public_header: None,
            token: *ext,
            requirement,
            lifted_flow,
            resolution: None,
        }),
        leaf,
    )
}

type DeferredTailDecision = (crate::ast::NodeId, super::DeferredTailResolution);

#[derive(Clone, Copy, Default)]
struct DeferredTailUsage {
    terminal: bool,
    escape: bool,
}

#[derive(Clone, Copy)]
enum DeferredTailSymbol {
    Empty,
    Candidate(u32),
    Pair(DeferredTailPairId),
    // Every candidate below Opaque was eagerly marked as an escape.
    Opaque,
}

#[derive(Clone, Copy)]
struct DeferredTailPairId(usize);

struct DeferredTailPair {
    left: DeferredTailSymbol,
    right: DeferredTailSymbol,
    marked_tail: bool,
    marked_escape: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeferredTailRole {
    Alias,
    Tail,
    Escape,
}

struct DeferredTailClassifier<'a> {
    candidates: &'a [Option<DeferredTailCandidate>],
    usage: &'a mut [DeferredTailUsage],
    pairs: Vec<DeferredTailPair>,
    mark_stack: Vec<DeferredTailSymbol>,
    aliases: DeferredTailAliases,
}

// Lexical branches share Copy handles into one invocation-local pair arena.
// Scoped bindings therefore update and restore this environment in place.
struct DeferredTailAliases(HashMap<String, DeferredTailSymbol>);

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DeferredTailClassifierWork {
    recipe_visits: usize,
    owned_symbol_nodes_cloned: usize,
    alias_snapshot_entries: usize,
    pair_nodes_allocated: usize,
    pair_mark_visits: usize,
    tail_pair_mark_visits: usize,
    escape_pair_mark_visits: usize,
    scope_binding_updates: usize,
}

#[cfg(test)]
thread_local! {
    static DEFERRED_TAIL_CLASSIFIER_WORK: std::cell::Cell<DeferredTailClassifierWork> =
        const { std::cell::Cell::new(DeferredTailClassifierWork {
            recipe_visits: 0,
            owned_symbol_nodes_cloned: 0,
            alias_snapshot_entries: 0,
            pair_nodes_allocated: 0,
            pair_mark_visits: 0,
            tail_pair_mark_visits: 0,
            escape_pair_mark_visits: 0,
            scope_binding_updates: 0,
        }) };
}

#[cfg(test)]
fn reset_deferred_tail_classifier_work() {
    DEFERRED_TAIL_CLASSIFIER_WORK.with(|work| work.set(DeferredTailClassifierWork::default()));
}

#[cfg(test)]
fn deferred_tail_classifier_work() -> DeferredTailClassifierWork {
    DEFERRED_TAIL_CLASSIFIER_WORK.with(std::cell::Cell::get)
}

#[cfg(test)]
fn record_deferred_tail_classifier_work(update: impl FnOnce(&mut DeferredTailClassifierWork)) {
    DEFERRED_TAIL_CLASSIFIER_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

fn resolve_deferred_tail_decisions(
    candidates: &[Option<DeferredTailCandidate>],
    usage: &[DeferredTailUsage],
) -> Result<Vec<Option<DeferredTailDecision>>, crate::error::Error> {
    let mut decisions = vec![None; candidates.len()];
    for (index, candidate) in candidates.iter().enumerate() {
        let Some(candidate) = candidate else { continue };
        let resolution = match (
            candidate.requirement,
            usage[index].terminal,
            usage[index].escape,
        ) {
            (_, true, false) => Lifted(candidate.lifted_flow),
            (RecOrderTailRequirement::Optional, false, _) => OrdinaryBypass,
            _ => {
                return Err(crate::pass::typecheck_full::deferred_rec_tail_error(
                    candidate.span,
                ));
            }
        };
        decisions[index] = Some((candidate.token, resolution));
    }
    Ok(decisions)
}

fn collect_deferred_tail_candidates(
    operand: &StagedOperand<'_>,
    candidates: &mut [Option<DeferredTailCandidate>],
) {
    match operand {
        StagedOperand::Pair(pair) => {
            for operand in &pair.operands {
                let state = operand
                    .state
                    .as_ref()
                    .expect("a staged pair operand lost its state");
                collect_deferred_tail_candidates(state, candidates);
            }
        }
        StagedOperand::DeferredRecLambda(deferred, leaf) => {
            let template = leaf
                .as_ref()
                .expect("a deferred recursive lambda lost its template identity")
                .template_index;
            assert!(
                candidates[template as usize]
                    .replace(DeferredTailCandidate {
                        token: deferred.token,
                        requirement: deferred.requirement,
                        lifted_flow: deferred.lifted_flow,
                        span: deferred.lambda.source.span(),
                    })
                    .is_none(),
                "two deferred recursive lambdas shared one template identity"
            );
        }
        StagedOperand::Lambda(..)
        | StagedOperand::Completed { .. }
        | StagedOperand::OpenCompleted { .. }
        | StagedOperand::Ordinary(..) => {}
    }
}

fn install_deferred_tail_resolutions(
    operand: &mut StagedOperand<'_>,
    decisions: &mut [Option<DeferredTailDecision>],
    template_slots: &mut [ProjectedTemplateSlot],
) {
    match operand {
        StagedOperand::Pair(pair) => {
            for operand in &mut pair.operands {
                let state = operand
                    .state
                    .as_mut()
                    .expect("a staged pair operand lost its state");
                install_deferred_tail_resolutions(state, decisions, template_slots);
            }
        }
        StagedOperand::DeferredRecLambda(deferred, leaf) => {
            let template = leaf
                .as_ref()
                .expect("a deferred recursive lambda lost its template identity")
                .template_index;
            let (token, resolution) = decisions[template as usize]
                .take()
                .expect("a deferred recursive lambda lost its classified disposition");
            assert_eq!(
                token, deferred.token,
                "a deferred recursive lambda changed exact source occurrence"
            );
            if let super::DeferredTailResolution::Lifted(flow) = resolution {
                assert_eq!(
                    flow, deferred.lifted_flow,
                    "a deferred recursive lambda changed its lifted typing flow"
                );
                let crate::ast::Expr::FnExpr { body, .. } = deferred.lambda.source else {
                    unreachable!("a deferred recursive lambda changed expression family")
                };
                let crate::ast::Expr::RecOrder { plan, .. } = body.as_ref() else {
                    unreachable!("a deferred recursive lambda lost its root carrier")
                };
                assert!(
                    template_slots[template as usize]
                        .lifted_runtime_return
                        .replace(plan.runtime_ty.clone())
                        .is_none(),
                    "a deferred recursive lambda acquired two runtime return views"
                );
            } else {
                assert_eq!(
                    deferred.requirement,
                    RecOrderTailRequirement::Optional,
                    "a required recursive lambda selected ordinary bypass"
                );
            }
            assert!(
                deferred.resolution.replace(resolution).is_none(),
                "a deferred recursive lambda was classified twice"
            );
        }
        StagedOperand::Lambda(..)
        | StagedOperand::Completed { .. }
        | StagedOperand::OpenCompleted { .. }
        | StagedOperand::Ordinary(..) => {}
    }
}

impl DeferredTailRole {
    fn branch(self) -> Self {
        if self == Self::Tail {
            Self::Tail
        } else {
            Self::Escape
        }
    }
}

impl DeferredTailAliases {
    fn new() -> Self {
        Self(HashMap::new())
    }

    fn get(&self, name: &str) -> DeferredTailSymbol {
        self.0
            .get(name)
            .copied()
            .unwrap_or(DeferredTailSymbol::Empty)
    }

    fn insert(&mut self, name: &str, symbol: DeferredTailSymbol) -> Option<DeferredTailSymbol> {
        #[cfg(test)]
        record_deferred_tail_classifier_work(|work| work.scope_binding_updates += 1);
        self.0.insert(name.to_owned(), symbol)
    }

    fn restore(&mut self, name: &str, previous: Option<DeferredTailSymbol>) {
        #[cfg(test)]
        record_deferred_tail_classifier_work(|work| work.scope_binding_updates += 1);
        if let Some(previous) = previous {
            self.0.insert(name.to_owned(), previous);
        } else {
            self.0.remove(name);
        }
    }
}

impl<'a> DeferredTailClassifier<'a> {
    fn new(
        candidates: &'a [Option<DeferredTailCandidate>],
        usage: &'a mut [DeferredTailUsage],
    ) -> Self {
        Self {
            candidates,
            usage,
            pairs: Vec::new(),
            mark_stack: Vec::new(),
            aliases: DeferredTailAliases::new(),
        }
    }

    fn pair(&mut self, left: DeferredTailSymbol, right: DeferredTailSymbol) -> DeferredTailSymbol {
        #[cfg(test)]
        record_deferred_tail_classifier_work(|work| work.pair_nodes_allocated += 1);
        let id = DeferredTailPairId(self.pairs.len());
        self.pairs.push(DeferredTailPair {
            left,
            right,
            marked_tail: false,
            marked_escape: false,
        });
        DeferredTailSymbol::Pair(id)
    }

    fn reference(
        &mut self,
        symbol: DeferredTailSymbol,
        role: DeferredTailRole,
    ) -> DeferredTailSymbol {
        if role == DeferredTailRole::Alias {
            symbol
        } else {
            self.mark(symbol, DeferredTailRole::Escape);
            DeferredTailSymbol::Empty
        }
    }

    fn mark(&mut self, symbol: DeferredTailSymbol, role: DeferredTailRole) {
        assert!(
            role != DeferredTailRole::Alias,
            "an alias role reached deferred-tail marking"
        );
        assert!(
            self.mark_stack.is_empty(),
            "a deferred-tail mark traversal was reentered"
        );
        self.mark_stack.push(symbol);
        while let Some(symbol) = self.mark_stack.pop() {
            match symbol {
                DeferredTailSymbol::Empty | DeferredTailSymbol::Opaque => {}
                DeferredTailSymbol::Candidate(template) => {
                    let usage = &mut self.usage[template as usize];
                    if role == DeferredTailRole::Tail {
                        usage.terminal = true;
                    } else {
                        usage.escape = true;
                    }
                }
                DeferredTailSymbol::Pair(id) => {
                    let pair = &mut self.pairs[id.0];
                    let marked = if role == DeferredTailRole::Tail {
                        &mut pair.marked_tail
                    } else {
                        &mut pair.marked_escape
                    };
                    if *marked {
                        continue;
                    }
                    *marked = true;
                    #[cfg(test)]
                    record_deferred_tail_classifier_work(|work| {
                        work.pair_mark_visits += 1;
                        if role == DeferredTailRole::Tail {
                            work.tail_pair_mark_visits += 1;
                        } else {
                            work.escape_pair_mark_visits += 1;
                        }
                    });
                    let left = pair.left;
                    let right = pair.right;
                    self.mark_stack.push(right);
                    self.mark_stack.push(left);
                }
            }
        }
    }

    /// Alias transport preserves only exact candidate topology. Every opaque
    /// subtree is first walked as an escape, including opaque pair siblings.
    fn fold(
        &mut self,
        recipe: &ProjectedCheckedRecipe,
        role: DeferredTailRole,
    ) -> DeferredTailSymbol {
        use DeferredTailRole::{Alias, Escape};
        use DeferredTailSymbol::{Candidate, Empty, Opaque, Pair};
        #[cfg(test)]
        record_deferred_tail_classifier_work(|work| work.recipe_visits += 1);
        match recipe.node.as_ref() {
            ProjectedRecipeNode::Closed(_) => Empty,
            ProjectedRecipeNode::TemplateValue(template) => {
                let symbol = if self
                    .candidates
                    .get(*template as usize)
                    .is_some_and(Option::is_some)
                {
                    Candidate(*template)
                } else {
                    Empty
                };
                self.reference(symbol, role)
            }
            ProjectedRecipeNode::Local { name } => {
                let symbol = self.aliases.get(name);
                self.reference(symbol, role)
            }
            ProjectedRecipeNode::Pair(left, right) if role == Alias => {
                let left = self.fold(left, Alias);
                let right = self.fold(right, Alias);
                self.pair(left, right)
            }
            ProjectedRecipeNode::Pair(left, right) => {
                self.fold(left, Escape);
                self.fold(right, Escape);
                Empty
            }
            ProjectedRecipeNode::ProductProjection { product, left, .. } => {
                let selected = match self.fold(product, Alias) {
                    Pair(id) => {
                        let pair = &self.pairs[id.0];
                        if *left { pair.left } else { pair.right }
                    }
                    Empty => Empty,
                    invalid => {
                        self.mark(invalid, Escape);
                        Opaque
                    }
                };
                self.reference(selected, role)
            }
            ProjectedRecipeNode::TermCall {
                function, argument, ..
            } => {
                let function = self.fold(function, Alias);
                if matches!(function, Candidate(_)) {
                    self.mark(function, role.branch());
                } else {
                    self.mark(function, Escape);
                }
                self.fold(argument, Escape);
                Opaque
            }
            ProjectedRecipeNode::IntrinsicAbsurd { bottom } => {
                self.fold(bottom, Escape);
                Opaque
            }
            ProjectedRecipeNode::IntrinsicEither {
                value,
                left_name,
                left_body,
                right_name,
                right_body,
                ..
            } => {
                self.fold(value, Escape);
                let previous = self.aliases.insert(left_name, Empty);
                self.fold(left_body, role.branch());
                self.aliases.restore(left_name, previous);
                let previous = self.aliases.insert(right_name, Empty);
                self.fold(right_body, role.branch());
                self.aliases.restore(right_name, previous);
                Opaque
            }
            ProjectedRecipeNode::TermLet {
                name, value, body, ..
            } => {
                let value = self.fold(value, Alias);
                let previous = self.aliases.insert(name, value);
                let result = self.fold(body, role);
                self.aliases.restore(name, previous);
                result
            }
            ProjectedRecipeNode::TermFn { names, body, .. } => {
                let mut previous = Vec::with_capacity(names.len());
                for name in names {
                    previous.push(self.aliases.insert(name, Empty));
                }
                self.fold(body, Escape);
                for (name, previous) in names.iter().zip(previous).rev() {
                    self.aliases.restore(name, previous);
                }
                Opaque
            }
            ProjectedRecipeNode::TermTypeApp { term, .. } => match self.fold(term, Alias) {
                exact @ (Candidate(_) | Empty) => self.reference(exact, role),
                opaque => {
                    self.mark(opaque, Escape);
                    Opaque
                }
            },
            ProjectedRecipeNode::TermTypeFn { body, .. }
            | ProjectedRecipeNode::IntrinsicInjection { value: body, .. } => {
                self.fold(body, Escape);
                Opaque
            }
            ProjectedRecipeNode::IntrinsicIfThenElse {
                condition,
                true_body,
                false_body,
            } => {
                self.fold(condition, Escape);
                self.fold(true_body, role.branch());
                self.fold(false_body, role.branch());
                Opaque
            }
        }
    }
}

pub(super) enum ProjectedDescentGate<'m> {
    Staged {
        resume: super::SelectingOrdinaryCallResume<'m>,
        completed: super::CompletedInFrame,
    },
    Ordinary(super::SelectingOrdinaryCallAdvance<'m>),
}

pub(super) fn stage_projected_descent<'m>(
    advance: super::SelectingOrdinaryCallAdvance<'m>,
    projection: &mut MarkedProjection<'m>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<ProjectedDescentGate<'m>, crate::error::Error> {
    let super::SelectingOrdinaryCallAdvance::Descend {
        mut child,
        inherited_mode,
        resume,
    } = advance
    else {
        return Ok(ProjectedDescentGate::Ordinary(advance));
    };
    let source_span = child.cursor.source.span();
    let (layer, expanded_start, expanded_width) = projected_source_range(&resume);
    let public_expected = projected_source_expected(&resume).clone();
    let public_header =
        public_expected
            .clone()
            .into_scoped(frontier.owner(), frontier.store(), source_span)?;
    if child.cursor.expected.is_none() {
        if child.cursor.awaits_expected() {
            child.cursor.supply_expected(public_expected);
        } else {
            child.cursor.expected = Some(public_expected);
        }
        child.next_mode = super::frontier::FrontierMode::ExpectedOnly;
    }
    let source_index = projection.sources.len();
    let mut state = if let Some(pair_operands) = certified_pair_operands(&child, tcx) {
        StagedOperand::Pair(Box::new(stage_pair_wrapper(
            child,
            pair_operands,
            frontier,
            tcx,
        )?))
    } else {
        stage_projected_source_operand(child, frontier, tcx)?
    };
    let inferred_type_arg_suffix = staged_operand_inferred_type_arg_suffix(&state);
    reserve_projected_source(
        &mut state,
        source_index,
        &mut projection.next_template,
        &mut projection.next_producer,
        &mut projection.producer_locations,
    );
    #[cfg(test)]
    record_projected_source_observation(&state, &tcx.env.module_path, source_span);
    if let StagedOperand::Pair(root) = &mut state {
        projection.template_slots.resize_with(
            projection.next_template as usize,
            ProjectedTemplateSlot::default,
        );
        capture_completed_pair_sources(root, layer, &mut projection.template_slots, frontier, tcx)?;
    }
    projection.sources.push(RetainedProjectedSource {
        state: Some(state),
        header: RetainedProjectedHeader::Open(public_header.clone()),
        inherited_mode,
        layer,
        expanded_start,
        expanded_width,
    });
    projection
        .producer_completed
        .resize(projection.next_producer as usize, false);
    projection.template_slots.resize_with(
        projection.next_template as usize,
        ProjectedTemplateSlot::default,
    );
    Ok(ProjectedDescentGate::Staged {
        resume,
        completed: super::CompletedInFrame::Value(public_header, inferred_type_arg_suffix),
    })
}

fn certified_pair_operands<'m>(
    child: &super::OrdinaryChildContinuation<'m>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Option<[&'m crate::ast::Expr<crate::ast::Lowered>; 2]> {
    let super::PlannedOrdinaryValue::Call(super::OrdinaryCallContinuation::Selecting(selecting)) =
        &child.cursor.planned
    else {
        return None;
    };
    let direct = selecting.direct.as_ref()?;
    if !canonical_pair_certificate(direct, tcx)
        || selecting.next_candidate != 0
        || selecting.selected_slots != 0
        || selecting.value_slots != 2
        || selecting.expanded_value_slots != 2
        || selecting.call.shape.result_is_pre_residual
        || !selecting.call.shape.has_value_layer
    {
        return None;
    }
    let [
        super::CallArgRef::Value(left),
        super::CallArgRef::Value(right),
    ] = selecting.candidates.as_slice()
    else {
        return None;
    };
    #[cfg(test)]
    if pair_transparency_is_disabled(&tcx.env.module_path, child.cursor.source.span()) {
        return None;
    }
    Some([*left, *right])
}

fn certified_path_unit_call_tree_source(
    source: &crate::ast::Expr<crate::ast::Lowered>,
    original_source: &crate::ast::Expr<crate::ast::Lowered>,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> bool {
    if tcx.is_pending_rec_order_source(original_source) {
        return false;
    }
    let mut current = source;
    let mut siblings = Vec::new();
    loop {
        if tcx.is_pending_rec_order_source(current) {
            return false;
        }
        let crate::ast::Expr::Call { callee, args, .. } = current else {
            return false;
        };
        if !matches!(callee.as_ref(), crate::ast::Expr::Path { .. })
            || tcx.is_pending_rec_order_path(callee)
        {
            return false;
        }
        // An empty prefix call supplies Unit; a nonempty type-only call does not.
        let mut has_value = args.is_empty();
        let mut nested = None;
        for arg in args {
            let crate::ast::CallArg::Value(value) = arg else {
                continue;
            };
            has_value = true;
            match value {
                crate::ast::Expr::Path { .. } => {
                    if tcx.is_pending_rec_order_path(value) {
                        return false;
                    }
                }
                crate::ast::Expr::Unit { .. } => {}
                crate::ast::Expr::Call { .. } => {
                    if let Some(sibling) = nested.replace(value) {
                        siblings.push(sibling);
                    }
                }
                _ => return false,
            }
        }
        if !has_value {
            return false;
        }
        // The common one-layer and unary-nested shapes need no heap worklist.
        // This selector enters no term bodies; ordinary inference closes the type.
        match nested.or_else(|| siblings.pop()) {
            Some(next) => current = next,
            None => return true,
        }
    }
}

fn certified_one_layer_scalar_literal_source(
    source: &crate::ast::Expr<crate::ast::Lowered>,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> bool {
    if tcx.has_pending_rec_order() {
        return false;
    }
    let crate::ast::Expr::Call { callee, args, .. } = source else {
        return false;
    };
    if !matches!(callee.as_ref(), crate::ast::Expr::Path { .. }) {
        return false;
    }
    let mut found_literal = false;
    for arg in args {
        let crate::ast::CallArg::Value(value) = arg else {
            continue;
        };
        match value {
            crate::ast::Expr::Path { .. } | crate::ast::Expr::Unit { .. } => {}
            value if super::super::literal_check_parts(value).is_some() => found_literal = true,
            _ => return false,
        }
    }
    found_literal
}

fn certified_fresh_unresolved_direct_call_source<'a>(
    child: &'a super::OrdinaryChildContinuation<'_>,
) -> Option<&'a crate::ast::Expr<crate::ast::Lowered>> {
    if child.next_mode != super::frontier::FrontierMode::ExpectedOnly {
        return None;
    }
    let super::PlannedOrdinaryValue::Call(super::OrdinaryCallContinuation::Selecting(selecting)) =
        &child.cursor.planned
    else {
        return None;
    };
    if selecting.direct.is_none()
        || untouched_projected_selecting_result(selecting).is_none_or(|result| {
            !crate::pass::typecheck_core::type_contains_goal(result.as_type())
                || crate::pass::typecheck_core::type_contains_infer(result.as_type())
        })
    {
        return None;
    }
    Some(
        selecting
            .regular_ufcs
            .as_ref()
            .map_or(child.cursor.source, |(_, normalized)| normalized),
    )
}

fn certified_fresh_path_unit_call(
    child: &super::OrdinaryChildContinuation<'_>,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> bool {
    certified_fresh_unresolved_direct_call_source(child).is_some_and(|source| {
        certified_path_unit_call_tree_source(source, child.cursor.source, tcx)
    })
}

fn certified_fresh_scalar_literal_call(
    child: &super::OrdinaryChildContinuation<'_>,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> bool {
    certified_fresh_unresolved_direct_call_source(child)
        .is_some_and(|source| certified_one_layer_scalar_literal_source(source, tcx))
}

fn path_receiver_field_access(source: &crate::ast::Expr<crate::ast::Lowered>) -> bool {
    matches!(
        source,
        crate::ast::Expr::Elaborator {
            kind: crate::ast::ElaboratorKind::Access,
            call: crate::ast::ElaboratorCall::FieldAccess { receiver, .. },
            ..
        } if matches!(receiver.as_ref(), crate::ast::Expr::Path { .. })
    )
}

fn certified_computed_field_arg(
    arg: &crate::ast::CallArg<crate::ast::Lowered>,
    allow_local_infer: bool,
) -> bool {
    match arg {
        crate::ast::CallArg::Type(ty) => {
            !crate::pass::typecheck_core::type_contains_goal(ty)
                && (!crate::pass::typecheck_core::type_contains_infer(ty)
                    || (allow_local_infer && matches!(ty, crate::ast::Type::Infer { .. })))
        }
        crate::ast::CallArg::Value(value) => certified_computed_field_value(value),
    }
}

fn certified_computed_field_value(source: &crate::ast::Expr<crate::ast::Lowered>) -> bool {
    match source {
        crate::ast::Expr::Path { .. } | crate::ast::Expr::Unit { .. } => true,
        source
            if super::super::literal_check_parts(source).is_some_and(|(_, annotation, _)| {
                annotation.is_none_or(|ty| {
                    !crate::pass::typecheck_core::type_contains_goal(ty)
                        && !crate::pass::typecheck_core::type_contains_infer(ty)
                })
            }) =>
        {
            true
        }
        source if path_receiver_field_access(source) => true,
        crate::ast::Expr::Call { callee, args, .. }
            if matches!(callee.as_ref(), crate::ast::Expr::Path { .. }) =>
        {
            args.iter()
                .all(|arg| certified_computed_field_arg(arg, true))
        }
        crate::ast::Expr::Call { callee, args, .. } if path_receiver_field_access(callee) => args
            .iter()
            .all(|arg| certified_computed_field_arg(arg, false)),
        _ => false,
    }
}

fn certified_computed_field_root(source: &crate::ast::Expr<crate::ast::Lowered>) -> bool {
    matches!(
        source,
        crate::ast::Expr::Call { callee, args, .. }
            if path_receiver_field_access(callee)
                && args
                    .iter()
                    .all(|arg| certified_computed_field_arg(arg, false))
    )
}

fn certified_fresh_field_source(
    child: &super::OrdinaryChildContinuation<'_>,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> bool {
    child.next_mode == super::frontier::FrontierMode::ExpectedOnly
        && !tcx.has_pending_rec_order()
        && match &child.cursor.planned {
            super::PlannedOrdinaryValue::Recursive(super::OrdinaryRecursiveFamily::Field) => {
                path_receiver_field_access(child.cursor.source)
            }
            super::PlannedOrdinaryValue::Recursive(
                super::OrdinaryRecursiveFamily::ComputedCall,
            ) => certified_computed_field_root(child.cursor.source),
            _ => false,
        }
}

fn projected_scalar_result_goals_are_contributed(
    ty: &crate::ast::Type<crate::ast::Lowered>,
    contributed: &std::collections::HashSet<crate::ast::TypeGoalRef>,
) -> bool {
    match ty {
        crate::ast::Type::Goal { goal, args, .. } => args.is_empty() && contributed.contains(goal),
        crate::ast::Type::Infer { .. } => false,
        crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => true,
        crate::ast::Type::Function { param, ret, .. } => {
            projected_scalar_result_goals_are_contributed(param, contributed)
                && projected_scalar_result_goals_are_contributed(ret, contributed)
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            projected_scalar_result_goals_are_contributed(left, contributed)
                && projected_scalar_result_goals_are_contributed(right, contributed)
        }
        crate::ast::Type::Path { args, .. } => args
            .iter()
            .all(|arg| projected_scalar_result_goals_are_contributed(arg, contributed)),
        crate::ast::Type::Forall { body, .. } => {
            projected_scalar_result_goals_are_contributed(body, contributed)
        }
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
    }
}

fn certified_scalar_literal_call_at_lexical_fallback<'m>(
    child: &mut super::OrdinaryChildContinuation<'m>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<bool, crate::error::Error> {
    if child.next_mode != super::frontier::FrontierMode::LexicalFallback {
        return Ok(false);
    }
    let super::OrdinaryChildContinuation {
        child: premise,
        cursor,
        ..
    } = child;
    let super::PlannedOrdinaryValue::Call(call) = &cursor.planned else {
        return Ok(false);
    };
    let advancing = match call {
        super::OrdinaryCallContinuation::Advancing(advancing) => advancing.as_ref(),
        super::OrdinaryCallContinuation::AdvancingLayer(layer) if layer.remaining.is_empty() => {
            layer.advancing.as_ref()
        }
        _ => return Ok(false),
    };
    if advancing.pending.is_empty() {
        return Ok(false);
    }
    let result = advancing.result.clone();
    let mut child_frontier = frontier.ordinary_child_frontier(premise);
    let (store, owner, delta, _publication) = child_frontier.parts_mut();
    assert_eq!(
        advancing.destination, owner,
        "a retained scalar call must advance in its exact child owner"
    );
    let ambient_binders = tcx.in_scope_type_param_binders();
    let aliases = tcx.binder_alias_ctx(&ambient_binders);
    let mut contributed = std::collections::HashSet::with_capacity(advancing.pending.len());
    for pending in &advancing.pending {
        if !matches!(&pending.role, super::PendingOrdinaryChildRole::Value)
            || pending.child.next_mode != super::frontier::FrontierMode::LexicalFallback
            || super::super::literal_check_parts(pending.child.cursor.source).is_none()
        {
            return Ok(false);
        }
        let span = pending.child.cursor.source.span();
        let Some(expected) = pending.child.cursor.expected.clone() else {
            return Ok(false);
        };
        let expected = expected.into_scoped(owner, store, span)?;
        let expected = store.zonk_for_progress(delta, expected, tcx)?;
        let (expected, _) = crate::pass::typecheck_core::aliases::canonicalize_deep_for_comparison(
            expected.ty().as_type(),
            &aliases,
            expected.ty().identity_is_canonical(),
        );
        match expected {
            crate::ast::Type::Goal { goal, args, .. } if args.is_empty() => {
                contributed.insert(goal);
            }
            expected
                if !crate::pass::typecheck_core::type_contains_goal(&expected)
                    && !crate::pass::typecheck_core::type_contains_infer(&expected) => {}
            _ => return Ok(false),
        }
    }
    let result = store.zonk_for_progress(delta, result, tcx)?;
    let (result, _) = crate::pass::typecheck_core::aliases::canonicalize_deep_for_comparison(
        result.ty().as_type(),
        &aliases,
        result.ty().identity_is_canonical(),
    );
    Ok(projected_scalar_result_goals_are_contributed(
        &result,
        &contributed,
    ))
}

fn stage_projected_source_operand<'m>(
    continuation: super::OrdinaryChildContinuation<'m>,
    frontier: &mut impl super::OrdinaryFrontierReentry<'m>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<StagedOperand<'m>, crate::error::Error> {
    if certified_fresh_path_unit_call(&continuation, tcx)
        || certified_fresh_scalar_literal_call(&continuation, tcx)
        || certified_fresh_field_source(&continuation, tcx)
    {
        stage_header_complete_operand(continuation, frontier, tcx)
    } else {
        stage_existing_operand(continuation, frontier, tcx)
    }
}

fn staged_operand_span(operand: &StagedOperand<'_>) -> crate::span::Span {
    match operand {
        StagedOperand::Pair(pair) => pair.source.span(),
        StagedOperand::Lambda(lambda, _) => lambda.source.span(),
        StagedOperand::DeferredRecLambda(deferred, _) => deferred.lambda.source.span(),
        StagedOperand::Completed { source, .. } | StagedOperand::OpenCompleted { source, .. } => {
            source.span()
        }
        StagedOperand::Ordinary(continuation, _) => continuation.cursor.source.span(),
    }
}

fn projected_fallback_header<'m>(
    expected: &super::OwnerBoundType,
    span: crate::span::Span,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
) -> Result<crate::pass::typecheck_core::ScopedType, crate::error::Error> {
    expected
        .clone()
        .into_scoped(frontier.owner(), frontier.store(), span)
}

fn reserve_observed_call_header<'m>(
    call: &mut super::OrdinaryCallContinuation<'m>,
    source_span: crate::span::Span,
    exports: &mut Vec<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<Option<crate::pass::typecheck_core::ScopedType>, crate::error::Error> {
    if let super::OrdinaryCallContinuation::Successor(successor) = call {
        let nested = {
            let mut child_frontier = frontier.ordinary_child_frontier(&mut successor.child);
            reserve_observed_cursor_header(
                &mut successor.cursor,
                exports,
                &mut child_frontier,
                tcx,
            )?
        };
        let Some(nested) = nested else {
            return Ok(None);
        };
        let export =
            frontier.reserve_parent_header_export(&successor.child, nested, source_span)?;
        let header = export.parent_header().clone();
        exports.push(export);
        return Ok(Some(header));
    }
    projected_call_header_result(call, source_span, frontier, tcx)
}

fn reserve_observed_marked_header<'m>(
    state: &mut MarkedCallState<'m>,
    source_span: crate::span::Span,
    exports: &mut Vec<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<Option<crate::pass::typecheck_core::ScopedType>, crate::error::Error> {
    match &mut state.phase {
        MarkedCallPhase::Project { call, .. } => {
            reserve_observed_call_header(call, source_span, exports, frontier, tcx)
        }
        MarkedCallPhase::Completed { output, .. }
        | MarkedCallPhase::AdoptingFills { output, .. }
        | MarkedCallPhase::Ready { output, .. } => Ok(Some(output.clone())),
    }
}

fn reserve_observed_cursor_header<'m>(
    cursor: &mut super::OrdinaryValueCursor<'m>,
    exports: &mut Vec<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<Option<crate::pass::typecheck_core::ScopedType>, crate::error::Error> {
    let span = cursor.source.span();
    match &mut cursor.planned {
        super::PlannedOrdinaryValue::Recursive(super::OrdinaryRecursiveFamily::RecQuote)
        | super::PlannedOrdinaryValue::RecQuote(_) => {
            let crate::ast::Expr::RecQuote { plan, .. } = cursor.source else {
                unreachable!("quoted source header keeps its carrier")
            };
            let crate::ast::RecQuotePlan::Operand {
                public_ty: Some(public_ty),
                ..
            } = plan.as_ref()
            else {
                return Ok(None);
            };
            Ok(Some(frontier.store().scoped_type(
                frontier.owner(),
                tcx.intern_type(public_ty),
                span,
            )?))
        }
        super::PlannedOrdinaryValue::Call(call) => {
            reserve_observed_call_header(call, span, exports, frontier, tcx)
        }
        super::PlannedOrdinaryValue::ComputedCall(
            super::ComputedCallContinuation::Application {
                callee_child,
                application,
            },
        ) => {
            let header = {
                let mut callee_frontier = frontier.ordinary_child_frontier(callee_child);
                let observed = {
                    let mut application_frontier =
                        callee_frontier.ordinary_child_frontier(&mut application.child);
                    reserve_observed_cursor_header(
                        &mut application.cursor,
                        exports,
                        &mut application_frontier,
                        tcx,
                    )?
                };
                let Some(observed) = observed else {
                    return Ok(None);
                };
                let export = callee_frontier.reserve_parent_header_export(
                    &application.child,
                    observed,
                    span,
                )?;
                let header = export.parent_header().clone();
                exports.push(export);
                header
            };
            let export = frontier.reserve_parent_header_export(callee_child, header, span)?;
            let header = export.parent_header().clone();
            exports.push(export);
            Ok(Some(header))
        }
        super::PlannedOrdinaryValue::Field(super::FieldContinuation::Receiver(receiver))
            if matches!(
                cursor.source,
                crate::ast::Expr::Elaborator {
                    call: crate::ast::ElaboratorCall::FieldAccess { .. },
                    ..
                }
            ) =>
        {
            let observed = {
                let mut receiver_frontier = frontier.ordinary_child_frontier(&mut receiver.child);
                let observed = reserve_observed_cursor_header(
                    &mut receiver.cursor,
                    exports,
                    &mut receiver_frontier,
                    tcx,
                )?;
                match observed {
                    Some(observed)
                        if !crate::pass::typecheck_core::type_contains_infer(
                            observed.ty().as_type(),
                        ) =>
                    {
                        let (store, _owner, delta, _publication) = receiver_frontier.parts_mut();
                        store.try_zonk_goal_free(delta, observed, tcx)?
                    }
                    _ => None,
                }
            };
            let Some(observed) = observed else {
                return Ok(None);
            };
            let export = frontier.reserve_parent_header_export(&receiver.child, observed, span)?;
            let receiver_header = export.parent_header().clone();
            exports.push(export);
            let crate::ast::Expr::Elaborator {
                call: crate::ast::ElaboratorCall::FieldAccess { labels, .. },
                ..
            } = cursor.source
            else {
                unreachable!("a projected field header changed expression family")
            };
            let result = crate::pass::typecheck_full::field_access_header_type(
                labels,
                receiver_header.ty(),
                span,
                tcx,
            )?;
            Ok(Some(frontier.store().scoped_type(
                frontier.owner(),
                result,
                span,
            )?))
        }
        super::PlannedOrdinaryValue::UserElaborator(call) => match &mut call.state {
            super::PlannedUserElaboratorCallState::Advancing(call) => {
                reserve_observed_call_header(call, span, exports, frontier, tcx)
            }
            super::PlannedUserElaboratorCallState::Completed { output, .. }
            | super::PlannedUserElaboratorCallState::Ready { output, .. } => {
                Ok(Some(output.clone()))
            }
            super::PlannedUserElaboratorCallState::Fills(state) => {
                reserve_observed_marked_header(state, span, exports, frontier, tcx)
            }
        },
        _ => Ok(None),
    }
}

fn install_observed_call_header<'m>(
    call: &mut super::OrdinaryCallContinuation<'m>,
    source_span: crate::span::Span,
    exports: &mut std::vec::IntoIter<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    if let super::OrdinaryCallContinuation::Successor(successor) = call {
        {
            let mut child_frontier = frontier.ordinary_child_frontier(&mut successor.child);
            install_observed_cursor_header(
                &mut successor.cursor,
                exports,
                &mut child_frontier,
                tcx,
            )?;
        }
        install_observed_header_edge(&mut successor.child, exports, source_span, frontier, tcx)?;
    }
    Ok(())
}

fn install_observed_marked_header<'m>(
    state: &mut MarkedCallState<'m>,
    source_span: crate::span::Span,
    exports: &mut std::vec::IntoIter<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    match &mut state.phase {
        MarkedCallPhase::Project { call, .. } => {
            install_observed_call_header(call, source_span, exports, frontier, tcx)
        }
        MarkedCallPhase::Completed { .. }
        | MarkedCallPhase::AdoptingFills { .. }
        | MarkedCallPhase::Ready { .. } => Ok(()),
    }
}

fn install_observed_cursor_header<'m>(
    cursor: &mut super::OrdinaryValueCursor<'m>,
    exports: &mut std::vec::IntoIter<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let span = cursor.source.span();
    match &mut cursor.planned {
        super::PlannedOrdinaryValue::Call(call) => {
            install_observed_call_header(call, span, exports, frontier, tcx)
        }
        super::PlannedOrdinaryValue::ComputedCall(
            super::ComputedCallContinuation::Application {
                callee_child,
                application,
            },
        ) => {
            {
                let mut callee_frontier = frontier.ordinary_child_frontier(callee_child);
                {
                    let mut application_frontier =
                        callee_frontier.ordinary_child_frontier(&mut application.child);
                    install_observed_cursor_header(
                        &mut application.cursor,
                        exports,
                        &mut application_frontier,
                        tcx,
                    )?;
                }
                install_observed_header_edge(
                    &mut application.child,
                    exports,
                    span,
                    &mut callee_frontier,
                    tcx,
                )?;
            }
            install_observed_header_edge(callee_child, exports, span, frontier, tcx)
        }
        super::PlannedOrdinaryValue::Field(super::FieldContinuation::Receiver(receiver))
            if matches!(
                cursor.source,
                crate::ast::Expr::Elaborator {
                    call: crate::ast::ElaboratorCall::FieldAccess { .. },
                    ..
                }
            ) =>
        {
            {
                let mut receiver_frontier = frontier.ordinary_child_frontier(&mut receiver.child);
                install_observed_cursor_header(
                    &mut receiver.cursor,
                    exports,
                    &mut receiver_frontier,
                    tcx,
                )?;
            }
            install_observed_header_edge(&mut receiver.child, exports, span, frontier, tcx)
        }
        super::PlannedOrdinaryValue::UserElaborator(call) => match &mut call.state {
            super::PlannedUserElaboratorCallState::Advancing(call) => {
                install_observed_call_header(call, span, exports, frontier, tcx)
            }
            super::PlannedUserElaboratorCallState::Completed { .. }
            | super::PlannedUserElaboratorCallState::Ready { .. } => Ok(()),
            super::PlannedUserElaboratorCallState::Fills(state) => {
                install_observed_marked_header(state, span, exports, frontier, tcx)
            }
        },
        _ => Ok(()),
    }
}

pub(super) fn projected_field_header_result<'m>(
    cursor: &mut super::OrdinaryValueCursor<'m>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<Option<crate::pass::typecheck_core::ScopedType>, crate::error::Error> {
    let mut exports = Vec::new();
    let observed = reserve_observed_cursor_header(cursor, &mut exports, frontier, tcx)?;
    let mut exports = exports.into_iter();
    install_observed_cursor_header(cursor, &mut exports, frontier, tcx)?;
    assert!(
        exports.next().is_none(),
        "a field header left an uninstalled owner export"
    );
    Ok(observed)
}

fn install_observed_header_edge<'m>(
    child: &mut super::frontier::OpenRetainedPremise<'m>,
    exports: &mut std::vec::IntoIter<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    span: crate::span::Span,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    if !exports
        .as_slice()
        .first()
        .is_some_and(|export| frontier.parent_header_export_matches(child, export))
    {
        return Ok(());
    }
    let export = exports
        .next()
        .expect("a matching projected header export disappeared");
    frontier.install_parent_header_export(child, export, span, tcx)?;
    Ok(())
}

enum ProjectedLambdaHeader {
    Scoped(crate::pass::typecheck_core::ScopedType),
    Prepared(crate::pass::typecheck_core::PreparedCloseType),
}

fn reserve_header_through_children<'m, F>(
    children: &mut [super::frontier::OpenRetainedPremise<'m>],
    span: crate::span::Span,
    exports: &mut Vec<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    build: &mut Option<F>,
) -> Result<crate::pass::typecheck_core::ScopedType, crate::error::Error>
where
    F: FnOnce(
        &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<ProjectedLambdaHeader, crate::error::Error>,
{
    let (child, rest) = children
        .split_first_mut()
        .expect("a projected lambda header has an owner endpoint");
    let child_header = {
        let mut child_frontier = frontier.ordinary_child_frontier(child);
        if rest.is_empty() {
            build
                .take()
                .expect("a projected lambda header was built twice")(
                &mut child_frontier, tcx
            )?
        } else {
            ProjectedLambdaHeader::Scoped(reserve_header_through_children(
                rest,
                span,
                exports,
                &mut child_frontier,
                tcx,
                build,
            )?)
        }
    };
    let export = match child_header {
        ProjectedLambdaHeader::Scoped(header) => {
            frontier.reserve_parent_header_export(child, header, span)?
        }
        ProjectedLambdaHeader::Prepared(header) => {
            frontier.reserve_prepared_parent_header_export(child, header, span)?
        }
    };
    let header = export.parent_header().clone();
    exports.push(export);
    Ok(header)
}

fn install_header_through_children<'m>(
    children: &mut [super::frontier::OpenRetainedPremise<'m>],
    span: crate::span::Span,
    exports: &mut std::vec::IntoIter<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let (child, rest) = children
        .split_first_mut()
        .expect("a projected lambda header has an owner endpoint");
    if !rest.is_empty() {
        let mut child_frontier = frontier.ordinary_child_frontier(child);
        install_header_through_children(rest, span, exports, &mut child_frontier, tcx)?;
    }
    let export = exports
        .next()
        .expect("a projected lambda owner lost its reserved header export");
    frontier.install_parent_header_export(child, export, span, tcx)?;
    Ok(())
}

fn reserve_lambda_header<'m>(
    lambda: &mut StagedLambda<'m>,
    exports: &mut Vec<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<crate::pass::typecheck_core::ScopedType, crate::error::Error> {
    let source = lambda.source;
    let expected = lambda.expected.clone();
    let selection = &lambda.prepared.selection;
    let value_param_types = &lambda.prepared.value_param_types;
    let body_expected = lambda.prepared.body_expected.clone();
    let retained_binders = &lambda.retained_binders;
    let provisional_body = if !retained_binders.is_empty()
        && matches!(
            selection,
            super::OrdinaryLambdaSelection::Synthesizing {
                annotated_return: None
            }
        ) {
        let mut child_frontier = frontier.ordinary_child_frontier(&mut lambda.premise);
        Some(super::OwnerBoundType::Retained {
            owner: child_frontier.owner(),
            ty: child_frontier.reserve_projected_header_hole(source.span())?,
        })
    } else {
        None
    };
    let mut build = Some(
        |lambda_frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
         tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>|
         -> Result<_, crate::error::Error> {
            if matches!(selection, super::OrdinaryLambdaSelection::Check { .. }) {
                return projected_fallback_header(
                    expected
                        .as_ref()
                        .expect("a checked projected lambda lost its expected header"),
                    source.span(),
                    lambda_frontier,
                )
                .map(ProjectedLambdaHeader::Scoped);
            }
            if let Some(body) = provisional_body {
                let crate::ast::Expr::FnExpr { sig, .. } = source else {
                    unreachable!("a staged lambda changed expression family")
                };
                let mark = tcx.save();
                super::push_ordinary_lambda_scope(sig, value_param_types, retained_binders, tcx);
                let prepared = super::prepare_signature_for_close(
                    sig,
                    body,
                    value_param_types,
                    retained_binders,
                    source.span(),
                    lambda_frontier,
                    tcx,
                );
                tcx.restore(mark);
                return prepared.map(ProjectedLambdaHeader::Prepared);
            }
            let body = match body_expected.clone() {
                Some(body) => body.into_scoped(
                    lambda_frontier.owner(),
                    lambda_frontier.store(),
                    source.span(),
                )?,
                None => lambda_frontier.reserve_projected_header_hole(source.span())?,
            };
            let mut params = Vec::with_capacity(value_param_types.len());
            for param in value_param_types {
                params.push(
                    param
                        .clone()
                        .into_scoped(
                            lambda_frontier.owner(),
                            lambda_frontier.store(),
                            source.span(),
                        )?
                        .into_interned_type(),
                );
            }
            let crate::ast::Expr::FnExpr { sig, .. } = source else {
                unreachable!("a staged lambda changed expression family")
            };
            let ty = super::super::synth::build_fn_type_from_body_type(
                sig,
                body.into_interned_type(),
                params,
                source.span(),
                tcx,
            );
            lambda_frontier
                .store()
                .scoped_type(
                    lambda_frontier.owner(),
                    crate::pass::typecheck_core::InternedType::fresh_canonical(ty),
                    source.span(),
                )
                .map(ProjectedLambdaHeader::Scoped)
        },
    );
    let child_header = {
        let mut child_frontier = frontier.ordinary_child_frontier(&mut lambda.premise);
        reserve_header_through_children(
            &mut lambda.prepared.lambda_owners.owners,
            source.span(),
            exports,
            &mut child_frontier,
            tcx,
            &mut build,
        )?
    };
    let export =
        frontier.reserve_parent_header_export(&lambda.premise, child_header, source.span())?;
    let header = export.parent_header().clone();
    exports.push(export);
    Ok(header)
}

fn reserve_projected_header<'m>(
    operand: &mut StagedOperand<'m>,
    fallback: crate::pass::typecheck_core::ScopedType,
    exports: &mut Vec<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<crate::pass::typecheck_core::ScopedType, crate::error::Error> {
    match operand {
        StagedOperand::Pair(pair) => {
            let pair_header = {
                let mut pair_frontier = frontier.ordinary_child_frontier(&mut pair.premise);
                let [left, right] = &mut pair.operands;
                let left_fallback = projected_fallback_header(
                    &left.expected,
                    staged_operand_span(
                        left.state
                            .as_ref()
                            .expect("a staged pair operand lost its header state"),
                    ),
                    &mut pair_frontier,
                )?;
                let left = reserve_projected_header(
                    left.state
                        .as_mut()
                        .expect("a staged pair operand lost its header state"),
                    left_fallback,
                    exports,
                    &mut pair_frontier,
                    tcx,
                )?;
                let right_fallback = projected_fallback_header(
                    &right.expected,
                    staged_operand_span(
                        right
                            .state
                            .as_ref()
                            .expect("a staged pair operand lost its header state"),
                    ),
                    &mut pair_frontier,
                )?;
                let right = reserve_projected_header(
                    right
                        .state
                        .as_mut()
                        .expect("a staged pair operand lost its header state"),
                    right_fallback,
                    exports,
                    &mut pair_frontier,
                    tcx,
                )?;
                left.projected_binary_peer(
                    &right,
                    crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
                    pair.source.span(),
                )
                .ok_or_else(|| {
                    crate::error::Error::elaborator(
                        pair.source.span(),
                        "a projected pair header crossed incompatible rigid scopes",
                    )
                })?
            };
            let export = frontier.reserve_parent_header_export(
                &pair.premise,
                pair_header,
                pair.source.span(),
            )?;
            let header = export.parent_header().clone();
            exports.push(export);
            Ok(header)
        }
        StagedOperand::Lambda(lambda, _) => reserve_lambda_header(lambda, exports, frontier, tcx),
        StagedOperand::DeferredRecLambda(deferred, _) => {
            let header = reserve_lambda_header(&mut deferred.lambda, exports, frontier, tcx)?;
            // The source expectation can be a bare result goal. Retain the
            // exact parent-local function spine manufactured above so a
            // subsequently lifted recursive body can select checking without
            // rebasing an unresolved goal across owners.
            assert!(
                deferred
                    .public_header
                    .replace(super::OwnerBoundType::Retained {
                        owner: frontier.owner(),
                        ty: header.clone(),
                    })
                    .is_none(),
                "a deferred recursive lambda reserved two public headers"
            );
            Ok(header)
        }
        StagedOperand::Ordinary(continuation, _) => {
            let observed = {
                let mut child_frontier = frontier.ordinary_child_frontier(&mut continuation.child);
                reserve_observed_cursor_header(
                    &mut continuation.cursor,
                    exports,
                    &mut child_frontier,
                    tcx,
                )?
            };
            let Some(observed) = observed else {
                return Ok(fallback);
            };
            let export = frontier.reserve_parent_header_export(
                &continuation.child,
                observed,
                continuation.cursor.source.span(),
            )?;
            let header = export.parent_header().clone();
            exports.push(export);
            Ok(header)
        }
        StagedOperand::Completed { completed, .. } => Ok(completed.scoped().clone()),
        StagedOperand::OpenCompleted { preview, .. } => Ok(preview.clone()),
    }
}

fn install_projected_header_exports<'m>(
    operand: &mut StagedOperand<'m>,
    exports: &mut std::vec::IntoIter<crate::pass::typecheck_core::ReservedParentHeaderExport>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let lambda = match operand {
        StagedOperand::Lambda(lambda, _) => Some(lambda),
        StagedOperand::DeferredRecLambda(deferred, _) => Some(&mut deferred.lambda),
        _ => None,
    };
    if let Some(lambda) = lambda {
        {
            let mut child_frontier = frontier.ordinary_child_frontier(&mut lambda.premise);
            install_header_through_children(
                &mut lambda.prepared.lambda_owners.owners,
                lambda.source.span(),
                exports,
                &mut child_frontier,
                tcx,
            )?;
        }
        let export = exports
            .next()
            .expect("a projected lambda lost its reserved root header export");
        frontier.install_parent_header_export(
            &mut lambda.premise,
            export,
            lambda.source.span(),
            tcx,
        )?;
        return Ok(());
    }
    match operand {
        StagedOperand::Pair(pair) => {
            {
                let mut pair_frontier = frontier.ordinary_child_frontier(&mut pair.premise);
                for operand in &mut pair.operands {
                    install_projected_header_exports(
                        operand
                            .state
                            .as_mut()
                            .expect("a staged pair operand lost its header state"),
                        exports,
                        &mut pair_frontier,
                        tcx,
                    )?;
                }
            }
            let export = exports
                .next()
                .expect("a projected pair wrapper lost its reserved header export");
            frontier.install_parent_header_export(
                &mut pair.premise,
                export,
                pair.source.span(),
                tcx,
            )?;
            Ok(())
        }
        StagedOperand::Lambda(..) | StagedOperand::DeferredRecLambda(..) => {
            unreachable!("a projected lambda skipped its shared header installation")
        }
        StagedOperand::Ordinary(continuation, _) => {
            {
                let mut child_frontier = frontier.ordinary_child_frontier(&mut continuation.child);
                install_observed_cursor_header(
                    &mut continuation.cursor,
                    exports,
                    &mut child_frontier,
                    tcx,
                )?;
            }
            install_observed_header_edge(
                &mut continuation.child,
                exports,
                continuation.cursor.source.span(),
                frontier,
                tcx,
            )
        }
        StagedOperand::Completed { .. } | StagedOperand::OpenCompleted { .. } => Ok(()),
    }
}

fn project_operand_recipe_from_header(
    operand: &StagedOperand<'_>,
    header: crate::pass::typecheck_core::ScopedType,
    seal: &InvocationSeal,
    specialization: &mut crate::pass::typecheck_core::IsolatedSpecializationContextBuilder,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> Result<ProjectedCheckedRecipe, crate::error::Error> {
    match operand {
        StagedOperand::Pair(pair) => {
            let left_header = header
                .projected_structural_child(
                    crate::pass::typecheck_core::ScopedTypeEdge::ProductLeft,
                )
                .ok_or_else(|| {
                    crate::error::Error::elaborator(
                        pair.source.span(),
                        "a certified pair projected a non-product header",
                    )
                })?;
            let right_header = header
                .projected_structural_child(
                    crate::pass::typecheck_core::ScopedTypeEdge::ProductRight,
                )
                .ok_or_else(|| {
                    crate::error::Error::elaborator(
                        pair.source.span(),
                        "a certified pair projected a non-product header",
                    )
                })?;
            let [left, right] = &pair.operands;
            let left = Arc::new(project_operand_recipe_from_header(
                left.state
                    .as_ref()
                    .expect("a staged pair operand lost its header state"),
                left_header,
                seal,
                specialization,
                tcx,
            )?);
            let right = Arc::new(project_operand_recipe_from_header(
                right
                    .state
                    .as_ref()
                    .expect("a staged pair operand lost its header state"),
                right_header,
                seal,
                specialization,
                tcx,
            )?);
            let producers = left.ty.producers.union(&right.ty.producers);
            Ok(ProjectedCheckedRecipe {
                ty: projected_type(seal, header, None, None, producers, specialization, tcx)?,
                node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Pair(left, right)),
            })
        }
        StagedOperand::Lambda(_, leaf)
        | StagedOperand::DeferredRecLambda(_, leaf)
        | StagedOperand::Ordinary(_, leaf)
        | StagedOperand::Completed { leaf, .. }
        | StagedOperand::OpenCompleted { leaf, .. } => {
            let leaf = leaf
                .as_ref()
                .expect("a sealed projected operand lost its recipe leaf");
            projected_recipe_leaf(seal, header, leaf, specialization, tcx)
        }
    }
}

#[cfg(test)]
fn seal_projected_operand_for_test<'m>(
    operand: &mut StagedOperand<'m>,
    fallback: crate::pass::typecheck_core::ScopedType,
    seal: &InvocationSeal,
    specialization: &mut crate::pass::typecheck_core::IsolatedSpecializationContextBuilder,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<ProjectedCheckedRecipe, crate::error::Error> {
    let mut exports = Vec::new();
    let reserved_header =
        reserve_projected_header(operand, fallback.clone(), &mut exports, frontier, tcx)?;
    let mut exports = exports.into_iter();
    install_projected_header_exports(operand, &mut exports, frontier, tcx)?;
    assert!(exports.next().is_none());
    {
        let (store, _owner, delta, _publication) = frontier.parts_mut();
        store.constrain(
            delta,
            fallback.clone(),
            reserved_header.clone(),
            staged_operand_span(operand),
            tcx,
        )?;
    }
    let progressed = {
        let (store, _owner, delta, _publication) = frontier.parts_mut();
        store.zonk_for_progress(delta, reserved_header, tcx)?
    };
    project_operand_recipe_from_header(operand, progressed, seal, specialization, tcx)
}

pub(super) fn projected_call_header_result(
    call: &super::OrdinaryCallContinuation<'_>,
    source_span: crate::span::Span,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, '_>,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> Result<Option<crate::pass::typecheck_core::ScopedType>, crate::error::Error> {
    #[cfg(test)]
    PROJECTED_HEADER_OBSERVER_CALLS.with(|calls| calls.set(calls.get() + 1));
    match call {
        super::OrdinaryCallContinuation::Advancing(advancing) => Ok(Some(advancing.result.clone())),
        super::OrdinaryCallContinuation::AdvancingLayer(layer) => {
            let effective_remaining = super::effective_rec_order_args(&layer.remaining, tcx);
            projected_written_type_tail(
                layer.advancing.result.clone(),
                effective_remaining.as_deref().unwrap_or(&layer.remaining),
                source_span,
                frontier,
                tcx,
            )
        }
        super::OrdinaryCallContinuation::Successor(_) => Ok(None),
        super::OrdinaryCallContinuation::AwaitingExpected(completed)
        | super::OrdinaryCallContinuation::AwaitingReturnedForall { completed, .. } => {
            Ok(Some(completed.result.clone()))
        }
        super::OrdinaryCallContinuation::Selecting(selecting) => {
            let Some(result) = untouched_projected_selecting_result(selecting) else {
                return Ok(None);
            };
            if crate::pass::typecheck_core::type_contains_infer(result.as_type()) {
                return Ok(None);
            }
            let result = frontier
                .store()
                .scoped_type(selecting.call.owner, result, source_span)?;
            let effective_candidates = super::effective_rec_order_args(&selecting.candidates, tcx);
            let candidates = effective_candidates
                .as_deref()
                .unwrap_or(&selecting.candidates);
            if candidates.is_empty() {
                return Ok(Some(result));
            }
            let successor = super::SemanticCallCursor::from_type(result.ty(), tcx);
            if successor.is_none()
                && candidates.len() > 1
                && matches!(result.ty().as_type(), crate::ast::Type::Goal { .. })
            {
                return Ok(None);
            }
            let has_later_type_binders = successor
                .as_ref()
                .is_some_and(|successor| !successor.binders.is_empty());
            let decoded = super::decode_application_packet(
                &[],
                candidates,
                selecting.value_slots,
                has_later_type_binders,
                source_span,
                tcx,
            )?;
            if decoded.selected_source_count == 0 {
                return Ok(None);
            }
            projected_written_type_tail(
                result,
                &candidates[decoded.selected_source_count..],
                source_span,
                frontier,
                tcx,
            )
        }
    }
}

fn untouched_projected_selecting_result(
    selecting: &super::SelectingOrdinaryCall<'_>,
) -> Option<crate::pass::typecheck_core::InternedType<crate::ast::Lowered>> {
    if selecting.next_candidate != 0
        || selecting.selected_slots != 0
        || selecting.selected_expanded_slots != 0
        || !selecting.selected.is_empty()
        || selecting.probe.is_some()
    {
        return None;
    }
    Some(
        if selecting.candidates.is_empty()
            && selecting.call.shape.has_written_type_only()
            && !selecting.call.shape.result_is_pre_residual
        {
            selecting.call.residual_function_type(selecting.call.owner)
        } else {
            selecting.call.result_type(selecting.call.owner)
        },
    )
}

/// Follow only the public type structure already fixed by the retained call.
/// Written values consume known function layers without entering their term
/// bodies. A bare goal or an omitted successor binder stays opaque so the
/// ordinary call frontier remains the only inference scheduler.
fn projected_written_type_tail(
    mut result: crate::pass::typecheck_core::ScopedType,
    mut remaining: &[super::CallArgRef<'_, crate::ast::Lowered>],
    source_span: crate::span::Span,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, '_>,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> Result<Option<crate::pass::typecheck_core::ScopedType>, crate::error::Error> {
    while !remaining.is_empty() {
        result = {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store.zonk_for_progress(delta, result, tcx)?
        };
        if matches!(result.ty().as_type(), crate::ast::Type::Goal { .. }) {
            return Ok(None);
        }
        let Some(cursor) = super::SemanticCallCursor::from_type(result.ty(), tcx) else {
            return Ok(None);
        };
        let selected = cursor.select(remaining, 0, source_span, tcx)?;
        if selected.explicit_prefix != cursor.binders.len() {
            return Ok(None);
        }
        let mut arguments = Vec::with_capacity(selected.explicit_prefix);
        for argument in remaining[..selected.explicit_prefix].iter().copied() {
            let raw = argument.to_type_arg()?;
            if matches!(raw, crate::ast::Type::Infer { .. }) {
                return Ok(None);
            }
            let (canonical, canonical_identity) = super::canonicalize_call_type_arg(&raw, tcx);
            let argument = frontier.store().scoped_type(
                frontier.owner(),
                crate::pass::typecheck_core::InternedType::fresh_with_identity(
                    canonical,
                    canonical_identity,
                ),
                argument.span(),
            )?;
            arguments.push(argument);
        }
        #[cfg(test)]
        PROJECTED_PUBLIC_FORALL_BATCH_CALLS
            .with(|calls| calls.set(calls.get().checked_add(1).expect("batch call overflow")));
        let Some(instantiated) = result.projected_public_forall_prefix_application(&arguments)
        else {
            return Ok(None);
        };
        result = instantiated;
        remaining = &remaining[selected.explicit_prefix..];
        if remaining.is_empty() {
            return Ok(Some(result));
        }
        let successor = cursor.successor(&selected, tcx);
        let decoded = super::decode_application_packet(
            &[],
            remaining,
            selected.width.capacity,
            successor
                .as_ref()
                .is_some_and(|successor| !successor.binders.is_empty()),
            source_span,
            tcx,
        )?;
        if decoded.selected_source_count == 0 {
            return Ok(None);
        }
        let Some(next) = result.projected_structural_child(
            crate::pass::typecheck_core::ScopedTypeEdge::FunctionReturn,
        ) else {
            return Ok(None);
        };
        result = next;
        remaining = &remaining[decoded.selected_source_count..];
    }
    Ok(Some(result))
}

#[cfg(test)]
thread_local! {
    static PROJECTED_PUBLIC_FORALL_BATCH_CALLS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
    static PROJECTED_HEADER_OBSERVER_CALLS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

fn projected_recipe_leaf(
    seal: &InvocationSeal,
    ty: crate::pass::typecheck_core::ScopedType,
    leaf: &ProjectedLeaf,
    specialization: &mut crate::pass::typecheck_core::IsolatedSpecializationContextBuilder,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> Result<ProjectedCheckedRecipe, crate::error::Error> {
    let producers = leaf
        .producer
        .map_or_else(ProducerSet::default, ProducerSet::leaf);
    Ok(ProjectedCheckedRecipe {
        ty: projected_type(seal, ty, None, None, producers, specialization, tcx)?,
        node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TemplateValue(
            leaf.template_index,
        )),
    })
}

fn projected_type(
    seal: &InvocationSeal,
    ty: crate::pass::typecheck_core::ScopedType,
    closed: Option<crate::normalization::ReflectedType>,
    focus: Option<ProjectedFocus>,
    producers: ProducerSet,
    specialization: &mut crate::pass::typecheck_core::IsolatedSpecializationContextBuilder,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
) -> Result<ProjectedType, crate::error::Error> {
    let specialization = specialization.canonicalize_carrier(&ty, tcx)?;
    Ok(ProjectedType {
        seal: seal.clone(),
        ty,
        specialization,
        closed,
        contains_focus: focus.is_some(),
        focus,
        producers,
    })
}

fn snapshot_projected_recipe<'m>(
    recipe: &mut ProjectedCheckedRecipe,
    specialization: &mut crate::pass::typecheck_core::IsolatedSpecializationContextBuilder,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    snapshot_projected_type(&mut recipe.ty, specialization, frontier, tcx)?;
    match crate::normalization::EvalRef::make_mut(&mut recipe.node) {
        ProjectedRecipeNode::Pair(left, right) => {
            snapshot_projected_recipe(Arc::make_mut(left), specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(right), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::ProductProjection {
            product_type,
            product,
            ..
        } => {
            snapshot_projected_type(product_type, specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(product), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::TermCall {
            function_type,
            function,
            argument,
        } => {
            snapshot_projected_type(function_type, specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(function), specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(argument), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::IntrinsicAbsurd { bottom } => {
            snapshot_projected_recipe(Arc::make_mut(bottom), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::IntrinsicEither {
            sum_type,
            value,
            left_body,
            right_body,
            ..
        } => {
            snapshot_projected_type(sum_type, specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(value), specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(left_body), specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(right_body), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::TermLet {
            value_type,
            value,
            body,
            ..
        } => {
            snapshot_projected_type(value_type, specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(value), specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(body), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::TermFn {
            function_type,
            body,
            ..
        } => {
            snapshot_projected_type(function_type, specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(body), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::TermTypeFn { body, .. } => {
            snapshot_projected_recipe(Arc::make_mut(body), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::TermTypeApp { term, .. } => {
            snapshot_projected_recipe(Arc::make_mut(term), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::IntrinsicInjection {
            sum_type, value, ..
        } => {
            snapshot_projected_type(sum_type, specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(value), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::IntrinsicIfThenElse {
            condition,
            true_body,
            false_body,
        } => {
            snapshot_projected_recipe(Arc::make_mut(condition), specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(true_body), specialization, frontier, tcx)?;
            snapshot_projected_recipe(Arc::make_mut(false_body), specialization, frontier, tcx)?;
        }
        ProjectedRecipeNode::Closed(_)
        | ProjectedRecipeNode::TemplateValue(_)
        | ProjectedRecipeNode::Local { .. } => {}
    }
    Ok(())
}

fn snapshot_projected_type<'m>(
    projected: &mut ProjectedType,
    specialization: &mut crate::pass::typecheck_core::IsolatedSpecializationContextBuilder,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let (progressed, closed) = {
        let (store, _owner, delta, _publication) = frontier.parts_mut();
        let progressed = store.zonk_for_progress(delta, projected.ty.clone(), tcx)?;
        specialization.record_carrier_goals(&progressed, store)?;
        let closed = store.try_zonk_goal_free(delta, progressed.clone(), tcx)?;
        (progressed, closed)
    };
    let specialization_ty = specialization.canonicalize_carrier(&progressed, tcx)?;
    let reflected = closed
        .as_ref()
        .map(|closed| crate::pass::typecheck_full::reflect_projected_type(closed.ty(), tcx))
        .transpose()?;
    projected.ty = progressed;
    projected.specialization = specialization_ty;
    projected.closed = reflected;
    Ok(())
}

fn projected_value_group_is_complete(
    param: &crate::ast::Type<crate::ast::Lowered>,
    authored_len: usize,
    abi_arity: usize,
) -> bool {
    let expected_abi_arity = match (authored_len, param) {
        (0, crate::ast::Type::Unit { .. }) | (1, crate::ast::Type::Unit { .. }) => 0,
        (len, _) => len,
    };
    if abi_arity != expected_abi_arity {
        return false;
    }
    if authored_len == 0 {
        return matches!(param, crate::ast::Type::Unit { .. });
    }

    let mut remaining = param;
    for index in 0..authored_len {
        let slot = if index + 1 == authored_len {
            remaining
        } else {
            let crate::ast::Type::Product { left, right, .. } = remaining else {
                return false;
            };
            remaining = right;
            left.as_ref()
        };
        if crate::pass::typecheck_core::type_contains_goal(slot)
            || crate::pass::typecheck_core::type_contains_infer(slot)
        {
            return false;
        }
    }
    true
}

fn lambda_header_leaves_only_open_result(
    sig: &crate::ast::Signature<crate::ast::Lowered>,
    ty: &crate::ast::Type<crate::ast::Lowered>,
) -> bool {
    if crate::pass::typecheck_core::type_contains_infer(ty) {
        return false;
    }

    let mut remaining = ty;
    for group in sig.canonical_group_refs() {
        match group {
            crate::ast::SignatureGroupRef::Type(params) => {
                for source_param in params {
                    let crate::ast::SignatureParam::Type(source_param) = source_param else {
                        unreachable!("a type signature group contains only type parameters")
                    };
                    let crate::ast::Type::Forall { param, body, .. } = remaining else {
                        return false;
                    };
                    if param.effective_kind() != source_param.effective_kind() {
                        return false;
                    }
                    remaining = body;
                }
            }
            crate::ast::SignatureGroupRef::Value(params) => {
                let crate::ast::Type::Function {
                    param,
                    ret,
                    abi_arity,
                    ..
                } = remaining
                else {
                    return false;
                };
                if !projected_value_group_is_complete(param, params.len(), *abi_arity) {
                    return false;
                }
                remaining = ret;
            }
        }
    }
    if !sig.has_value_group() {
        let crate::ast::Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } = remaining
        else {
            return false;
        };
        if !projected_value_group_is_complete(param, 0, *abi_arity) {
            return false;
        }
        remaining = ret;
    }

    crate::pass::typecheck_core::type_contains_goal(remaining)
        && !crate::pass::typecheck_core::type_contains_infer(remaining)
}

fn projected_source_pre_action_error(span: crate::span::Span) -> crate::error::Error {
    crate::error::Error::type_(
        span,
        "this value source needs a complete type before an `impl(fills)` elaborator can run",
    )
    .with_help(
        "add a type argument or annotation, or complete the value in an ordinary `let` first",
    )
}

fn assert_live_projected_leaf(leaf: &ProjectedLeaf, recipe: &ProjectedCheckedRecipe) {
    let producer = leaf
        .producer
        .expect("a producer-bearing projected source lost its producer ordinal");
    let Some(recipe_producers) = recipe.ty.producers.0.as_deref() else {
        unreachable!("a live projected leaf lost its sealed producer evidence")
    };
    let ProducerDag::Leaf(recipe_producer) = recipe_producers else {
        unreachable!("one projected leaf retained a producer union")
    };
    assert_eq!(
        producer, *recipe_producer,
        "a staged projected leaf and its sealed recipe disagree on producer identity"
    );
    let ProjectedRecipeNode::TemplateValue(template) = recipe.node.as_ref() else {
        unreachable!("a staged projected leaf retained a non-template recipe")
    };
    assert_eq!(
        leaf.template_index, *template,
        "a staged projected leaf and its sealed recipe disagree on template identity"
    );
}

fn validate_projected_source_before_action(
    operand: &StagedOperand<'_>,
    recipe: &ProjectedCheckedRecipe,
) -> Result<(), crate::error::Error> {
    if recipe.ty.closed.is_some() {
        return Ok(());
    }

    match operand {
        StagedOperand::Pair(pair) => {
            let ProjectedRecipeNode::Pair(left, right) = recipe.node.as_ref() else {
                unreachable!("a staged pair lost its sealed recipe topology")
            };
            let [left_operand, right_operand] = &pair.operands;
            validate_projected_source_before_action(
                left_operand
                    .state
                    .as_ref()
                    .expect("a staged pair left operand lost its state"),
                left,
            )?;
            validate_projected_source_before_action(
                right_operand
                    .state
                    .as_ref()
                    .expect("a staged pair right operand lost its state"),
                right,
            )
        }
        StagedOperand::Completed { source, leaf, .. }
        | StagedOperand::OpenCompleted { source, leaf, .. } => {
            if !recipe.ty.producers.is_empty() {
                let leaf = leaf
                    .as_ref()
                    .expect("a producer-bearing completed source lost its projected leaf");
                assert_live_projected_leaf(leaf, recipe);
            }
            Err(projected_source_pre_action_error(source.span()))
        }
        StagedOperand::Ordinary(continuation, leaf) => {
            let leaf = leaf
                .as_ref()
                .expect("a producer-bearing ordinary source lost its projected leaf");
            assert_live_projected_leaf(leaf, recipe);
            Err(projected_source_pre_action_error(
                continuation.cursor.source.span(),
            ))
        }
        StagedOperand::Lambda(lambda, leaf) => {
            validate_projected_lambda_before_action(lambda, leaf, recipe)
        }
        StagedOperand::DeferredRecLambda(deferred, leaf) => {
            validate_projected_lambda_before_action(&deferred.lambda, leaf, recipe)
        }
    }
}

fn validate_projected_lambda_before_action(
    lambda: &StagedLambda<'_>,
    leaf: &Option<ProjectedLeaf>,
    recipe: &ProjectedCheckedRecipe,
) -> Result<(), crate::error::Error> {
    let leaf = leaf
        .as_ref()
        .expect("a producer-bearing lambda source lost its projected leaf");
    assert_live_projected_leaf(leaf, recipe);
    let permitted_selection = matches!(
        lambda.prepared.selection,
        super::OrdinaryLambdaSelection::Check { .. }
            | super::OrdinaryLambdaSelection::Synthesizing {
                annotated_return: None
            }
    );
    let crate::ast::Expr::FnExpr { sig, .. } = lambda.source else {
        unreachable!("a staged lambda changed expression family")
    };
    if permitted_selection
        && lambda_header_leaves_only_open_result(sig, recipe.ty.specialization.ty().as_type())
    {
        Ok(())
    } else {
        Err(projected_source_pre_action_error(lambda.source.span()))
    }
}

fn split_projected_recipe(
    recipe: ProjectedCheckedRecipe,
    width: usize,
) -> Result<Vec<ProjectedCheckedRecipe>, crate::error::Error> {
    assert!(width != 0, "a projected source has at least one ABI slot");
    if width == 1 {
        return Ok(vec![recipe]);
    }
    let left = project_recipe_product_child(&recipe, true)?;
    let right = project_recipe_product_child(&recipe, false)?;
    let mut pieces = Vec::with_capacity(width);
    pieces.push(left);
    pieces.extend(split_projected_recipe(right, width - 1)?);
    Ok(pieces)
}

fn project_recipe_product_child(
    recipe: &ProjectedCheckedRecipe,
    left: bool,
) -> Result<ProjectedCheckedRecipe, crate::error::Error> {
    if let ProjectedRecipeNode::Pair(first, second) = recipe.node.as_ref() {
        return Ok(if left {
            first.as_ref().clone()
        } else {
            second.as_ref().clone()
        });
    }
    let edge = if left {
        ProjectedTypeEdge::ProductLeft
    } else {
        ProjectedTypeEdge::ProductRight
    };
    let product_type = recipe.ty.clone();
    let mut ty = product_type.structural_child(edge).ok_or_else(|| {
        crate::error::Error::elaborator(
            recipe.ty.ty.ty().span(),
            "a projected source width exceeded its exact product spine",
        )
    })?;
    ty.producers = ty.producers.union(&product_type.producers);
    Ok(ProjectedCheckedRecipe {
        ty,
        node: crate::normalization::EvalRef::new(ProjectedRecipeNode::ProductProjection {
            product_type,
            product: Arc::new(recipe.clone()),
            left,
        }),
    })
}

fn fold_projected_recipe_packet(
    mut pieces: Vec<ProjectedCheckedRecipe>,
) -> Result<ProjectedCheckedRecipe, crate::error::Error> {
    let mut packet = pieces
        .pop()
        .expect("a declared projected value slot has nonzero ABI width");
    while let Some(left) = pieces.pop() {
        if !left.ty.seal.matches(&packet.ty.seal) {
            return Err(crate::error::Error::elaborator(
                left.ty.ty.ty().span(),
                "a projected ABI packet crossed invocation ownership",
            ));
        }
        let span = left.ty.ty.ty().span();
        let ty = left
            .ty
            .ty
            .projected_binary_peer(
                &packet.ty.ty,
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
                span,
            )
            .ok_or_else(|| {
                crate::error::Error::elaborator(
                    span,
                    "a projected ABI packet combined incompatible rigid scopes",
                )
            })?;
        let specialization = left
            .ty
            .specialization
            .projected_binary_peer(
                &packet.ty.specialization,
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
                span,
            )
            .expect("an exact projected ABI packet lost its canonical peer scope");
        let producers = left.ty.producers.union(&packet.ty.producers);
        let closed = match (&left.ty.closed, &packet.ty.closed) {
            (Some(left), Some(right)) => crate::normalization::ReflectedType::new(
                crate::ast::Type::Product {
                    left: Box::new(left.as_type().clone()),
                    right: Box::new(right.as_type().clone()),
                    meta: crate::ast::Meta::new(span),
                },
                "a closed projected ABI packet must remain infer-free",
            )
            .ok(),
            _ => None,
        };
        packet = ProjectedCheckedRecipe {
            ty: ProjectedType {
                seal: left.ty.seal.clone(),
                ty,
                specialization,
                closed,
                focus: None,
                contains_focus: left.ty.contains_focus || packet.ty.contains_focus,
                producers,
            },
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Pair(
                Arc::new(left),
                Arc::new(packet),
            )),
        };
    }
    Ok(packet)
}

fn reserve_projected_source(
    root: &mut StagedOperand<'_>,
    source: usize,
    next_template: &mut u32,
    next_producer: &mut u32,
    locations: &mut Vec<ProducerLocation>,
) {
    fn reserve(
        operand: &mut StagedOperand<'_>,
        source: usize,
        path: &mut Vec<u8>,
        next_template: &mut u32,
        next_producer: &mut u32,
        locations: &mut Vec<ProducerLocation>,
    ) {
        match operand {
            StagedOperand::Pair(pair) => {
                for (index, operand) in pair.operands.iter_mut().enumerate() {
                    path.push(index as u8);
                    reserve(
                        operand
                            .state
                            .as_mut()
                            .expect("a staged pair operand lost its state"),
                        source,
                        path,
                        next_template,
                        next_producer,
                        locations,
                    );
                    path.pop();
                }
            }
            StagedOperand::Lambda(_, leaf)
            | StagedOperand::DeferredRecLambda(_, leaf)
            | StagedOperand::Ordinary(_, leaf) => {
                assert_eq!(locations.len(), *next_producer as usize);
                locations.push(ProducerLocation {
                    source,
                    path: path.clone().into(),
                });
                *next_producer = next_producer
                    .checked_add(1)
                    .expect("fills producer count exceeded u32");
                *leaf = Some(ProjectedLeaf {
                    template_index: *next_template,
                    producer: Some(*next_producer - 1),
                });
            }
            StagedOperand::Completed { leaf, .. } | StagedOperand::OpenCompleted { leaf, .. } => {
                *leaf = Some(ProjectedLeaf {
                    template_index: *next_template,
                    producer: None,
                });
            }
        }
        if !matches!(operand, StagedOperand::Pair(_)) {
            *next_template = next_template
                .checked_add(1)
                .expect("fills template leaf count exceeded u32");
        }
    }

    let mut path = Vec::new();
    reserve(
        root,
        source,
        &mut path,
        next_template,
        next_producer,
        locations,
    );
}

#[cfg(test)]
fn record_projected_source_observation(
    root: &StagedOperand<'_>,
    module_path: &str,
    span: crate::span::Span,
) {
    if !PROJECTED_SOURCE_OBSERVATIONS.with(|observations| {
        observations
            .borrow()
            .as_ref()
            .is_some_and(|(target_module, target_span, _)| {
                target_module == module_path && *target_span == span
            })
    }) {
        return;
    }

    fn collect(
        operand: &StagedOperand<'_>,
        path: &mut Vec<u8>,
        leaves: &mut Vec<ProjectedLeafObservation>,
    ) {
        match operand {
            StagedOperand::Pair(pair) => {
                for (index, operand) in pair.operands.iter().enumerate() {
                    path.push(index as u8);
                    collect(
                        operand
                            .state
                            .as_ref()
                            .expect("an observed staged pair operand lost its state"),
                        path,
                        leaves,
                    );
                    path.pop();
                }
            }
            StagedOperand::Lambda(_, leaf)
            | StagedOperand::DeferredRecLambda(_, leaf)
            | StagedOperand::Completed { leaf, .. }
            | StagedOperand::OpenCompleted { leaf, .. }
            | StagedOperand::Ordinary(_, leaf) => {
                let leaf = leaf
                    .as_ref()
                    .expect("an observed projected leaf was not reserved");
                leaves.push(ProjectedLeafObservation {
                    path: path.clone(),
                    template_index: leaf.template_index,
                    producer: leaf.producer,
                });
            }
        }
    }

    let mut leaves = Vec::new();
    collect(root, &mut Vec::new(), &mut leaves);
    PROJECTED_SOURCE_OBSERVATIONS.with(|observations| {
        let mut observations = observations.borrow_mut();
        let Some((_, _, observations)) = observations.as_mut() else {
            return;
        };
        observations.push(leaves);
    });
}

fn stage_pair_wrapper<'m>(
    wrapper: super::OrdinaryChildContinuation<'m>,
    operands: [&'m crate::ast::Expr<crate::ast::Lowered>; 2],
    frontier: &mut impl super::OrdinaryFrontierReentry<'m>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<StagedPair<'m>, crate::error::Error> {
    let super::OrdinaryChildContinuation {
        child: mut premise,
        cursor,
        next_mode,
        suspension,
    } = wrapper;
    assert!(
        matches!(suspension, super::OrdinarySuspension::Mode),
        "an unstaged pair cannot already wait for completion"
    );
    assert_eq!(
        next_mode,
        super::frontier::FrontierMode::ExpectedOnly,
        "a freshly staged canonical pair must start at ExpectedOnly"
    );
    let super::OrdinaryValueCursor {
        source,
        expected,
        planned,
    } = *cursor;
    let super::PlannedOrdinaryValue::Call(super::OrdinaryCallContinuation::Selecting(
        mut selecting,
    )) = planned
    else {
        unreachable!("a certified pair lost its planned direct wrapper")
    };
    let mut staged = Vec::with_capacity(2);
    {
        let mut pair_frontier = frontier.ordinary_child_frontier(&mut premise);
        if let Some(expected) = expected.as_ref() {
            let owner = pair_frontier.owner();
            let result = selecting.call.result_type(pair_frontier.goal_owner());
            let result =
                super::prepare_owner_bound_type(&mut pair_frontier, result, source.span(), tcx)?
                    .into_scoped(owner, pair_frontier.store(), source.span())?;
            let _ = super::try_apply_expected_equation(
                &mut pair_frontier,
                result,
                expected,
                source.span(),
                tcx,
            )?;
        }
        for (index, operand) in operands.into_iter().enumerate() {
            let expanded_start = selecting.selected_expanded_slots;
            let (operand_expected, selected_slots, selected_expanded_slots) =
                super::OrdinaryValueCursor::selected_source_expectation(
                    &mut selecting,
                    operand,
                    1,
                    index == 1,
                    &mut pair_frontier,
                    tcx,
                )?;
            let state = stage_operand(operand, operand_expected.clone(), &mut pair_frontier, tcx)?;
            staged.push(StagedPairOperand {
                expected: operand_expected,
                expanded_start,
                expanded_width: selected_expanded_slots - expanded_start,
                state: Some(state),
            });
            selecting.selected_slots = selected_slots;
            selecting.selected_expanded_slots = selected_expanded_slots;
            selecting.next_candidate += 1;
        }
    }
    Ok(StagedPair {
        premise,
        source,
        expected,
        selecting,
        operands: staged
            .try_into()
            .unwrap_or_else(|_| unreachable!("a certified pair has exactly two operands")),
    })
}

fn stage_operand<'m>(
    source: &'m crate::ast::Expr<crate::ast::Lowered>,
    expected: super::OwnerBoundType,
    frontier: &mut impl super::OrdinaryFrontierReentry<'m>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<StagedOperand<'m>, crate::error::Error> {
    let continuation =
        super::OrdinaryValueCursor::begin_child(source, Some(expected), frontier, tcx)?;
    stage_existing_operand(continuation, frontier, tcx)
}

fn stage_existing_operand<'m>(
    continuation: super::OrdinaryChildContinuation<'m>,
    frontier: &mut impl super::OrdinaryFrontierReentry<'m>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<StagedOperand<'m>, crate::error::Error> {
    if let Some(operands) = certified_pair_operands(&continuation, tcx) {
        return stage_pair_wrapper(continuation, operands, frontier, tcx)
            .map(Box::new)
            .map(StagedOperand::Pair);
    }
    let super::OrdinaryChildContinuation {
        child: mut premise,
        cursor,
        next_mode,
        suspension,
    } = continuation;
    let super::OrdinaryValueCursor {
        source,
        expected,
        planned,
    } = *cursor;
    let super::PlannedOrdinaryLambda {
        mut header,
        mut retained_binders,
        state,
    } = match planned {
        super::PlannedOrdinaryValue::Lambda(lambda) => lambda,
        planned => {
            let header_complete = matches!(
                planned,
                super::PlannedOrdinaryValue::Leaf(_) | super::PlannedOrdinaryValue::Literal
            ) || matches!(source,
                crate::ast::Expr::RecQuote { plan, .. }
                    if matches!(plan.as_ref(), crate::ast::RecQuotePlan::Operand { public_ty: Some(public_ty), .. }
                        if !crate::pass::typecheck_core::type_contains_goal(public_ty)
                            && !crate::pass::typecheck_core::type_contains_infer(public_ty))
            );
            let continuation = super::OrdinaryChildContinuation {
                child: premise,
                cursor: Box::new(super::OrdinaryValueCursor {
                    source,
                    expected,
                    planned,
                }),
                next_mode,
                suspension,
            };
            return if header_complete {
                stage_header_complete_operand(continuation, frontier, tcx)
            } else {
                Ok(StagedOperand::Ordinary(continuation, None))
            };
        }
    };
    assert!(
        matches!(state, super::OrdinaryLambdaState::Unselected),
        "a freshly planned fills operand lambda already entered its body"
    );
    let crate::ast::Expr::FnExpr { sig, .. } = source else {
        unreachable!("a planned lambda retained another expression family")
    };
    let ambient_binders = tcx.in_scope_type_param_presentation();
    let mut child_frontier = frontier.ordinary_child_frontier(&mut premise);
    let Some((mut selection, mut value_param_types)) = super::select_ordinary_lambda(
        source,
        &mut header,
        expected.as_ref(),
        super::frontier::FrontierMode::ExpectedOnly,
        &mut child_frontier,
        tcx,
    )?
    else {
        return Ok(StagedOperand::Ordinary(
            super::OrdinaryChildContinuation {
                child: premise,
                cursor: Box::new(super::OrdinaryValueCursor {
                    source,
                    expected,
                    planned: super::PlannedOrdinaryValue::Lambda(super::PlannedOrdinaryLambda {
                        header,
                        retained_binders,
                        state: super::OrdinaryLambdaState::Unselected,
                    }),
                }),
                next_mode,
                suspension,
            },
            None,
        ));
    };
    if retained_binders.is_empty() {
        retained_binders = super::OrdinaryValueCursor::retain_lambda_binders(sig, tcx);
    }
    if matches!(selection, super::OrdinaryLambdaSelection::Check { .. }) {
        let (body_type, selected) = header.bind_selected_alpha_edges(&retained_binders);
        selection = super::OrdinaryLambdaSelection::Check {
            body_type: super::OwnerBoundType::Plain(body_type),
        };
        value_param_types = selected;
    }
    let owners = super::OrdinaryValueCursor::reserve_lambda_owner_chain(
        source.span(),
        sig,
        &mut header,
        &retained_binders,
        &mut child_frontier,
        tcx,
    )?;
    let prepared = super::OrdinaryValueCursor::prepare_reserved_lambda_body(
        source,
        owners,
        &mut header,
        selection,
        value_param_types
            .into_iter()
            .map(super::OwnerBoundType::Plain)
            .collect(),
        &retained_binders,
        ambient_binders,
        &mut child_frontier,
        tcx,
    )?;
    Ok(staged_lambda_operand(
        StagedLambda {
            source,
            expected,
            premise,
            header,
            retained_binders,
            prepared,
            header_mode: super::frontier::FrontierMode::ExpectedOnly,
            body_next_mode: super::frontier::FrontierMode::ExpectedOnly,
        },
        None,
    ))
}

fn prepare_staged_pair_headers<'m>(
    pair: &mut StagedPair<'m>,
    mode: super::frontier::FrontierMode,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let mut pair_frontier = frontier.ordinary_child_frontier(&mut pair.premise);
    for operand in &mut pair.operands {
        let state = operand
            .state
            .take()
            .expect("a staged pair operand lost its header state");
        operand.state = Some(prepare_projected_source_header(
            state,
            mode,
            &mut pair_frontier,
            tcx,
        )?);
    }
    Ok(())
}

fn prepare_projected_source_header<'m>(
    state: StagedOperand<'m>,
    mode: super::frontier::FrontierMode,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<StagedOperand<'m>, crate::error::Error> {
    isolate_staged_producer(&state, frontier, tcx)?;
    let StagedOperand::Ordinary(mut continuation, leaf) = state else {
        return prepare_staged_header(state, mode, frontier, tcx);
    };
    if !certified_scalar_literal_call_at_lexical_fallback(&mut continuation, frontier, tcx)?
        && matches!(
            continuation.cursor.planned,
            super::PlannedOrdinaryValue::Call(_)
                | super::PlannedOrdinaryValue::ComputedCall(_)
                | super::PlannedOrdinaryValue::Recursive(
                    super::OrdinaryRecursiveFamily::ComputedCall
                )
                | super::PlannedOrdinaryValue::Field(_)
                | super::PlannedOrdinaryValue::Recursive(super::OrdinaryRecursiveFamily::Field)
        )
    {
        assert!(
            leaf.as_ref().is_some_and(|leaf| leaf.producer.is_some()),
            "a public call header advanced before producer reservation"
        );
        let source = continuation.cursor.source;
        match super::OrdinaryValueCursor::advance_open_child_iterative(
            continuation,
            super::frontier::FrontierMode::ExpectedOnly,
            super::OrdinaryCompletionDemand::PublicHeader,
            false,
            frontier,
            tcx,
        )? {
            super::OpenOrdinaryChildAdvance::Pending(pending) => {
                continuation = pending;
            }
            super::OpenOrdinaryChildAdvance::Complete { child, completed } => {
                let preview = child.preview_goal_free_close_output(
                    frontier.store(),
                    completed.scoped().clone(),
                    tcx,
                )?;
                return Ok(StagedOperand::OpenCompleted {
                    source,
                    child,
                    completed,
                    preview,
                    accepted: false,
                    #[cfg(test)]
                    accepted_by_relation_wave: false,
                    leaf,
                });
            }
        }
    }
    if !certified_scalar_literal_call_at_lexical_fallback(&mut continuation, frontier, tcx)? {
        return prepare_staged_header(
            StagedOperand::Ordinary(continuation, leaf),
            mode,
            frontier,
            tcx,
        );
    }
    assert_eq!(
        mode,
        super::frontier::FrontierMode::LexicalFallback,
        "a projected scalar call advanced outside its fixed lexical pass"
    );
    assert!(
        leaf.as_ref().is_some_and(|leaf| leaf.producer.is_some()),
        "a projected scalar call reached header preparation before producer reservation"
    );
    advance_staged_producer(
        StagedOperand::Ordinary(continuation, leaf),
        mode,
        false,
        frontier,
        tcx,
    )
    .map(|(state, complete)| {
        assert!(
            if complete {
                matches!(&state, StagedOperand::OpenCompleted { .. })
            } else {
                matches!(
                    &state,
                    StagedOperand::Ordinary(
                        super::OrdinaryChildContinuation {
                            next_mode: super::frontier::FrontierMode::FinalPreflight,
                            ..
                        },
                        _
                    )
                )
            },
            "a projected scalar call left its bounded lexical header state"
        );
        state
    })
}

fn prepare_staged_header<'m>(
    state: StagedOperand<'m>,
    mode: super::frontier::FrontierMode,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<StagedOperand<'m>, crate::error::Error> {
    match state {
        StagedOperand::Pair(mut pair) => {
            prepare_staged_pair_headers(&mut pair, mode, frontier, tcx)?;
            Ok(StagedOperand::Pair(pair))
        }
        StagedOperand::Ordinary(continuation, leaf)
            if matches!(
                &continuation.cursor.planned,
                super::PlannedOrdinaryValue::Literal
            ) =>
        {
            assert_eq!(
                continuation.next_mode, mode,
                "a projected literal skipped its lexical header pass"
            );
            advance_staged_producer(
                StagedOperand::Ordinary(continuation, leaf),
                mode,
                false,
                frontier,
                tcx,
            )
            .map(|(state, complete)| {
                assert!(
                    complete && matches!(state, StagedOperand::OpenCompleted { .. }),
                    "a projected literal remained open after its lexical header pass"
                );
                state
            })
        }
        StagedOperand::Ordinary(continuation, leaf) => {
            prepare_staged_lambda_header(continuation, leaf, mode, frontier, tcx)
        }
        state @ (StagedOperand::Lambda(..)
        | StagedOperand::DeferredRecLambda(..)
        | StagedOperand::Completed { .. }
        | StagedOperand::OpenCompleted { .. }) => Ok(state),
    }
}

fn has_unselected_projected_lambda(state: &StagedOperand<'_>) -> bool {
    match state {
        StagedOperand::Pair(pair) => pair.operands.iter().any(|operand| {
            has_unselected_projected_lambda(
                operand
                    .state
                    .as_ref()
                    .expect("a staged pair lost its operand"),
            )
        }),
        StagedOperand::Ordinary(continuation, _) => matches!(
            continuation.cursor.planned,
            super::PlannedOrdinaryValue::Lambda(super::PlannedOrdinaryLambda {
                state: super::OrdinaryLambdaState::Unselected,
                ..
            })
        ),
        _ => false,
    }
}

fn refresh_linked_lambda_context<'m>(
    state: &StagedOperand<'m>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    if let StagedOperand::Pair(pair) = state {
        for operand in &pair.operands {
            refresh_linked_lambda_context(
                operand
                    .state
                    .as_ref()
                    .expect("a staged pair lost its operand"),
                frontier,
                tcx,
            )?;
        }
    } else if has_unselected_projected_lambda(state) {
        isolate_staged_producer(state, frontier, tcx)?;
    }
    Ok(())
}

fn select_linked_lambda_headers<'m>(
    state: StagedOperand<'m>,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<StagedOperand<'m>, crate::error::Error> {
    if !has_unselected_projected_lambda(&state) {
        return Ok(state);
    }
    refresh_linked_lambda_context(&state, frontier, tcx)?;
    match state {
        StagedOperand::Pair(mut pair) => {
            let mut pair_frontier = frontier.ordinary_child_frontier(&mut pair.premise);
            for operand in &mut pair.operands {
                let state = operand
                    .state
                    .take()
                    .expect("a staged pair lost its operand");
                operand.state = Some(select_linked_lambda_headers(
                    state,
                    &mut pair_frontier,
                    tcx,
                )?);
            }
            Ok(StagedOperand::Pair(pair))
        }
        StagedOperand::Ordinary(continuation, leaf) => {
            let mut state = prepare_staged_lambda_header(
                continuation,
                leaf,
                super::frontier::FrontierMode::ExpectedOnly,
                frontier,
                tcx,
            )?;
            if let StagedOperand::DeferredRecLambda(deferred, _) = &mut state {
                deferred.public_header = deferred.lambda.expected.clone();
            }
            Ok(state)
        }
        _ => unreachable!("linked header selection crossed a selected source"),
    }
}

fn prepare_staged_lambda_header<'m>(
    continuation: super::OrdinaryChildContinuation<'m>,
    leaf: Option<ProjectedLeaf>,
    mode: super::frontier::FrontierMode,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<StagedOperand<'m>, crate::error::Error> {
    let super::OrdinaryChildContinuation {
        child: mut premise,
        cursor,
        next_mode,
        suspension,
    } = continuation;
    let super::OrdinaryValueCursor {
        source,
        expected,
        planned,
    } = *cursor;
    let super::PlannedOrdinaryValue::Lambda(super::PlannedOrdinaryLambda {
        mut header,
        mut retained_binders,
        state: super::OrdinaryLambdaState::Unselected,
    }) = planned
    else {
        return Ok(StagedOperand::Ordinary(
            super::OrdinaryChildContinuation {
                child: premise,
                cursor: Box::new(super::OrdinaryValueCursor {
                    source,
                    expected,
                    planned,
                }),
                next_mode,
                suspension,
            },
            leaf,
        ));
    };
    match mode {
        super::frontier::FrontierMode::LexicalFallback => assert_eq!(
            next_mode,
            super::frontier::FrontierMode::ExpectedOnly,
            "a projected lambda skipped or repeated its expectation-only header pass"
        ),
        super::frontier::FrontierMode::ExpectedOnly => assert_eq!(
            next_mode,
            super::frontier::FrontierMode::FinalPreflight,
            "a linked lambda selection did not follow its one lexical header pass"
        ),
        _ => unreachable!("projected header selection entered a body-completion mode"),
    }
    let crate::ast::Expr::FnExpr { sig, .. } = source else {
        unreachable!("a planned lambda retained another expression family")
    };
    let mut child_frontier = frontier.ordinary_child_frontier(&mut premise);
    let Some((mut selection, mut value_param_types)) = super::select_ordinary_lambda(
        source,
        &mut header,
        expected.as_ref(),
        mode,
        &mut child_frontier,
        tcx,
    )?
    else {
        return Ok(StagedOperand::Ordinary(
            super::OrdinaryChildContinuation {
                child: premise,
                cursor: Box::new(super::OrdinaryValueCursor {
                    source,
                    expected,
                    planned: super::PlannedOrdinaryValue::Lambda(super::PlannedOrdinaryLambda {
                        header,
                        retained_binders,
                        state: super::OrdinaryLambdaState::Unselected,
                    }),
                }),
                next_mode: if mode == super::frontier::FrontierMode::ExpectedOnly {
                    next_mode
                } else {
                    super::frontier::FrontierMode::FinalPreflight
                },
                suspension: if mode == super::frontier::FrontierMode::ExpectedOnly {
                    suspension
                } else {
                    super::OrdinarySuspension::Mode
                },
            },
            leaf,
        ));
    };
    if mode == super::frontier::FrontierMode::ExpectedOnly {
        assert!(
            matches!(selection, super::OrdinaryLambdaSelection::Check { .. }),
            "linked public headers must select checking without synthesis"
        );
    }
    let ambient_binders = tcx.in_scope_type_param_presentation();
    if retained_binders.is_empty() {
        retained_binders = super::OrdinaryValueCursor::retain_lambda_binders(sig, tcx);
    }
    if matches!(selection, super::OrdinaryLambdaSelection::Check { .. }) {
        let (body_type, selected) = header.bind_selected_alpha_edges(&retained_binders);
        selection = super::OrdinaryLambdaSelection::Check {
            body_type: super::OwnerBoundType::Plain(body_type),
        };
        value_param_types = selected;
    }
    let owners = super::OrdinaryValueCursor::reserve_lambda_owner_chain(
        source.span(),
        sig,
        &mut header,
        &retained_binders,
        &mut child_frontier,
        tcx,
    )?;
    let prepared = super::OrdinaryValueCursor::prepare_reserved_lambda_body(
        source,
        owners,
        &mut header,
        selection,
        value_param_types
            .into_iter()
            .map(super::OwnerBoundType::Plain)
            .collect(),
        &retained_binders,
        ambient_binders,
        &mut child_frontier,
        tcx,
    )?;
    Ok(staged_lambda_operand(
        StagedLambda {
            source,
            expected,
            premise,
            header,
            retained_binders,
            prepared,
            header_mode: mode,
            body_next_mode: super::frontier::FrontierMode::ExpectedOnly,
        },
        leaf,
    ))
}

fn stage_header_complete_operand<'m>(
    continuation: super::OrdinaryChildContinuation<'m>,
    frontier: &mut impl super::OrdinaryFrontierReentry<'m>,
    tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<StagedOperand<'m>, crate::error::Error> {
    let next_mode = continuation.next_mode;
    assert_eq!(
        next_mode,
        super::frontier::FrontierMode::ExpectedOnly,
        "a freshly staged nonrecursive operand must start at ExpectedOnly"
    );
    let source = continuation.cursor.source;
    match super::OrdinaryValueCursor::advance_open_child_iterative(
        continuation,
        next_mode,
        super::OrdinaryCompletionDemand::GoalFreeValue,
        false,
        frontier,
        tcx,
    )? {
        super::OpenOrdinaryChildAdvance::Pending(continuation) => {
            assert_eq!(
                continuation.next_mode,
                super::frontier::FrontierMode::LexicalFallback,
                "a staged header-complete operand skipped its lexical fallback boundary"
            );
            Ok(StagedOperand::Ordinary(continuation, None))
        }
        super::OpenOrdinaryChildAdvance::Complete { child, completed } => {
            let destination = frontier.owner();
            let completed = super::close_completed_ordinary_child(
                frontier,
                child,
                completed,
                destination,
                tcx,
            )?;
            Ok(StagedOperand::Completed {
                source,
                completed,
                leaf: None,
            })
        }
    }
}

struct InvocationEvidence {
    specialization:
        std::sync::OnceLock<Arc<crate::pass::typecheck_core::IsolatedSpecializationContext>>,
}

#[derive(Clone)]
pub(crate) struct InvocationSeal(Arc<InvocationEvidence>);

impl std::fmt::Debug for InvocationSeal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InvocationSeal(..)")
    }
}

impl InvocationSeal {
    fn fresh() -> Self {
        Self(Arc::new(InvocationEvidence {
            specialization: std::sync::OnceLock::new(),
        }))
    }

    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    fn install_specialization_context(
        &self,
        context: Arc<crate::pass::typecheck_core::IsolatedSpecializationContext>,
    ) {
        assert!(
            self.0.specialization.set(context).is_ok(),
            "one projected invocation installed specialization evidence twice"
        );
    }

    fn specialization_context(
        &self,
    ) -> &Arc<crate::pass::typecheck_core::IsolatedSpecializationContext> {
        self.0
            .specialization
            .get()
            .expect("a projected value escaped before specialization evidence was sealed")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectedHelperClass {
    ContextInsensitive,
    FocusStructural,
    GoalFreeOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ProjectedTypeEdge {
    ProductLeft,
    ProductRight,
    SumLeft,
    SumRight,
    FunctionParam,
    FunctionReturn,
    ForallBody,
    PathArgument(usize),
}

/// A dense identity minted by one marked projection builder. This is not a
/// source position or a declaration slot; the invocation seal authenticates
/// the table in which it has meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ProjectedResultRoot(u32);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProjectedFocus {
    result_root: ProjectedResultRoot,
    path: Arc<[ProjectedTypeEdge]>,
}

#[derive(Clone, Debug)]
pub(crate) enum ProducerDag {
    Leaf(u32),
    Union(
        crate::normalization::EvalRef<ProducerDag>,
        crate::normalization::EvalRef<ProducerDag>,
    ),
}

impl crate::normalization::ownership::Reclaim for ProducerDag {
    fn reclaim(self, pending: &mut Vec<crate::normalization::ownership::Node>) {
        if let Self::Union(left, right) = self {
            crate::normalization::ownership::shared(left, pending);
            crate::normalization::ownership::shared(right, pending);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProducerSet(Option<crate::normalization::EvalRef<ProducerDag>>);

impl ProducerSet {
    fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    fn leaf(ordinal: u32) -> Self {
        Self(Some(crate::normalization::EvalRef::new(ProducerDag::Leaf(
            ordinal,
        ))))
    }

    fn union(&self, other: &Self) -> Self {
        match (&self.0, &other.0) {
            (None, _) => other.clone(),
            (_, None) => self.clone(),
            (Some(left), Some(right)) if crate::normalization::EvalRef::ptr_eq(left, right) => {
                self.clone()
            }
            (Some(left), Some(right)) => Self(Some(crate::normalization::EvalRef::new(
                ProducerDag::Union(left.clone(), right.clone()),
            ))),
        }
    }

    fn same_root(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(left), Some(right)) => crate::normalization::EvalRef::ptr_eq(left, right),
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectedType {
    seal: InvocationSeal,
    /// Exact snapshotted type retained for fill and whole-handle reflection.
    ty: crate::pass::typecheck_core::ScopedType,
    /// Authenticated alias-unfolded carrier used by structural operations.
    specialization: crate::pass::typecheck_core::ScopedType,
    closed: Option<crate::normalization::ReflectedType>,
    focus: Option<ProjectedFocus>,
    /// Whether any exact component descends from an authenticated result
    /// focus. A structural constructor can combine several focuses without
    /// manufacturing a single destination key.
    contains_focus: bool,
    producers: ProducerSet,
}

impl ProjectedType {
    pub(crate) fn matches_proof(&self, proof: &MarkedComptimeProof) -> bool {
        self.seal.matches(&proof.seal)
    }

    pub(crate) fn closed_reflected(&self) -> Option<&crate::normalization::ReflectedType> {
        self.closed.as_ref()
    }

    fn specialization_root_is_unresolved(&self) -> bool {
        matches!(
            self.specialization.ty().as_type(),
            crate::ast::Type::Goal { .. } | crate::ast::Type::Infer { .. }
        )
    }

    pub(crate) fn same_snapshot(&self, other: &Self) -> bool {
        self.seal.matches(&other.seal)
            && self.ty.same_snapshot(&other.ty)
            && self.specialization.same_snapshot(&other.specialization)
            && self.focus == other.focus
            && self.contains_focus == other.contains_focus
            && self.producers.same_root(&other.producers)
    }

    fn scope_compatible(&self, other: &Self) -> bool {
        self.ty.projected_scope_compatible(&other.ty)
            && self
                .specialization
                .projected_scope_compatible(&other.specialization)
    }

    fn structural_child(&self, edge: ProjectedTypeEdge) -> Option<Self> {
        let scoped_edge = match edge {
            ProjectedTypeEdge::ProductLeft => {
                crate::pass::typecheck_core::ScopedTypeEdge::ProductLeft
            }
            ProjectedTypeEdge::ProductRight => {
                crate::pass::typecheck_core::ScopedTypeEdge::ProductRight
            }
            ProjectedTypeEdge::SumLeft => crate::pass::typecheck_core::ScopedTypeEdge::SumLeft,
            ProjectedTypeEdge::SumRight => crate::pass::typecheck_core::ScopedTypeEdge::SumRight,
            ProjectedTypeEdge::FunctionParam => {
                crate::pass::typecheck_core::ScopedTypeEdge::FunctionParam
            }
            ProjectedTypeEdge::FunctionReturn => {
                crate::pass::typecheck_core::ScopedTypeEdge::FunctionReturn
            }
            ProjectedTypeEdge::ForallBody => {
                crate::pass::typecheck_core::ScopedTypeEdge::ForallBody
            }
            ProjectedTypeEdge::PathArgument(index) => {
                crate::pass::typecheck_core::ScopedTypeEdge::PathArgument(index)
            }
        };
        let specialization = self
            .specialization
            .projected_structural_child(scoped_edge)?;
        let focus = self.focus.as_ref().map(|focus| {
            let mut path = focus.path.to_vec();
            path.push(edge);
            ProjectedFocus {
                result_root: focus.result_root,
                path: path.into(),
            }
        });
        // Reflected structural operations observe the alias-unfolded carrier.
        // Reusing a source-spelled child could pair the wrong argument with an
        // alias that reorders or drops parameters. The child remains projected
        // and retains the parent's proof; only a parent already authenticated
        // as closed can carry that state to the semantic child.
        let closed = self.closed.as_ref().map(|_| {
            crate::normalization::ReflectedType::new(
                crate::ast::convert_type::<crate::ast::Lowered, crate::ast::UncheckedPrime>(
                    specialization.ty().as_type(),
                ),
                "a child of a closed projected type must remain infer-free",
            )
            .expect("a child of a closed projected type must remain infer-free")
        });
        Some(Self {
            seal: self.seal.clone(),
            ty: specialization.clone(),
            specialization,
            closed,
            focus,
            contains_focus: self.contains_focus,
            producers: self.producers.clone(),
        })
    }
}

pub(crate) enum ProjectedTypeView {
    Unit,
    Bottom,
    Product {
        left: ProjectedValue,
        right: ProjectedValue,
    },
    Sum {
        left: ProjectedValue,
        right: ProjectedValue,
    },
    Function {
        param: ProjectedValue,
        abi_arity: usize,
        ret: ProjectedValue,
    },
    Forall {
        name: String,
        arity: usize,
        body: ProjectedValue,
    },
    TypeVar {
        name: String,
        arity: usize,
    },
    TypeName {
        segments: Vec<String>,
        param_arities: Vec<usize>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectedCheckedRecipe {
    ty: ProjectedType,
    node: crate::normalization::EvalRef<ProjectedRecipeNode>,
}

#[allow(clippy::large_enum_variant)] // projected recipes remain inline; exact layouts are checked below
pub(crate) enum ProjectedCheckedResult {
    Closed(crate::normalization::EvalRef<crate::normalization::CheckedTerm>),
    Recipe(ProjectedCheckedRecipe),
}

#[allow(clippy::large_enum_variant)] // immediate evaluator handoff; boxing would precede its existing projected-value Arc allocation
pub(crate) enum ProjectedSpecializationOutcome {
    Matched(ProjectedValue),
    NoMatch,
    Diagnostic(String),
}

#[allow(clippy::large_enum_variant)] // immediate evaluator handoff; boxing would precede its existing projected-value Arc allocation
pub(crate) enum ProjectedTypeInstantiationOutcome {
    Instantiated(ProjectedValue),
    Diagnostic(String),
}

#[cfg(all(target_pointer_width = "64", not(test)))]
const _: () = {
    assert!(std::mem::size_of::<ProjectedValue>() == 240);
    assert!(std::mem::size_of::<ProjectedSpecializationOutcome>() == 240);
    assert!(std::mem::size_of::<ProjectedTypeInstantiationOutcome>() == 240);
};
#[cfg(all(target_pointer_width = "64", test))]
const _: () = {
    assert!(std::mem::size_of::<ProjectedSpecializationOutcome>() == 256);
    assert!(std::mem::size_of::<ProjectedTypeInstantiationOutcome>() == 256);
};

#[derive(Clone, Debug)]
pub(crate) enum ProjectedRecipeNode {
    Closed(crate::normalization::EvalRef<crate::normalization::CheckedTerm>),
    TemplateValue(u32),
    Pair(Arc<ProjectedCheckedRecipe>, Arc<ProjectedCheckedRecipe>),
    ProductProjection {
        product_type: ProjectedType,
        product: Arc<ProjectedCheckedRecipe>,
        left: bool,
    },
    TermCall {
        function_type: ProjectedType,
        function: Arc<ProjectedCheckedRecipe>,
        argument: Arc<ProjectedCheckedRecipe>,
    },
    IntrinsicAbsurd {
        bottom: Arc<ProjectedCheckedRecipe>,
    },
    Local {
        name: String,
    },
    IntrinsicEither {
        sum_type: ProjectedType,
        value: Arc<ProjectedCheckedRecipe>,
        left_name: String,
        left_body: Arc<ProjectedCheckedRecipe>,
        right_name: String,
        right_body: Arc<ProjectedCheckedRecipe>,
    },
    TermLet {
        value_type: ProjectedType,
        name: String,
        value: Arc<ProjectedCheckedRecipe>,
        body: Arc<ProjectedCheckedRecipe>,
    },
    TermFn {
        function_type: ProjectedType,
        names: Vec<String>,
        body: Arc<ProjectedCheckedRecipe>,
    },
    TermTypeFn {
        param: crate::ast::TypeParam,
        body: Arc<ProjectedCheckedRecipe>,
    },
    TermTypeApp {
        term: Arc<ProjectedCheckedRecipe>,
        argument: crate::pass::typecheck_core::ScopedType,
    },
    IntrinsicInjection {
        sum_type: ProjectedType,
        value: Arc<ProjectedCheckedRecipe>,
        left: bool,
    },
    IntrinsicIfThenElse {
        condition: Arc<ProjectedCheckedRecipe>,
        true_body: Arc<ProjectedCheckedRecipe>,
        false_body: Arc<ProjectedCheckedRecipe>,
    },
}

impl crate::normalization::ownership::Reclaim for ProjectedRecipeNode {
    fn reclaim(self, pending: &mut Vec<crate::normalization::ownership::Node>) {
        fn recipe(
            value: Arc<ProjectedCheckedRecipe>,
            pending: &mut Vec<crate::normalization::ownership::Node>,
        ) {
            if let Some(value) = Arc::into_inner(value) {
                crate::normalization::ownership::shared(value.node, pending);
            }
        }
        match self {
            Self::Closed(term) => crate::normalization::ownership::shared(term, pending),
            Self::TemplateValue(_) | Self::Local { .. } => {}
            Self::Pair(left, right) => {
                recipe(left, pending);
                recipe(right, pending);
            }
            Self::ProductProjection { product, .. } => recipe(product, pending),
            Self::TermCall {
                function, argument, ..
            } => {
                recipe(function, pending);
                recipe(argument, pending);
            }
            Self::IntrinsicAbsurd { bottom } => recipe(bottom, pending),
            Self::IntrinsicEither {
                value,
                left_body,
                right_body,
                ..
            } => {
                recipe(value, pending);
                recipe(left_body, pending);
                recipe(right_body, pending);
            }
            Self::TermLet { value, body, .. } => {
                recipe(value, pending);
                recipe(body, pending);
            }
            Self::TermFn { body, .. } | Self::TermTypeFn { body, .. } => recipe(body, pending),
            Self::TermTypeApp { term, .. } => recipe(term, pending),
            Self::IntrinsicInjection { value, .. } => recipe(value, pending),
            Self::IntrinsicIfThenElse {
                condition,
                true_body,
                false_body,
            } => {
                recipe(condition, pending);
                recipe(true_body, pending);
                recipe(false_body, pending);
            }
        }
    }
}

impl ProjectedCheckedRecipe {
    pub(crate) fn matches_proof(&self, proof: &MarkedComptimeProof) -> bool {
        self.ty.matches_proof(proof)
    }

    pub(crate) fn closed_term(&self) -> Option<&crate::normalization::CheckedTerm> {
        match (self.ty.closed_reflected(), self.node.as_ref()) {
            (Some(_), ProjectedRecipeNode::Closed(term)) => Some(term),
            _ => None,
        }
    }

    pub(crate) fn checked_error(
        &self,
    ) -> Option<(crate::normalization::CheckedTermErrorKind, &str)> {
        match self.node.as_ref() {
            ProjectedRecipeNode::Closed(term) => term.checked_error(),
            ProjectedRecipeNode::TemplateValue(_) | ProjectedRecipeNode::Local { .. } => None,
            ProjectedRecipeNode::Pair(left, right) => {
                left.checked_error().or_else(|| right.checked_error())
            }
            ProjectedRecipeNode::ProductProjection { product, .. } => product.checked_error(),
            ProjectedRecipeNode::TermCall {
                function, argument, ..
            } => function
                .checked_error()
                .or_else(|| argument.checked_error()),
            ProjectedRecipeNode::IntrinsicAbsurd { bottom } => bottom.checked_error(),
            ProjectedRecipeNode::IntrinsicEither {
                value,
                left_body,
                right_body,
                ..
            } => value
                .checked_error()
                .or_else(|| left_body.checked_error())
                .or_else(|| right_body.checked_error()),
            ProjectedRecipeNode::TermLet { value, body, .. } => {
                value.checked_error().or_else(|| body.checked_error())
            }
            ProjectedRecipeNode::TermFn { body, .. }
            | ProjectedRecipeNode::TermTypeFn { body, .. } => body.checked_error(),
            ProjectedRecipeNode::TermTypeApp { term, .. } => term.checked_error(),
            ProjectedRecipeNode::IntrinsicInjection { value, .. } => value.checked_error(),
            ProjectedRecipeNode::IntrinsicIfThenElse {
                condition,
                true_body,
                false_body,
            } => condition
                .checked_error()
                .or_else(|| true_body.checked_error())
                .or_else(|| false_body.checked_error()),
        }
    }

    pub(crate) fn same_snapshot(&self, other: &Self) -> bool {
        self.ty.same_snapshot(&other.ty)
            && crate::normalization::EvalRef::ptr_eq(&self.node, &other.node)
    }
}

#[derive(Clone, Debug)]
enum ProjectedValueKind {
    Type(ProjectedType),
    CheckedRecipe(ProjectedCheckedRecipe),
}

#[derive(Clone, Debug)]
pub struct ProjectedValue(ProjectedValueKind);

impl ProjectedValue {
    pub(crate) fn matches_proof(&self, proof: &MarkedComptimeProof) -> bool {
        match &self.0 {
            ProjectedValueKind::Type(ty) => ty.matches_proof(proof),
            ProjectedValueKind::CheckedRecipe(term) => term.matches_proof(proof),
        }
    }

    pub(crate) fn same_snapshot(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (ProjectedValueKind::Type(left), ProjectedValueKind::Type(right)) => {
                left.same_snapshot(right)
            }
            (ProjectedValueKind::CheckedRecipe(left), ProjectedValueKind::CheckedRecipe(right)) => {
                left.same_snapshot(right)
            }
            _ => false,
        }
    }

    pub(crate) fn structural_measure(&self) -> Option<usize> {
        match &self.0 {
            ProjectedValueKind::Type(ty) => {
                projected_type_measure(ty.specialization.ty().as_type(), false)
            }
            ProjectedValueKind::CheckedRecipe(_) => Some(1),
        }
    }

    pub(crate) fn initial_structural_measure(&self) -> Option<usize> {
        match &self.0 {
            ProjectedValueKind::Type(ty) => {
                let carrier = ty.specialization.ty().as_type();
                if matches!(
                    carrier,
                    crate::ast::Type::Goal { .. } | crate::ast::Type::Infer { .. }
                ) {
                    None
                } else {
                    projected_type_measure(carrier, true)
                }
            }
            ProjectedValueKind::CheckedRecipe(_) => Some(1),
        }
    }

    pub(crate) fn authenticates_fill_destination(&self, proof: &MarkedComptimeProof) -> bool {
        matches!(&self.0, ProjectedValueKind::Type(ty) if ty.matches_proof(proof) && ty.focus.is_some())
    }

    pub(crate) fn closed_reflected(&self) -> Option<&crate::normalization::ReflectedType> {
        match &self.0 {
            ProjectedValueKind::Type(ty) => ty.closed_reflected(),
            ProjectedValueKind::CheckedRecipe(_) => None,
        }
    }

    pub(crate) fn reflection_candidate(
        &self,
        proof: &MarkedComptimeProof,
    ) -> Option<crate::pass::typecheck_core::ProjectedReflectionCandidate> {
        match &self.0 {
            ProjectedValueKind::Type(ty) if ty.matches_proof(proof) => {
                ty.ty.projected_reflection_candidate()
            }
            ProjectedValueKind::Type(_) | ProjectedValueKind::CheckedRecipe(_) => None,
        }
    }

    pub(crate) fn closed_term(&self) -> Option<&crate::normalization::CheckedTerm> {
        match &self.0 {
            ProjectedValueKind::Type(_) => None,
            ProjectedValueKind::CheckedRecipe(recipe) => recipe.closed_term(),
        }
    }

    pub(crate) fn is_type(&self) -> bool {
        matches!(&self.0, ProjectedValueKind::Type(_))
    }

    pub(crate) fn authenticated_checked_recipe(
        &self,
        proof: &MarkedComptimeProof,
    ) -> Option<ProjectedCheckedRecipe> {
        match &self.0 {
            ProjectedValueKind::CheckedRecipe(recipe) if recipe.matches_proof(proof) => {
                Some(recipe.clone())
            }
            _ => None,
        }
    }

    fn authenticated_type_producers(&self, proof: &MarkedComptimeProof) -> Option<ProducerSet> {
        match &self.0 {
            ProjectedValueKind::Type(ty) if ty.matches_proof(proof) => Some(ty.producers.clone()),
            _ => None,
        }
    }
}

fn projected_type_measure(
    ty: &crate::ast::Type<crate::ast::Lowered>,
    unresolved_children_are_zero: bool,
) -> Option<usize> {
    let mut total = 0usize;
    let mut pending = vec![ty];
    while let Some(ty) = pending.pop() {
        // A projected goal or source placeholder can refine after the
        // snapshot. It contributes no stable nested structure to an initial
        // lower bound and cannot be measured at all on a recursive edge.
        if matches!(
            ty,
            crate::ast::Type::Goal { .. } | crate::ast::Type::Infer { .. }
        ) {
            if unresolved_children_are_zero {
                continue;
            }
            return None;
        }
        total = total.checked_add(1)?;
        match ty {
            crate::ast::Type::Product { left, right, .. }
            | crate::ast::Type::Sum { left, right, .. }
            | crate::ast::Type::Function {
                param: left,
                ret: right,
                ..
            } => {
                pending.push(right);
                pending.push(left);
            }
            crate::ast::Type::Forall { body, .. } => pending.push(body),
            crate::ast::Type::Path { args, .. } => pending.extend(args.iter().rev()),
            crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => {}
            crate::ast::Type::Goal { .. } | crate::ast::Type::Infer { .. } => {
                unreachable!("unresolved projected children are handled before node counting")
            }
            crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        }
    }
    Some(total)
}

pub(crate) fn projected_term_type(
    proof: &MarkedComptimeProof,
    term: &ProjectedValue,
) -> Result<ProjectedValue, crate::error::Error> {
    match &term.0 {
        ProjectedValueKind::CheckedRecipe(recipe) if recipe.matches_proof(proof) => {
            Ok(ProjectedValue(ProjectedValueKind::Type(recipe.ty.clone())))
        }
        ProjectedValueKind::CheckedRecipe(recipe) => Err(crate::error::Error::elaborator(
            recipe.ty.ty.ty().span(),
            "__term_type__ received a checked term from another invocation",
        )),
        ProjectedValueKind::Type(ty) => Err(crate::error::Error::elaborator(
            ty.ty.ty().span(),
            "__term_type__ expected a projected checked term",
        )),
    }
}

pub(crate) fn projected_type_view(
    proof: &MarkedComptimeProof,
    value: &ProjectedValue,
) -> Result<ProjectedTypeView, crate::error::Error> {
    let ProjectedValueKind::Type(ty) = &value.0 else {
        return Err(crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__type_view__ expected a projected type",
        ));
    };
    if !ty.matches_proof(proof) {
        return Err(crate::error::Error::elaborator(
            ty.ty.ty().span(),
            "__type_view__ received a type from another invocation",
        ));
    }
    let child = |edge| {
        ty.structural_child(edge)
            .map(ProjectedValueKind::Type)
            .map(ProjectedValue)
            .expect("a canonical structural view lost its child")
    };
    Ok(match ty.specialization.ty().as_type() {
        crate::ast::Type::Unit { .. } => ProjectedTypeView::Unit,
        crate::ast::Type::Bottom { .. } => ProjectedTypeView::Bottom,
        crate::ast::Type::Product { .. } => ProjectedTypeView::Product {
            left: child(ProjectedTypeEdge::ProductLeft),
            right: child(ProjectedTypeEdge::ProductRight),
        },
        crate::ast::Type::Sum { .. } => ProjectedTypeView::Sum {
            left: child(ProjectedTypeEdge::SumLeft),
            right: child(ProjectedTypeEdge::SumRight),
        },
        crate::ast::Type::Function { abi_arity, .. } => ProjectedTypeView::Function {
            param: child(ProjectedTypeEdge::FunctionParam),
            abi_arity: *abi_arity,
            ret: child(ProjectedTypeEdge::FunctionReturn),
        },
        crate::ast::Type::Forall { param, .. } => ProjectedTypeView::Forall {
            name: param.name.clone(),
            arity: param.effective_kind().arity(),
            body: child(ProjectedTypeEdge::ForallBody),
        },
        crate::ast::Type::Path { segments, args, .. } => {
            if let Some(arity) = ty.specialization.projected_rigid_path_arity() {
                ProjectedTypeView::TypeVar {
                    name: segments[0].name.clone(),
                    arity,
                }
            } else {
                ProjectedTypeView::TypeName {
                    segments: segments
                        .iter()
                        .map(|segment| segment.name.clone())
                        .collect(),
                    param_arities: args
                        .iter()
                        .map(|arg| match arg {
                            crate::ast::Type::Forall { param, .. } => {
                                param.effective_kind().arity()
                            }
                            _ => 0,
                        })
                        .collect(),
                }
            }
        }
        crate::ast::Type::Goal { meta, .. } => {
            return Err(crate::error::Error::elaborator(
                meta.span,
                "__type_view__ inspected an unresolved projected type",
            ));
        }
        crate::ast::Type::Infer { meta, .. } => {
            return Err(crate::error::Error::elaborator(
                meta.span,
                "__type_view__ inspected an unresolved source placeholder",
            ));
        }
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
    })
}

pub(crate) fn projected_type_arguments(
    proof: &MarkedComptimeProof,
    value: &ProjectedValue,
) -> Result<Option<Vec<ProjectedValue>>, crate::error::Error> {
    let ProjectedValueKind::Type(ty) = &value.0 else {
        return Err(crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__type_args_fold__ expected a projected type",
        ));
    };
    if !ty.matches_proof(proof) {
        return Err(crate::error::Error::elaborator(
            ty.ty.ty().span(),
            "__type_args_fold__ received a type from another invocation",
        ));
    }
    if ty.specialization_root_is_unresolved() {
        return Ok(None);
    }
    let crate::ast::Type::Path { args, .. } = ty.specialization.ty().as_type() else {
        return Ok(Some(Vec::new()));
    };
    (0..args.len())
        .map(|index| {
            ty.structural_child(ProjectedTypeEdge::PathArgument(index))
                .map(ProjectedValueKind::Type)
                .map(ProjectedValue)
                .ok_or_else(|| {
                    crate::error::Error::elaborator(
                        ty.ty.ty().span(),
                        "__type_args_fold__ could not preserve projected argument identity",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn authenticated_projected_type_operand(
    proof: &MarkedComptimeProof,
    value: &Value,
    peer: &ProjectedType,
    helper: &str,
) -> Result<ProjectedType, crate::error::Error> {
    match value {
        Value::Projected(value) => match &value.0 {
            ProjectedValueKind::Type(ty)
                if ty.matches_proof(proof)
                    && ty.seal.matches(&peer.seal)
                    && ty.scope_compatible(peer) =>
            {
                Ok(ty.clone())
            }
            ProjectedValueKind::Type(ty) => Err(crate::error::Error::elaborator(
                ty.ty.ty().span(),
                format!("{helper} received a type from another projected invocation"),
            )),
            ProjectedValueKind::CheckedRecipe(recipe) => Err(crate::error::Error::elaborator(
                recipe.ty.ty.ty().span(),
                format!("{helper} expected a projected type"),
            )),
        },
        Value::ReflType(reflected) if peer.matches_proof(proof) => {
            let converted = crate::ast::convert_type::<
                crate::ast::UncheckedPrime,
                crate::ast::Lowered,
            >(reflected.as_type());
            let exact = peer.ty.projected_closed_peer(
                crate::pass::typecheck_core::InternedType::fresh_canonical(converted.clone()),
            );
            let specialization = peer.specialization.projected_closed_peer(
                crate::pass::typecheck_core::InternedType::fresh_canonical(converted),
            );
            Ok(ProjectedType {
                seal: peer.seal.clone(),
                ty: exact,
                specialization,
                closed: Some(reflected.clone()),
                focus: None,
                contains_focus: false,
                producers: ProducerSet::default(),
            })
        }
        _ => Err(crate::error::Error::elaborator(
            peer.ty.ty().span(),
            format!("{helper} expected a reflected type with a same-invocation peer"),
        )),
    }
}

fn authenticated_projected_recipe_operand(
    proof: &MarkedComptimeProof,
    value: &Value,
    peer: &ProjectedType,
    helper: &str,
) -> Result<ProjectedCheckedRecipe, crate::error::Error> {
    match value {
        Value::Projected(value) => match &value.0 {
            ProjectedValueKind::CheckedRecipe(recipe)
                if recipe.matches_proof(proof)
                    && recipe.ty.seal.matches(&peer.seal)
                    && recipe.ty.scope_compatible(peer) =>
            {
                Ok(recipe.clone())
            }
            ProjectedValueKind::CheckedRecipe(recipe) => Err(crate::error::Error::elaborator(
                recipe.ty.ty.ty().span(),
                format!("{helper} received a term from another projected invocation"),
            )),
            ProjectedValueKind::Type(ty) => Err(crate::error::Error::elaborator(
                ty.ty.ty().span(),
                format!("{helper} expected a projected checked term"),
            )),
        },
        Value::CheckedTerm(term) => {
            let reflected = crate::normalization::ReflectedType::new(
                term.ty().clone(),
                "a checked term operand must have an infer-free type",
            )?;
            let converted = crate::ast::convert_type::<
                crate::ast::UncheckedPrime,
                crate::ast::Lowered,
            >(term.ty());
            let exact = peer.ty.projected_closed_peer(
                crate::pass::typecheck_core::InternedType::fresh_canonical(converted.clone()),
            );
            let specialization = peer.specialization.projected_closed_peer(
                crate::pass::typecheck_core::InternedType::fresh_canonical(converted),
            );
            Ok(ProjectedCheckedRecipe {
                ty: ProjectedType {
                    seal: peer.seal.clone(),
                    ty: exact,
                    specialization,
                    closed: Some(reflected),
                    focus: None,
                    contains_focus: false,
                    producers: ProducerSet::default(),
                },
                node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Closed(term.clone())),
            })
        }
        _ => Err(crate::error::Error::elaborator(
            peer.ty.ty().span(),
            format!("{helper} expected a checked term"),
        )),
    }
}

fn first_projected_type<'a>(
    values: impl IntoIterator<Item = &'a Value>,
) -> Option<&'a ProjectedType> {
    values.into_iter().find_map(|value| match value {
        Value::Projected(value) => match &value.0 {
            ProjectedValueKind::Type(ty) => Some(ty),
            ProjectedValueKind::CheckedRecipe(recipe) => Some(&recipe.ty),
        },
        _ => None,
    })
}

fn projected_binary_type(
    proof: &MarkedComptimeProof,
    left: &Value,
    right: &Value,
    helper: &str,
    kind: crate::pass::typecheck_core::ScopedTypeBinaryKind,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([left, right]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            format!("{helper} projected dispatch had no projected operand"),
        )
    })?;
    let left = authenticated_projected_type_operand(proof, left, peer, helper)?;
    let right = authenticated_projected_type_operand(proof, right, peer, helper)?;
    let span = left.ty.ty().span();
    let ty = left
        .ty
        .projected_binary_peer(&right.ty, kind, span)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                format!("{helper} received types with incompatible rigid scopes"),
            )
        })?;
    let specialization = left
        .specialization
        .projected_binary_peer(&right.specialization, kind, span)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                format!("{helper} received incompatible specialization snapshots"),
            )
        })?;
    let closed = match (&left.closed, &right.closed) {
        (Some(left), Some(right)) => {
            let meta = crate::ast::Meta::new(span);
            let ty = match kind {
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Product => {
                    crate::ast::Type::Product {
                        left: Box::new(left.as_type().clone()),
                        right: Box::new(right.as_type().clone()),
                        meta,
                    }
                }
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Sum => crate::ast::Type::Sum {
                    left: Box::new(left.as_type().clone()),
                    right: Box::new(right.as_type().clone()),
                    meta,
                },
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Function { abi_arity } => {
                    crate::ast::Type::Function {
                        param: Box::new(left.as_type().clone()),
                        ret: Box::new(right.as_type().clone()),
                        abi_arity,
                        caps: (),
                        meta,
                    }
                }
            };
            Some(crate::normalization::ReflectedType::new(
                ty,
                "a closed projected structural type must remain infer-free",
            )?)
        }
        _ => None,
    };
    Ok(ProjectedValue(ProjectedValueKind::Type(ProjectedType {
        seal: left.seal,
        ty,
        specialization,
        closed,
        focus: None,
        contains_focus: left.contains_focus || right.contains_focus,
        producers: left.producers.union(&right.producers),
    })))
}

pub(crate) fn projected_type_product(
    proof: &MarkedComptimeProof,
    left: &Value,
    right: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    projected_binary_type(
        proof,
        left,
        right,
        "__type_product__",
        crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
    )
}

pub(crate) fn projected_type_sum(
    proof: &MarkedComptimeProof,
    left: &Value,
    right: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    projected_binary_type(
        proof,
        left,
        right,
        "__type_sum__",
        crate::pass::typecheck_core::ScopedTypeBinaryKind::Sum,
    )
}

pub(crate) fn projected_type_arrow(
    proof: &MarkedComptimeProof,
    params: &Value,
    abi_arity: usize,
    result: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    projected_binary_type(
        proof,
        params,
        result,
        "__type_arrow__",
        crate::pass::typecheck_core::ScopedTypeBinaryKind::Function { abi_arity },
    )
}

pub(crate) fn projected_type_apply(
    proof: &MarkedComptimeProof,
    head: &Value,
    argument: &Value,
) -> Result<Option<ProjectedValue>, crate::error::Error> {
    let peer = first_projected_type([head, argument]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__type_apply__ projected dispatch had no projected operand",
        )
    })?;
    let head = authenticated_projected_type_operand(proof, head, peer, "__type_apply__")?;
    let argument = authenticated_projected_type_operand(proof, argument, &head, "__type_apply__")?;
    if head.specialization_root_is_unresolved() {
        return Ok(None);
    }
    if !matches!(
        head.specialization.ty().as_type(),
        crate::ast::Type::Path { .. }
    ) {
        return Ok(Some(ProjectedValue(ProjectedValueKind::Type(head))));
    }
    let span = head.ty.ty().span();
    let ty = head.ty.projected_path_apply(&argument.ty).ok_or_else(|| {
        crate::error::Error::elaborator(
            span,
            "__type_apply__ could not preserve the exact projected path identity",
        )
    })?;
    let specialization = head
        .specialization
        .projected_path_apply(&argument.specialization)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                "__type_apply__ received incompatible specialization snapshots",
            )
        })?;
    let closed = match (&head.closed, &argument.closed) {
        (Some(head), Some(argument)) => {
            let mut applied = head.as_type().clone();
            let crate::ast::Type::Path { args, .. } = &mut applied else {
                unreachable!("a canonical projected path had a non-path closed snapshot")
            };
            args.push(argument.as_type().clone());
            Some(crate::normalization::ReflectedType::new(
                applied,
                "a closed projected type application must remain infer-free",
            )?)
        }
        _ => None,
    };
    Ok(Some(ProjectedValue(ProjectedValueKind::Type(
        ProjectedType {
            seal: head.seal,
            ty,
            specialization,
            closed,
            focus: None,
            contains_focus: head.contains_focus || argument.contains_focus,
            producers: head.producers.union(&argument.producers),
        },
    ))))
}

pub(crate) fn projected_type_instantiate(
    proof: &MarkedComptimeProof,
    scheme: &Value,
    argument: &Value,
) -> Result<ProjectedTypeInstantiationOutcome, crate::error::Error> {
    let peer = first_projected_type([scheme, argument]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__type_instantiate__ projected dispatch had no projected operand",
        )
    })?;
    let scheme = authenticated_projected_type_operand(proof, scheme, peer, "__type_instantiate__")?;
    let argument =
        authenticated_projected_type_operand(proof, argument, &scheme, "__type_instantiate__")?;
    let crate::ast::Type::Forall { param, .. } = scheme.specialization.ty().as_type() else {
        return Ok(ProjectedTypeInstantiationOutcome::Diagnostic(format!(
            "__type_instantiate__ expected a forall type, got `{}`",
            crate::pass::typecheck_core::display_type(scheme.specialization.ty().as_type())
        )));
    };
    if param.effective_kind().arity() > 0
        && !matches!(
            argument.specialization.ty().as_type(),
            crate::ast::Type::Path { .. }
        )
    {
        return Ok(ProjectedTypeInstantiationOutcome::Diagnostic(format!(
            "__type_instantiate__ cannot substitute structural type `{}` for higher-kinded binder `{}`",
            crate::pass::typecheck_core::display_type(argument.specialization.ty().as_type()),
            param.name
        )));
    }
    let span = scheme.ty.ty().span();
    let Some(ty) = scheme.ty.projected_instantiate_forall(&argument.ty) else {
        return Err(crate::error::Error::elaborator(
            span,
            "__type_instantiate__ could not preserve the exact projected scope",
        ));
    };
    let Some(specialization) = scheme
        .specialization
        .projected_instantiate_forall(&argument.specialization)
    else {
        return Err(crate::error::Error::elaborator(
            span,
            "__type_instantiate__ could not preserve its specialization snapshot",
        ));
    };
    let closed = match (&scheme.closed, &argument.closed) {
        (Some(scheme), Some(argument)) => {
            let crate::ast::Type::Forall { param, body, .. } = scheme.as_type() else {
                unreachable!("a canonical projected forall had a non-forall closed snapshot")
            };
            Some(crate::normalization::ReflectedType::new(
                crate::pass::typecheck_core::subst_type(
                    body,
                    &std::collections::HashMap::from([(
                        param.name.clone(),
                        argument.as_type().clone(),
                    )]),
                ),
                "a closed projected forall instantiation must remain infer-free",
            )?)
        }
        _ => None,
    };
    Ok(ProjectedTypeInstantiationOutcome::Instantiated(
        ProjectedValue(ProjectedValueKind::Type(ProjectedType {
            seal: scheme.seal,
            ty,
            specialization,
            closed,
            focus: None,
            contains_focus: scheme.contains_focus || argument.contains_focus,
            producers: scheme.producers.union(&argument.producers),
        })),
    ))
}

pub(crate) fn projected_type_forall_finish(
    proof: &MarkedComptimeProof,
    name: String,
    arity: usize,
    body: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([body]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__type_forall__ projected callback returned no projected type",
        )
    })?;
    let body = authenticated_projected_type_operand(proof, body, peer, "__type_forall__")?;
    let span = body.ty.ty().span();
    let param = crate::ast::TypeParam {
        name,
        span,
        kind: Some(crate::ast::Kind::arrow_chain(arity)),
    };
    let ty = body
        .ty
        .projected_forall_abstract(param.clone(), span)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                "__type_forall__ generated a binder that would capture an existing rigid",
            )
        })?;
    let specialization = body
        .specialization
        .projected_forall_abstract(param.clone(), span)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                "__type_forall__ could not preserve its specialization scope",
            )
        })?;
    let closed = body
        .closed
        .as_ref()
        .map(|body| {
            crate::normalization::ReflectedType::new(
                crate::ast::Type::Forall {
                    param,
                    body: Box::new(body.as_type().clone()),
                    meta: crate::ast::Meta::new(span),
                },
                "a closed projected forall must remain infer-free",
            )
        })
        .transpose()?;
    Ok(ProjectedValue(ProjectedValueKind::Type(ProjectedType {
        seal: body.seal,
        ty,
        specialization,
        closed,
        focus: None,
        contains_focus: body.contains_focus,
        producers: body.producers,
    })))
}

pub(crate) fn projected_term_type_app(
    proof: &MarkedComptimeProof,
    term: &Value,
    argument: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([term, argument]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__term_type_app__ projected dispatch had no projected operand",
        )
    })?;
    let term = authenticated_projected_recipe_operand(proof, term, peer, "__term_type_app__")?;
    let argument =
        authenticated_projected_type_operand(proof, argument, &term.ty, "__term_type_app__")?;
    let span = term.ty.ty.ty().span();
    let ty = term
        .ty
        .ty
        .projected_instantiate_forall(&argument.ty)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                "__term_type_app__ expected a forall term and a compatible type argument",
            )
        })?;
    let specialization = term
        .ty
        .specialization
        .projected_instantiate_forall(&argument.specialization)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                "__term_type_app__ could not preserve its specialization scope",
            )
        })?;
    let closed = match (&term.ty.closed, &argument.closed) {
        (Some(scheme), Some(argument)) => {
            let crate::ast::Type::Forall { param, body, .. } = scheme.as_type() else {
                unreachable!("a closed projected type-function term had a non-forall type")
            };
            Some(crate::normalization::ReflectedType::new(
                crate::pass::typecheck_core::subst_type(
                    body,
                    &std::collections::HashMap::from([(
                        param.name.clone(),
                        argument.as_type().clone(),
                    )]),
                ),
                "a closed projected term type application must remain infer-free",
            )?)
        }
        _ => None,
    };
    let producers = term.ty.producers.union(&argument.producers);
    let result = ProjectedType {
        seal: term.ty.seal.clone(),
        ty,
        specialization,
        closed,
        focus: term.ty.focus.clone(),
        contains_focus: term.ty.contains_focus || argument.contains_focus,
        producers,
    };
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: result,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermTypeApp {
                term: Arc::new(term),
                argument: argument.ty,
            }),
        },
    )))
}

pub(crate) fn projected_intrinsic_injection(
    proof: &MarkedComptimeProof,
    sum_type: &Value,
    value: &Value,
    left: bool,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([sum_type, value]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "projected sum injection had no projected operand",
        )
    })?;
    let sum_type =
        authenticated_projected_type_operand(proof, sum_type, peer, "projected sum injection")?;
    let payload_type = sum_type
        .structural_child(if left {
            ProjectedTypeEdge::SumLeft
        } else {
            ProjectedTypeEdge::SumRight
        })
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                sum_type.ty.ty().span(),
                "projected injection expected a sum type",
            )
        })?;
    let value = authenticated_projected_recipe_operand(
        proof,
        value,
        &payload_type,
        "projected sum injection",
    )?;
    let mut result = sum_type.clone();
    result.producers = result.producers.union(&value.ty.producers);
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: result,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::IntrinsicInjection {
                sum_type,
                value: Arc::new(value),
                left,
            }),
        },
    )))
}

pub(crate) fn projected_intrinsic_if_then_else(
    proof: &MarkedComptimeProof,
    result_type: &Value,
    condition: &Value,
    true_body: &Value,
    false_body: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer =
        first_projected_type([result_type, condition, true_body, false_body]).ok_or_else(|| {
            crate::error::Error::elaborator(
                crate::span::Span::new(0, 0),
                "__intrinsic_if_then_else__ projected dispatch had no projected operand",
            )
        })?;
    let result_type = authenticated_projected_type_operand(
        proof,
        result_type,
        peer,
        "__intrinsic_if_then_else__",
    )?;
    let condition = authenticated_projected_recipe_operand(
        proof,
        condition,
        &result_type,
        "__intrinsic_if_then_else__",
    )?;
    let true_body = authenticated_projected_recipe_operand(
        proof,
        true_body,
        &result_type,
        "__intrinsic_if_then_else__",
    )?;
    let false_body = authenticated_projected_recipe_operand(
        proof,
        false_body,
        &result_type,
        "__intrinsic_if_then_else__",
    )?;
    let mut result = result_type;
    result.producers = result
        .producers
        .union(&condition.ty.producers)
        .union(&true_body.ty.producers)
        .union(&false_body.ty.producers);
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: result,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::IntrinsicIfThenElse {
                condition: Arc::new(condition),
                true_body: Arc::new(true_body),
                false_body: Arc::new(false_body),
            }),
        },
    )))
}

pub(crate) fn projected_term_local(
    proof: &MarkedComptimeProof,
    (ty, peer): (&Value, &Value),
    name: String,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([ty, peer]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "a projected local requires a projected type",
        )
    })?;
    let ty = authenticated_projected_type_operand(proof, ty, peer, "projected local")?;
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local { name }),
        },
    )))
}

pub(crate) fn projected_term_let(
    proof: &MarkedComptimeProof,
    value_type: &Value,
    value: &Value,
    name: String,
    body: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([value_type, value, body]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__term_let__ projected dispatch had no projected operand",
        )
    })?;
    let value_type = authenticated_projected_type_operand(proof, value_type, peer, "__term_let__")?;
    let value = authenticated_projected_recipe_operand(proof, value, &value_type, "__term_let__")?;
    let body = authenticated_projected_recipe_operand(proof, body, &value_type, "__term_let__")?;
    let mut result = body.ty.clone();
    result.producers = result
        .producers
        .union(&value_type.producers)
        .union(&value.ty.producers);
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: result,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermLet {
                value_type,
                name,
                value: Arc::new(value),
                body: Arc::new(body),
            }),
        },
    )))
}

fn projected_function_parameter_types(
    function_type: &ProjectedType,
) -> Result<Vec<ProjectedType>, crate::error::Error> {
    let crate::ast::Type::Function { abi_arity, .. } = function_type.specialization.ty().as_type()
    else {
        return Err(crate::error::Error::elaborator(
            function_type.ty.ty().span(),
            "__term_fn__ expected a function type",
        ));
    };
    if *abi_arity == 0 {
        return Ok(Vec::new());
    }
    let mut cursor = function_type
        .structural_child(ProjectedTypeEdge::FunctionParam)
        .expect("a canonical function lost its parameter child");
    let mut params = Vec::with_capacity(*abi_arity);
    for index in 0..*abi_arity {
        if index + 1 == *abi_arity {
            params.push(cursor);
            break;
        }
        let left = cursor
            .structural_child(ProjectedTypeEdge::ProductLeft)
            .ok_or_else(|| {
                crate::error::Error::elaborator(
                    cursor.ty.ty().span(),
                    "__term_fn__ ABI arity exceeded its exact parameter product spine",
                )
            })?;
        cursor = cursor
            .structural_child(ProjectedTypeEdge::ProductRight)
            .expect("a projected product left parameter lost its right peer");
        params.push(left);
    }
    Ok(params)
}

pub(crate) fn projected_term_fn_parameter_types(
    proof: &MarkedComptimeProof,
    function_type: &Value,
) -> Result<Vec<ProjectedValue>, crate::error::Error> {
    let peer = first_projected_type([function_type]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__term_fn__ projected parameter preparation had no projected function type",
        )
    })?;
    let function_type =
        authenticated_projected_type_operand(proof, function_type, peer, "__term_fn__")?;
    projected_function_parameter_types(&function_type).map(|params| {
        params
            .into_iter()
            .map(|param| ProjectedValue(ProjectedValueKind::Type(param)))
            .collect()
    })
}

pub(crate) fn projected_term_fn(
    proof: &MarkedComptimeProof,
    function_type: &Value,
    names: Vec<String>,
    body: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([function_type, body]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__term_fn__ projected dispatch had no projected operand",
        )
    })?;
    let function_type =
        authenticated_projected_type_operand(proof, function_type, peer, "__term_fn__")?;
    let params = projected_function_parameter_types(&function_type)?;
    if names.len() != params.len() {
        return Err(crate::error::Error::elaborator(
            function_type.ty.ty().span(),
            "__term_fn__ generated the wrong number of value binders",
        ));
    }
    let body = authenticated_projected_recipe_operand(proof, body, &function_type, "__term_fn__")?;
    let mut result = function_type.clone();
    result.producers = result.producers.union(&body.ty.producers);
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: result,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermFn {
                function_type,
                names,
                body: Arc::new(body),
            }),
        },
    )))
}

pub(crate) fn projected_term_type_fn_finish(
    proof: &MarkedComptimeProof,
    name: String,
    arity: usize,
    body: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([body]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__term_type_fn__ projected callback returned no projected term",
        )
    })?;
    let body = authenticated_projected_recipe_operand(proof, body, peer, "__term_type_fn__")?;
    let span = body.ty.ty.ty().span();
    let param = crate::ast::TypeParam {
        name,
        span,
        kind: Some(crate::ast::Kind::arrow_chain(arity)),
    };
    let ty = body
        .ty
        .ty
        .projected_forall_abstract(param.clone(), span)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                "__term_type_fn__ generated a binder that would capture an existing rigid",
            )
        })?;
    let specialization = body
        .ty
        .specialization
        .projected_forall_abstract(param.clone(), span)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                "__term_type_fn__ could not preserve its specialization scope",
            )
        })?;
    let closed = body
        .ty
        .closed
        .as_ref()
        .map(|body| {
            crate::normalization::ReflectedType::new(
                crate::ast::Type::Forall {
                    param: param.clone(),
                    body: Box::new(body.as_type().clone()),
                    meta: crate::ast::Meta::new(span),
                },
                "a closed projected type function must remain infer-free",
            )
        })
        .transpose()?;
    let result = ProjectedType {
        seal: body.ty.seal.clone(),
        ty,
        specialization,
        closed,
        focus: None,
        contains_focus: body.ty.contains_focus,
        producers: body.ty.producers.clone(),
    };
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: result,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermTypeFn {
                param,
                body: Arc::new(body),
            }),
        },
    )))
}

pub(crate) fn projected_intrinsic_pair(
    proof: &MarkedComptimeProof,
    left: &Value,
    right: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([left, right]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__intrinsic_pair__ projected dispatch had no projected operand",
        )
    })?;
    let left = authenticated_projected_recipe_operand(proof, left, peer, "__intrinsic_pair__")?;
    let right = authenticated_projected_recipe_operand(proof, right, peer, "__intrinsic_pair__")?;
    let span = left.ty.ty.ty().span();
    let ty = left
        .ty
        .ty
        .projected_binary_peer(
            &right.ty.ty,
            crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
            span,
        )
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                "__intrinsic_pair__ received terms with incompatible rigid scopes",
            )
        })?;
    let specialization = left
        .ty
        .specialization
        .projected_binary_peer(
            &right.ty.specialization,
            crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
            span,
        )
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                span,
                "__intrinsic_pair__ received incompatible specialization snapshots",
            )
        })?;
    let producers = left.ty.producers.union(&right.ty.producers);
    let contains_focus = left.ty.contains_focus || right.ty.contains_focus;
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: ProjectedType {
                seal: left.ty.seal.clone(),
                ty,
                specialization,
                closed: None,
                focus: None,
                contains_focus,
                producers,
            },
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Pair(
                Arc::new(left),
                Arc::new(right),
            )),
        },
    )))
}

pub(crate) fn projected_intrinsic_product_projection(
    proof: &MarkedComptimeProof,
    product_type: &Value,
    value: &Value,
    left: bool,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([product_type, value]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "projected product projection had no projected operand",
        )
    })?;
    let product_type = authenticated_projected_type_operand(
        proof,
        product_type,
        peer,
        "projected product projection",
    )?;
    let value = authenticated_projected_recipe_operand(
        proof,
        value,
        &product_type,
        "projected product projection",
    )?;
    let edge = if left {
        ProjectedTypeEdge::ProductLeft
    } else {
        ProjectedTypeEdge::ProductRight
    };
    let mut result = product_type.structural_child(edge).ok_or_else(|| {
        crate::error::Error::elaborator(
            product_type.ty.ty().span(),
            "projected product projection expected a product type",
        )
    })?;
    result.producers = result
        .producers
        .union(&product_type.producers)
        .union(&value.ty.producers);
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: result,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::ProductProjection {
                product_type,
                product: Arc::new(value),
                left,
            }),
        },
    )))
}

pub(crate) fn projected_term_call(
    proof: &MarkedComptimeProof,
    function_type: &Value,
    function: &Value,
    argument: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([function_type, function, argument]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__term_call__ projected dispatch had no projected operand",
        )
    })?;
    let function_type =
        authenticated_projected_type_operand(proof, function_type, peer, "__term_call__")?;
    let function =
        authenticated_projected_recipe_operand(proof, function, &function_type, "__term_call__")?;
    let parameter = function_type
        .structural_child(ProjectedTypeEdge::FunctionParam)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                function_type.ty.ty().span(),
                "__term_call__ expected a function type",
            )
        })?;
    let argument =
        authenticated_projected_recipe_operand(proof, argument, &parameter, "__term_call__")?;
    let mut result = function_type
        .structural_child(ProjectedTypeEdge::FunctionReturn)
        .expect("a projected function parameter had a matching return child");
    result.producers = result
        .producers
        .union(&function.ty.producers)
        .union(&argument.ty.producers);
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: result,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermCall {
                function_type,
                function: Arc::new(function),
                argument: Arc::new(argument),
            }),
        },
    )))
}

pub(crate) fn projected_intrinsic_absurd(
    proof: &MarkedComptimeProof,
    bottom: &Value,
    result_type: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let peer = first_projected_type([bottom, result_type]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__intrinsic_absurd__ projected dispatch had no projected operand",
        )
    })?;
    let result =
        authenticated_projected_type_operand(proof, result_type, peer, "__intrinsic_absurd__")?;
    let bottom =
        authenticated_projected_recipe_operand(proof, bottom, &result, "__intrinsic_absurd__")?;
    if !matches!(
        bottom.ty.specialization.ty().as_type(),
        crate::ast::Type::Bottom { .. }
    ) {
        return Err(crate::error::Error::elaborator(
            bottom.ty.ty.ty().span(),
            "__intrinsic_absurd__ expected a checked Bottom term",
        ));
    }
    let mut ty = result;
    ty.producers = ty.producers.union(&bottom.ty.producers);
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::IntrinsicAbsurd {
                bottom: Arc::new(bottom),
            }),
        },
    )))
}

pub(crate) struct ProjectedEitherPrepared {
    sum_type: ProjectedType,
    result_type: ProjectedType,
    value: ProjectedCheckedRecipe,
    left_name: String,
    right_name: String,
}

pub(crate) fn projected_intrinsic_either_prepare(
    proof: &MarkedComptimeProof,
    sum_type: &Value,
    result_type: &Value,
    value: &Value,
    left_name: String,
    right_name: String,
) -> Result<(ProjectedEitherPrepared, ProjectedValue, ProjectedValue), crate::error::Error> {
    let peer = first_projected_type([sum_type, result_type, value]).ok_or_else(|| {
        crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__intrinsic_either__ projected dispatch had no projected operand",
        )
    })?;
    let sum_type =
        authenticated_projected_type_operand(proof, sum_type, peer, "__intrinsic_either__")?;
    let result_type = authenticated_projected_type_operand(
        proof,
        result_type,
        &sum_type,
        "__intrinsic_either__",
    )?;
    let value =
        authenticated_projected_recipe_operand(proof, value, &sum_type, "__intrinsic_either__")?;
    let left_type = sum_type
        .structural_child(ProjectedTypeEdge::SumLeft)
        .ok_or_else(|| {
            crate::error::Error::elaborator(
                sum_type.ty.ty().span(),
                "__intrinsic_either__ expected a sum type",
            )
        })?;
    let right_type = sum_type
        .structural_child(ProjectedTypeEdge::SumRight)
        .expect("a projected sum left child had a matching right child");
    let left_payload = ProjectedValue(ProjectedValueKind::CheckedRecipe(ProjectedCheckedRecipe {
        ty: left_type,
        node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
            name: left_name.clone(),
        }),
    }));
    let right_payload = ProjectedValue(ProjectedValueKind::CheckedRecipe(ProjectedCheckedRecipe {
        ty: right_type,
        node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
            name: right_name.clone(),
        }),
    }));
    Ok((
        ProjectedEitherPrepared {
            sum_type,
            result_type,
            value,
            left_name,
            right_name,
        },
        left_payload,
        right_payload,
    ))
}

pub(crate) fn projected_intrinsic_either_finish(
    proof: &MarkedComptimeProof,
    prepared: ProjectedEitherPrepared,
    left_body: &Value,
    right_body: &Value,
) -> Result<ProjectedValue, crate::error::Error> {
    let left_body = authenticated_projected_recipe_operand(
        proof,
        left_body,
        &prepared.result_type,
        "__intrinsic_either__",
    )?;
    let right_body = authenticated_projected_recipe_operand(
        proof,
        right_body,
        &prepared.result_type,
        "__intrinsic_either__",
    )?;
    let mut result = prepared.result_type;
    result.producers = result
        .producers
        .union(&prepared.value.ty.producers)
        .union(&left_body.ty.producers)
        .union(&right_body.ty.producers);
    Ok(ProjectedValue(ProjectedValueKind::CheckedRecipe(
        ProjectedCheckedRecipe {
            ty: result,
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::IntrinsicEither {
                sum_type: prepared.sum_type,
                value: Arc::new(prepared.value),
                left_name: prepared.left_name,
                left_body: Arc::new(left_body),
                right_name: prepared.right_name,
                right_body: Arc::new(right_body),
            }),
        },
    )))
}

pub(crate) fn specialize_projected_term(
    proof: &MarkedComptimeProof,
    term: &ProjectedValue,
    pattern: &Value,
    target: &Value,
) -> Result<ProjectedSpecializationOutcome, crate::error::Error> {
    let ProjectedValueKind::CheckedRecipe(recipe) = &term.0 else {
        return Err(crate::error::Error::elaborator(
            crate::span::Span::new(0, 0),
            "__term_specialize__ expected a projected checked term",
        ));
    };
    if !recipe.matches_proof(proof) {
        return Err(crate::error::Error::elaborator(
            recipe.ty.ty.ty().span(),
            "__term_specialize__ received a checked term from another invocation",
        ));
    }

    let mut exact_result = recipe.ty.ty.clone();
    let mut specialization_result = recipe.ty.specialization.clone();
    let mut params = Vec::new();
    let mut operand_scope = specialization_result.clone();
    while let crate::ast::Type::Forall { param, .. } = operand_scope.ty().as_type() {
        params.push(param.clone());
        operand_scope = operand_scope
            .projected_structural_child(crate::pass::typecheck_core::ScopedTypeEdge::ForallBody)
            .expect("a canonical forall carrier lost its body");
    }

    fn operand(
        value: &Value,
        proof: &MarkedComptimeProof,
        peer: &crate::pass::typecheck_core::ScopedType,
        span: crate::span::Span,
    ) -> Result<(crate::pass::typecheck_core::ScopedType, ProducerSet), crate::error::Error> {
        match value {
            Value::Projected(value) => match &value.0 {
                ProjectedValueKind::Type(ty) if ty.matches_proof(proof) => {
                    Ok((ty.specialization.clone(), ty.producers.clone()))
                }
                ProjectedValueKind::Type(_) | ProjectedValueKind::CheckedRecipe(_) => {
                    Err(crate::error::Error::elaborator(
                        span,
                        "__term_specialize__ received an unauthenticated projected type",
                    ))
                }
            },
            Value::ReflType(ty) => {
                let ty = crate::ast::convert_type::<crate::ast::UncheckedPrime, crate::ast::Lowered>(
                    ty.as_type(),
                );
                Ok((
                    peer.projected_closed_peer(
                        crate::pass::typecheck_core::InternedType::fresh_canonical(ty),
                    ),
                    ProducerSet::default(),
                ))
            }
            _ => Err(crate::error::Error::elaborator(
                span,
                "__term_specialize__ expected reflected pattern and target types",
            )),
        }
    }

    let span = recipe.ty.ty.ty().span();
    let (pattern, pattern_producers) = operand(pattern, proof, &operand_scope, span)?;
    let (target, target_producers) = operand(target, proof, &operand_scope, span)?;
    let context = recipe.ty.seal.specialization_context();
    let arguments = match crate::pass::typecheck_core::solve_scoped_specialization_by_name(
        &params, &pattern, &target, span, context,
    ) {
        Ok(crate::pass::typecheck_core::IsolatedSpecialization::Matched(arguments)) => arguments,
        Ok(crate::pass::typecheck_core::IsolatedSpecialization::NoMatch) => {
            return Ok(ProjectedSpecializationOutcome::NoMatch);
        }
        Err(error) => {
            return Ok(ProjectedSpecializationOutcome::Diagnostic(
                error.diagnostic().message.clone(),
            ));
        }
    };

    let all_producers = recipe
        .ty
        .producers
        .union(&pattern_producers)
        .union(&target_producers);
    let mut specialized = recipe.clone();
    specialized.ty.producers = all_producers.clone();
    for argument in arguments {
        let Some(next_exact) = exact_result.projected_instantiate_forall(&argument) else {
            return Ok(ProjectedSpecializationOutcome::Diagnostic(
                "__term_specialize__ produced an inapplicable type argument".to_owned(),
            ));
        };
        let Some(next_specialization) =
            specialization_result.projected_instantiate_forall(&argument)
        else {
            return Ok(ProjectedSpecializationOutcome::Diagnostic(
                "__term_specialize__ produced an inapplicable canonical type argument".to_owned(),
            ));
        };
        specialized = ProjectedCheckedRecipe {
            ty: ProjectedType {
                seal: recipe.ty.seal.clone(),
                ty: next_exact.clone(),
                specialization: next_specialization.clone(),
                closed: None,
                focus: recipe.ty.focus.clone(),
                contains_focus: recipe.ty.contains_focus,
                producers: all_producers.clone(),
            },
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermTypeApp {
                term: Arc::new(specialized),
                argument,
            }),
        };
        exact_result = next_exact;
        specialization_result = next_specialization;
    }
    Ok(ProjectedSpecializationOutcome::Matched(ProjectedValue(
        ProjectedValueKind::CheckedRecipe(specialized),
    )))
}

#[cfg(test)]
pub(crate) fn focused_type_for_test(
    proof: &MarkedComptimeProof,
    reflected: crate::normalization::ReflectedType,
) -> ProjectedValue {
    let ty =
        crate::pass::typecheck_core::ScopedType::projected_for_test(crate::ast::convert_type::<
            crate::ast::UncheckedPrime,
            crate::ast::Lowered,
        >(reflected.as_type()));
    ProjectedValue(ProjectedValueKind::Type(ProjectedType {
        seal: proof.seal.clone(),
        specialization: ty.clone(),
        ty,
        closed: Some(reflected),
        focus: Some(ProjectedFocus {
            result_root: ProjectedResultRoot(0),
            path: Arc::from([]),
        }),
        contains_focus: true,
        producers: ProducerSet::default(),
    }))
}

#[cfg(test)]
pub(crate) fn projected_type_for_fuel_test(
    ty: crate::ast::Type<crate::ast::Lowered>,
    specialization: crate::ast::Type<crate::ast::Lowered>,
) -> ProjectedValue {
    projected_type_with_proof_for_fuel_test(ty, specialization).1
}

#[cfg(test)]
pub(crate) fn projected_type_with_proof_for_fuel_test(
    ty: crate::ast::Type<crate::ast::Lowered>,
    specialization: crate::ast::Type<crate::ast::Lowered>,
) -> (MarkedComptimeProof, ProjectedValue) {
    let seal = InvocationSeal::fresh();
    (
        MarkedComptimeProof { seal: seal.clone() },
        open_specialized_type_reflection_for_test(
            seal,
            crate::pass::typecheck_core::ScopedType::projected_for_test(ty),
            crate::pass::typecheck_core::ScopedType::projected_for_test(specialization),
        ),
    )
}

#[cfg(test)]
pub(crate) fn closed_specialized_type_for_test(
    ty: crate::ast::Type<crate::ast::Lowered>,
    specialization: crate::ast::Type<crate::ast::Lowered>,
) -> (MarkedComptimeProof, MarkedComptimeProof, ProjectedValue) {
    let seal = InvocationSeal::fresh();
    let foreign = InvocationSeal::fresh();
    let closed = crate::normalization::ReflectedType::new(
        crate::ast::convert_type::<crate::ast::Lowered, crate::ast::UncheckedPrime>(&ty),
        "a closed projected test carrier must be infer-free",
    )
    .expect("a closed projected test carrier must be infer-free");
    (
        MarkedComptimeProof { seal: seal.clone() },
        MarkedComptimeProof { seal: foreign },
        ProjectedValue(ProjectedValueKind::Type(ProjectedType {
            seal,
            ty: crate::pass::typecheck_core::ScopedType::projected_for_test(ty),
            specialization: crate::pass::typecheck_core::ScopedType::projected_for_test(
                specialization,
            ),
            closed: Some(closed),
            focus: Some(ProjectedFocus {
                result_root: ProjectedResultRoot(0),
                path: Arc::from([]),
            }),
            contains_focus: true,
            producers: ProducerSet::leaf(0),
        })),
    )
}

#[cfg(test)]
pub(crate) fn projected_type_fuel_evidence_for_test(
    value: &ProjectedValue,
) -> (bool, bool, bool, bool) {
    let ProjectedValueKind::Type(ty) = &value.0 else {
        panic!("fuel evidence expected a projected type")
    };
    (
        ty.closed.is_some(),
        ty.focus.is_some(),
        ty.contains_focus,
        ty.producers.is_empty(),
    )
}

#[cfg(test)]
pub(crate) fn open_function_reflection_for_test(
    param: crate::normalization::ReflectedType,
) -> (MarkedComptimeProof, MarkedComptimeProof, ProjectedValue) {
    let span = crate::span::Span::new(0, 0);
    let ty =
        crate::pass::typecheck_core::ScopedType::projected_for_test(crate::ast::Type::Function {
            param: Box::new(crate::ast::convert_type::<
                crate::ast::UncheckedPrime,
                crate::ast::Lowered,
            >(param.as_type())),
            ret: Box::new(crate::ast::Type::Goal {
                goal: crate::ast::TypeGoalRef::for_test(1, 0, 0),
                args: Vec::new(),
                meta: crate::ast::Meta::new(span),
                ext: (),
            }),
            abi_arity: 1,
            caps: (),
            meta: crate::ast::Meta::new(span),
        });
    open_function_reflection_capabilities_for_test(ty)
}

#[cfg(test)]
pub(crate) fn open_noncanonical_function_reflection_for_test()
-> (MarkedComptimeProof, MarkedComptimeProof, ProjectedValue) {
    let span = crate::span::Span::new(0, 0);
    let rigid = crate::ast::Type::synth_path(vec!["A".to_owned()], Vec::new(), span);
    let alias = crate::ast::Type::synth_path(vec!["Alias".to_owned()], vec![rigid.clone()], span);
    let goal = crate::ast::Type::Goal {
        goal: crate::ast::TypeGoalRef::for_test(2, 0, 0),
        args: Vec::new(),
        meta: crate::ast::Meta::new(span),
        ext: (),
    };
    let exact = crate::pass::typecheck_core::ScopedType::projected_noncanonical_for_test(
        crate::ast::Type::Function {
            param: Box::new(alias),
            ret: Box::new(goal.clone()),
            abi_arity: 1,
            caps: (),
            meta: crate::ast::Meta::new(span),
        },
        "consumer",
        vec![("A".to_owned(), crate::ast::Kind::Star, 7)],
    );
    let specialization = exact.projected_canonical_peer_for_test(crate::ast::Type::Function {
        param: Box::new(rigid),
        ret: Box::new(goal),
        abi_arity: 1,
        caps: (),
        meta: crate::ast::Meta::new(span),
    });
    let seal = InvocationSeal::fresh();
    let foreign = InvocationSeal::fresh();
    (
        MarkedComptimeProof { seal: seal.clone() },
        MarkedComptimeProof { seal: foreign },
        open_specialized_type_reflection_for_test(seal, exact, specialization),
    )
}

#[cfg(test)]
fn open_function_reflection_capabilities_for_test(
    ty: crate::pass::typecheck_core::ScopedType,
) -> (MarkedComptimeProof, MarkedComptimeProof, ProjectedValue) {
    let seal = InvocationSeal::fresh();
    let foreign = InvocationSeal::fresh();
    (
        MarkedComptimeProof { seal: seal.clone() },
        MarkedComptimeProof { seal: foreign },
        open_type_reflection_for_test(seal, ty),
    )
}

#[cfg(test)]
fn open_type_reflection_for_test(
    seal: InvocationSeal,
    ty: crate::pass::typecheck_core::ScopedType,
) -> ProjectedValue {
    open_specialized_type_reflection_for_test(seal, ty.clone(), ty)
}

#[cfg(test)]
fn open_specialized_type_reflection_for_test(
    seal: InvocationSeal,
    ty: crate::pass::typecheck_core::ScopedType,
    specialization: crate::pass::typecheck_core::ScopedType,
) -> ProjectedValue {
    ProjectedValue(ProjectedValueKind::Type(ProjectedType {
        seal,
        specialization,
        ty,
        closed: None,
        focus: Some(ProjectedFocus {
            result_root: ProjectedResultRoot(0),
            path: Arc::from([]),
        }),
        contains_focus: true,
        producers: ProducerSet::leaf(0),
    }))
}

#[cfg(test)]
pub(crate) fn unresolved_type_reflections_for_test() -> (
    MarkedComptimeProof,
    MarkedComptimeProof,
    ProjectedValue,
    ProjectedValue,
    ProjectedValue,
) {
    let span = crate::span::Span::new(0, 0);
    let seal = InvocationSeal::fresh();
    let foreign = InvocationSeal::fresh();
    let goal =
        crate::pass::typecheck_core::ScopedType::projected_for_test(crate::ast::Type::Goal {
            goal: crate::ast::TypeGoalRef::for_test(4, 0, 0),
            args: Vec::new(),
            meta: crate::ast::Meta::new(span),
            ext: (),
        });
    let infer =
        crate::pass::typecheck_core::ScopedType::projected_for_test(crate::ast::Type::Infer {
            meta: crate::ast::Meta::new(span),
            ext: (),
        });
    let alias = crate::pass::typecheck_core::ScopedType::projected_for_test(
        crate::ast::Type::synth_path(vec!["Alias".to_owned()], Vec::new(), span),
    );
    (
        MarkedComptimeProof { seal: seal.clone() },
        MarkedComptimeProof { seal: foreign },
        open_type_reflection_for_test(seal.clone(), goal.clone()),
        open_type_reflection_for_test(seal.clone(), infer),
        open_specialized_type_reflection_for_test(seal, alias, goal),
    )
}

#[cfg(test)]
pub(crate) fn open_function_reflection_evidence_for_test(
    parent: &ProjectedValue,
    param: &ProjectedValue,
    ret: &ProjectedValue,
) -> bool {
    let (
        ProjectedValueKind::Type(parent),
        ProjectedValueKind::Type(param),
        ProjectedValueKind::Type(ret),
    ) = (&parent.0, &param.0, &ret.0)
    else {
        return false;
    };
    parent.closed.is_none()
        && parent.ty.projected_reflection_candidate().is_none()
        && param.ty.projected_reflection_candidate().is_some()
        && ret.ty.projected_reflection_candidate().is_none()
        && parent.contains_focus
        && param.contains_focus
        && ret.contains_focus
        && parent.producers.same_root(&param.producers)
        && parent.producers.same_root(&ret.producers)
        && parent
            .focus
            .as_ref()
            .is_some_and(|focus| focus.path.is_empty())
        && param
            .focus
            .as_ref()
            .is_some_and(|focus| focus.path.as_ref() == [ProjectedTypeEdge::FunctionParam])
        && ret
            .focus
            .as_ref()
            .is_some_and(|focus| focus.path.as_ref() == [ProjectedTypeEdge::FunctionReturn])
}

#[cfg(test)]
pub(crate) fn open_either_reflection_for_test() -> (
    MarkedComptimeProof,
    ProjectedValue,
    ProjectedValue,
    ProjectedValue,
    ProjectedValue,
    ProjectedValue,
) {
    let span = crate::span::Span::new(0, 0);
    let unit = crate::ast::Type::Unit {
        meta: crate::ast::Meta::new(span),
    };
    let sum = crate::ast::Type::Sum {
        left: Box::new(unit.clone()),
        right: Box::new(unit.clone()),
        meta: crate::ast::Meta::new(span),
    };
    let seal = InvocationSeal::fresh();
    let proof = MarkedComptimeProof { seal: seal.clone() };
    let projected_type = |ty: crate::ast::Type<crate::ast::Lowered>,
                          closed: Option<crate::normalization::ReflectedType>,
                          focus: Option<ProjectedFocus>,
                          producers: ProducerSet| {
        let ty = crate::pass::typecheck_core::ScopedType::projected_for_test(ty);
        let contains_focus = focus.is_some();
        ProjectedType {
            seal: seal.clone(),
            specialization: ty.clone(),
            ty,
            closed,
            focus,
            contains_focus,
            producers,
        }
    };
    let reflected = |ty: &crate::ast::Type<crate::ast::Lowered>| {
        crate::normalization::ReflectedType::new(
            crate::ast::convert_type::<crate::ast::Lowered, crate::ast::UncheckedPrime>(ty),
            "the projected either fixture must remain infer-free",
        )
        .expect("reflect projected either fixture")
    };
    let sum_ty = projected_type(
        sum.clone(),
        Some(reflected(&sum)),
        None,
        ProducerSet::leaf(0),
    );
    let result_ty = projected_type(
        unit.clone(),
        Some(reflected(&unit)),
        Some(ProjectedFocus {
            result_root: ProjectedResultRoot(0),
            path: Arc::from([]),
        }),
        ProducerSet::default(),
    );
    let scrutinee = ProjectedValue(ProjectedValueKind::CheckedRecipe(ProjectedCheckedRecipe {
        ty: sum_ty.clone(),
        node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TemplateValue(0)),
    }));
    let branch = |slot, producer, name: &str| {
        let goal = crate::ast::Type::Goal {
            goal: crate::ast::TypeGoalRef::for_test(3, 0, slot),
            args: Vec::new(),
            meta: crate::ast::Meta::new(span),
            ext: (),
        };
        ProjectedValue(ProjectedValueKind::CheckedRecipe(ProjectedCheckedRecipe {
            ty: projected_type(goal, None, None, ProducerSet::leaf(producer)),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                name: name.to_owned(),
            }),
        }))
    };
    (
        proof,
        ProjectedValue(ProjectedValueKind::Type(sum_ty)),
        ProjectedValue(ProjectedValueKind::Type(result_ty)),
        scrutinee,
        branch(0, 1, "left_result"),
        branch(1, 2, "right_result"),
    )
}

#[cfg(test)]
pub(crate) fn deferred_either_evidence_for_test(
    proof: &MarkedComptimeProof,
    value: &ProjectedValue,
) -> bool {
    let Some(recipe) = value.authenticated_checked_recipe(proof) else {
        return false;
    };
    let ProjectedRecipeNode::IntrinsicEither {
        left_body,
        right_body,
        ..
    } = recipe.node.as_ref()
    else {
        return false;
    };
    let mut producers = Vec::new();
    fn collect(node: &ProducerDag, producers: &mut Vec<u32>) {
        match node {
            ProducerDag::Leaf(producer) => producers.push(*producer),
            ProducerDag::Union(left, right) => {
                collect(left, producers);
                collect(right, producers);
            }
        }
    }
    if let Some(root) = &recipe.ty.producers.0 {
        collect(root, &mut producers);
    }
    producers.sort_unstable();
    producers.dedup();
    matches!(recipe.ty.ty.ty().as_type(), crate::ast::Type::Unit { .. })
        && recipe.ty.contains_focus
        && recipe.ty.focus.as_ref().is_some_and(|focus| {
            focus.result_root == ProjectedResultRoot(0) && focus.path.is_empty()
        })
        && matches!(
            left_body.ty.ty.ty().as_type(),
            crate::ast::Type::Goal { .. }
        )
        && matches!(
            right_body.ty.ty.ty().as_type(),
            crate::ast::Type::Goal { .. }
        )
        && producers == [0, 1, 2]
}

#[cfg(test)]
pub(crate) fn specialization_term_for_test() -> (MarkedComptimeProof, FillContext, ProjectedValue) {
    let span = crate::span::Span::new(0, 0);
    let scheme = crate::ast::Type::Forall {
        param: crate::ast::TypeParam {
            name: "A".to_owned(),
            span,
            kind: None,
        },
        body: Box::new(crate::pass::typecheck_core::ty_path("A", span)),
        meta: crate::ast::Meta::new(span),
    };
    let ty = crate::pass::typecheck_core::ScopedType::projected_for_test(scheme);
    let seal = InvocationSeal::fresh();
    seal.install_specialization_context(
        crate::pass::typecheck_core::IsolatedSpecializationContext::from_canonical_nominal_head_kinds(
            std::collections::HashMap::new(),
        ),
    );
    let (proof, context) = capabilities_for(&seal);
    let term = ProjectedValue(ProjectedValueKind::CheckedRecipe(ProjectedCheckedRecipe {
        ty: ProjectedType {
            seal,
            specialization: ty.clone(),
            ty,
            closed: None,
            focus: Some(ProjectedFocus {
                result_root: ProjectedResultRoot(0),
                path: Arc::from([]),
            }),
            contains_focus: true,
            producers: ProducerSet::leaf(0),
        },
        node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TemplateValue(0)),
    }));
    (proof, context, term)
}

#[cfg(test)]
pub(crate) fn specialization_evidence_for_test(
    original: &ProjectedValue,
    specialized: &ProjectedValue,
) -> bool {
    let (
        ProjectedValueKind::CheckedRecipe(original),
        ProjectedValueKind::CheckedRecipe(specialized),
    ) = (&original.0, &specialized.0)
    else {
        return false;
    };
    let unit = original.ty.ty.projected_closed_peer(
        crate::pass::typecheck_core::InternedType::fresh_canonical(crate::ast::Type::Unit {
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
        }),
    );
    let Some(expected) = original.ty.ty.projected_instantiate_forall(&unit) else {
        return false;
    };
    original.ty.focus == specialized.ty.focus
        && original.ty.producers.same_root(&specialized.ty.producers)
        && specialized.ty.ty.transient_key() == expected.transient_key()
}

#[cfg(test)]
pub(crate) fn projected_term_let_evidence_for_test(
    proof: &MarkedComptimeProof,
    value: &ProjectedValue,
) -> bool {
    let Some(recipe) = value.authenticated_checked_recipe(proof) else {
        return false;
    };
    let ProjectedRecipeNode::TermLet {
        value_type,
        name,
        value,
        body,
    } = recipe.node.as_ref()
    else {
        return false;
    };
    value_type.closed.is_some()
        && value.matches_proof(proof)
        && !matches!(value.node.as_ref(), ProjectedRecipeNode::Closed(_))
        && name.starts_with("__ct_let_")
        && name.ends_with("__")
        && matches!(
            body.node.as_ref(),
            ProjectedRecipeNode::Local { name: local } if local == name
        )
}

#[derive(Clone, Debug)]
pub(crate) struct MarkedComptimeProof {
    seal: InvocationSeal,
}

impl MarkedComptimeProof {
    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.seal.matches(&other.seal)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FillRelation {
    previous: Option<crate::normalization::EvalRef<FillRelation>>,
    destination: Value,
    candidate: Value,
    producers: ProducerSet,
}

impl crate::normalization::ownership::Reclaim for FillRelation {
    fn reclaim(self, pending: &mut Vec<crate::normalization::ownership::Node>) {
        if let Some(previous) = self.previous {
            crate::normalization::ownership::shared(previous, pending);
        }
        if let Some(producers) = self.producers.0 {
            crate::normalization::ownership::shared(producers, pending);
        }
        pending.push(self.destination.into());
        pending.push(self.candidate.into());
    }
}

pub(super) struct FillAdoption {
    relations: Vec<AdoptedFillRelation>,
    relation_producers: Vec<u32>,
    independent_producers: Vec<u32>,
    producer_mode: super::frontier::FrontierMode,
    producer_index: usize,
    sweep_complete: bool,
    barrier_applied: bool,
    phase: FillAdoptionPhase,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<FillAdoption>() == 88);

pub(super) enum FillAdoptionAdvance {
    Complete,
    Pending,
    BarrierApplied,
    Producer(FillProducerRequest),
}

pub(super) struct FillProducerRequest {
    pub(super) producer: u32,
    pub(super) parent: crate::ast::TypeGoalOwner,
    pub(super) producer_mode: super::frontier::FrontierMode,
    pub(super) inherited_mode: super::frontier::FrontierMode,
    relation_probe: bool,
    expected: Option<crate::pass::typecheck_core::ScopedType>,
}

pub(super) struct ResumedFillProducer {
    producer: u32,
    complete: bool,
    newly_complete: bool,
}

pub(super) struct FillProducerCloseRequest {
    producer: u32,
    pub(super) parent: crate::ast::TypeGoalOwner,
    leaf_expected: crate::pass::typecheck_core::ScopedType,
    root_expected: Option<crate::pass::typecheck_core::ScopedType>,
    #[cfg(test)]
    count_relation_close: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FillAdoptionPhase {
    Relations,
    Independent,
}

struct AdoptedFillRelation {
    destination: crate::pass::typecheck_core::ScopedType,
    candidate: crate::pass::typecheck_core::ScopedType,
    producers: ProducerSet,
}

fn freeze_relation_producers<'a>(
    producers: impl IntoIterator<Item = &'a ProducerSet>,
    producer_bound: u32,
) -> (Vec<u32>, Vec<u32>) {
    fn visit(
        node: &crate::normalization::EvalRef<ProducerDag>,
        visited: &mut std::collections::HashSet<*const ProducerDag>,
        relation: &mut [bool],
    ) {
        if !visited.insert(crate::normalization::EvalRef::as_ptr(node)) {
            return;
        }
        match node.as_ref() {
            ProducerDag::Leaf(ordinal) => relation[*ordinal as usize] = true,
            ProducerDag::Union(left, right) => {
                visit(left, visited, relation);
                visit(right, visited, relation);
            }
        }
    }
    let mut visited = std::collections::HashSet::new();
    let mut relation = vec![false; producer_bound as usize];
    for producers in producers {
        if let Some(root) = &producers.0 {
            visit(root, &mut visited, &mut relation);
        }
    }
    let mut relation_producers = Vec::new();
    let mut independent_producers = Vec::new();
    for (producer, belongs_to_relation) in relation.into_iter().enumerate() {
        if belongs_to_relation {
            relation_producers.push(producer as u32);
        } else {
            independent_producers.push(producer as u32);
        }
    }
    (relation_producers, independent_producers)
}

#[cfg(test)]
fn advance_mode_major_sweep(
    producers: &[u32],
    mut advance: impl FnMut(u32) -> Result<bool, crate::error::Error>,
) -> Result<bool, crate::error::Error> {
    let mut complete = true;
    for producer in producers.iter().copied() {
        complete &= advance(producer)?;
    }
    Ok(complete)
}

impl FillAdoption {
    pub(super) fn accepts_resumed_producer_immediately(&self) -> bool {
        self.phase == FillAdoptionPhase::Independent
    }

    #[allow(clippy::too_many_arguments)] // one producer transition shares its projection, checked output, mode, and retained frontier
    pub(super) fn advance<'m>(
        &mut self,
        projection: &mut MarkedProjection<'m>,
        checked: &ProjectedCheckedResult,
        output: &crate::pass::typecheck_core::ScopedType,
        mode: super::frontier::FrontierMode,
        span: crate::span::Span,
        frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
        tcx: &mut super::TypeCtx<'m, '_, crate::ast::Lowered>,
    ) -> Result<FillAdoptionAdvance, crate::error::Error> {
        if self.producer_mode.rank() > mode.rank() {
            return Ok(FillAdoptionAdvance::Pending);
        }
        loop {
            if self.barrier_applied {
                self.barrier_applied = false;
                let complete = self.sweep_complete;
                self.producer_index = 0;
                self.sweep_complete = true;
                if !complete {
                    #[cfg(test)]
                    record_fill_relation_work(|work| work.soft_restarts += 1);
                    // A committed relation wave is genuine monotone progress.
                    // Retry the remaining producers from the exact phase so a
                    // newly accepted proposal can unlock another producer
                    // before either advances to hard preflight/final checks.
                    // Every restart accepted at least one previously open
                    // producer, so the finite authored producer set bounds
                    // this loop.
                    self.producer_mode = super::frontier::FrontierMode::ExpectedOnly;
                    if self.producer_mode.rank() <= mode.rank() {
                        continue;
                    }
                    return Ok(FillAdoptionAdvance::Pending);
                }
                self.producer_mode = super::frontier::FrontierMode::ExpectedOnly;
                require_relations_closed(&self.relations, span, frontier, tcx)?;
                self.phase = FillAdoptionPhase::Independent;
                continue;
            }
            let producers = match self.phase {
                FillAdoptionPhase::Relations => &self.relation_producers,
                FillAdoptionPhase::Independent => &self.independent_producers,
            };
            #[cfg(test)]
            if self.producer_index == 0 {
                super::record_completion_work(|work| work.producer_sweeps += 1);
            }
            while let Some(&producer) = producers.get(self.producer_index) {
                if projection.producer_is_complete(producer) {
                    self.producer_index += 1;
                    continue;
                }
                // Returned relations may close an independent producer's
                // header before this phase, so take the same late snapshot
                // used for relation producers.
                let expected =
                    projection.producer_expected_snapshot(producer, span, frontier, tcx)?;
                #[cfg(test)]
                record_fill_relation_work(|work| work.producer_request_visits += 1);
                return Ok(FillAdoptionAdvance::Producer(FillProducerRequest {
                    producer,
                    parent: projection.producer_parent(producer),
                    producer_mode: self.producer_mode,
                    inherited_mode: mode,
                    relation_probe: self.phase == FillAdoptionPhase::Relations,
                    expected,
                }));
            }
            if self.phase == FillAdoptionPhase::Relations
                && !self.relation_producers.is_empty()
                && self.producer_mode == super::frontier::FrontierMode::LexicalFallback
            {
                self.producer_index = 0;
                self.sweep_complete = true;
                self.producer_mode = self
                    .producer_mode
                    .successor(true)
                    .unwrap_or_else(|| unreachable!("a Fill relation probe followed Final"));
                if self.producer_mode.rank() <= mode.rank() {
                    continue;
                }
                return Ok(FillAdoptionAdvance::Pending);
            }
            if self.phase == FillAdoptionPhase::Relations
                && self.producer_mode != super::frontier::FrontierMode::LexicalFallback
                // An exact wave may commit early only when every relation
                // producer completed. A strict subset must remain isolated
                // until RelationProbe gives independently ready actions the
                // same proposal opportunity as structural fallbacks.
                && (self.producer_mode != super::frontier::FrontierMode::ExpectedOnly
                    || self.sweep_complete)
                && projection.accept_relation_producer_wave(
                    &self.relation_producers,
                    span,
                    frontier,
                    tcx,
                )?
            {
                self.barrier_applied = true;
                return Ok(FillAdoptionAdvance::BarrierApplied);
            }
            let complete = self.sweep_complete;
            self.producer_index = 0;
            self.sweep_complete = true;
            if !complete {
                let Some(next_mode) = self
                    .producer_mode
                    .successor(self.phase == FillAdoptionPhase::Relations)
                else {
                    if producers
                        .iter()
                        .all(|&producer| match projection.producer_state(producer) {
                            StagedOperand::Ordinary(child, _) => child.is_completion_wait(),
                            StagedOperand::Completed { .. }
                            | StagedOperand::OpenCompleted { .. } => true,
                            _ => false,
                        })
                    {
                        return Ok(FillAdoptionAdvance::Pending);
                    }
                    unreachable!("a fill producer remained pending after Final")
                };
                self.producer_mode = next_mode;
                if next_mode.rank() <= mode.rank() {
                    continue;
                }
                return Ok(FillAdoptionAdvance::Pending);
            }
            self.producer_mode = super::frontier::FrontierMode::ExpectedOnly;
            match self.phase {
                FillAdoptionPhase::Relations => {
                    require_relations_closed(&self.relations, span, frontier, tcx)?;
                    self.phase = FillAdoptionPhase::Independent;
                }
                FillAdoptionPhase::Independent => {
                    {
                        let (store, _owner, delta, _publication) = frontier.parts_mut();
                        store.require_goal_free_after_delta(delta, output.clone(), tcx)?;
                    }
                    let terminal_result = projection
                        .terminal_result
                        .as_ref()
                        .expect("a fills adoption lost its projected eta-body result");
                    install_checked_result_expectation(
                        checked,
                        terminal_result,
                        span,
                        frontier,
                        tcx,
                    )?;
                    return Ok(FillAdoptionAdvance::Complete);
                }
            }
        }
    }

    pub(super) fn record_producer(&mut self, resumed: &ResumedFillProducer) {
        let producers = match self.phase {
            FillAdoptionPhase::Relations => &self.relation_producers,
            FillAdoptionPhase::Independent => &self.independent_producers,
        };
        let producer = producers
            .get(self.producer_index)
            .expect("a fills producer resumed after its frozen adoption phase");
        assert_eq!(
            *producer, resumed.producer,
            "a fills producer resumed outside its frozen mode-major position"
        );
        self.sweep_complete &= resumed.complete;
        self.producer_index += 1;
    }
}

fn require_relations_closed<'m>(
    relations: &[AdoptedFillRelation],
    _span: crate::span::Span,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    for relation in relations {
        let (store, _owner, delta, _publication) = frontier.parts_mut();
        store.require_goal_free_after_delta(delta, relation.destination.clone(), tcx)?;
        store.require_goal_free_after_delta(delta, relation.candidate.clone(), tcx)?;
    }
    Ok(())
}

fn install_checked_result_expectation<'m>(
    checked: &ProjectedCheckedResult,
    output: &crate::pass::typecheck_core::ScopedType,
    span: crate::span::Span,
    frontier: &mut super::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &super::TypeCtx<'m, '_, crate::ast::Lowered>,
) -> Result<(), crate::error::Error> {
    let result = match checked {
        ProjectedCheckedResult::Recipe(recipe) => recipe.ty.ty.clone(),
        ProjectedCheckedResult::Closed(term) => {
            let ty = crate::ast::convert_type::<crate::ast::UncheckedPrime, crate::ast::Lowered>(
                term.ty(),
            );
            let (store, owner, _delta, _publication) = frontier.parts_mut();
            store.scoped_type(
                owner,
                crate::pass::typecheck_core::InternedType::fresh_canonical(ty),
                span,
            )?
        }
    };
    let (store, _owner, delta, _publication) = frontier.parts_mut();
    let output = store.require_goal_free_after_delta(delta, output.clone(), tcx)?;
    store.constrain(delta, result, output, span, tcx)
}

#[derive(Clone, Debug)]
pub(crate) struct FillContext {
    seal: InvocationSeal,
    head: Option<crate::normalization::EvalRef<FillRelation>>,
    len: usize,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReturnedFillRelationObservation {
    pub(crate) destination_root: u32,
    pub(crate) destination_path: Vec<ProjectedTypeEdge>,
    pub(crate) destination_closed: bool,
    pub(crate) candidate_display: String,
}

#[cfg(test)]
thread_local! {
    static RETURNED_FILL_RELATION_OBSERVATIONS:
        TargetedTestState<Vec<Vec<ReturnedFillRelationObservation>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[must_use]
pub(crate) struct ReturnedFillRelationObserver {
    active: bool,
}

#[cfg(test)]
pub(crate) fn observe_returned_fill_relations_at(
    module_path: &str,
    span: crate::span::Span,
) -> ReturnedFillRelationObserver {
    RETURNED_FILL_RELATION_OBSERVATIONS.with(|observations| {
        let mut observations = observations.borrow_mut();
        assert!(
            observations.is_none(),
            "returned-fill-relation observers cannot nest"
        );
        *observations = Some((module_path.to_owned(), span, Vec::new()));
    });
    ReturnedFillRelationObserver { active: true }
}

#[cfg(test)]
impl ReturnedFillRelationObserver {
    pub(crate) fn take(mut self) -> Vec<Vec<ReturnedFillRelationObservation>> {
        self.active = false;
        RETURNED_FILL_RELATION_OBSERVATIONS.with(|observations| {
            observations
                .borrow_mut()
                .take()
                .map(|(_, _, observations)| observations)
                .unwrap_or_default()
        })
    }
}

#[cfg(test)]
impl Drop for ReturnedFillRelationObserver {
    fn drop(&mut self) {
        if self.active {
            RETURNED_FILL_RELATION_OBSERVATIONS.with(|observations| {
                observations.borrow_mut().take();
            });
        }
    }
}

#[cfg(test)]
fn returned_fill_relation_observer_matches(module_path: &str, span: crate::span::Span) -> bool {
    RETURNED_FILL_RELATION_OBSERVATIONS.with(|observations| {
        observations
            .borrow()
            .as_ref()
            .is_some_and(|(target_module, target_span, _)| {
                target_module == module_path && *target_span == span
            })
    })
}

#[cfg(test)]
fn observe_authenticated_fill_relation(
    relation: &FillRelation,
    candidate: &crate::pass::typecheck_core::ScopedType,
) -> ReturnedFillRelationObservation {
    let Value::Projected(destination) = &relation.destination else {
        unreachable!("an authenticated fill destination is projected")
    };
    let ProjectedValueKind::Type(destination) = &destination.0 else {
        unreachable!("an authenticated fill destination is a projected type")
    };
    let focus = destination
        .focus
        .as_ref()
        .expect("an authenticated fill destination retains its exact result focus");
    ReturnedFillRelationObservation {
        destination_root: focus.result_root.0,
        destination_path: focus.path.to_vec(),
        destination_closed: destination.closed.is_some(),
        candidate_display: crate::pass::typecheck_core::display_type(candidate.ty().as_type()),
    }
}

#[cfg(test)]
fn record_returned_fill_relations(
    module_path: &str,
    span: crate::span::Span,
    snapshot: Vec<ReturnedFillRelationObservation>,
) {
    RETURNED_FILL_RELATION_OBSERVATIONS.with(|observations| {
        let mut observations = observations.borrow_mut();
        let Some((target_module, target_span, snapshots)) = observations.as_mut() else {
            return;
        };
        if target_module == module_path && *target_span == span {
            snapshots.push(snapshot);
        }
    });
}

pub(crate) fn fresh_capabilities() -> (MarkedComptimeProof, FillContext) {
    let seal = InvocationSeal::fresh();
    capabilities_for(&seal)
}

#[cfg(test)]
pub(crate) fn projected_error_recipes_for_test() -> (
    MarkedComptimeProof,
    FillContext,
    MarkedComptimeProof,
    ProjectedValue,
    ProjectedValue,
) {
    let span = crate::span::Span::new(0, 0);
    let seal = InvocationSeal::fresh();
    let (proof, context) = capabilities_for(&seal);
    let foreign_proof = MarkedComptimeProof {
        seal: InvocationSeal::fresh(),
    };
    let unit = crate::pass::typecheck_core::ScopedType::projected_for_test(crate::ast::Type::<
        crate::ast::Lowered,
    >::Unit {
        meta: crate::ast::Meta::new(span),
    });
    let reflected_unit = crate::normalization::ReflectedType::new(
        crate::ast::Type::<crate::ast::UncheckedPrime>::Unit {
            meta: crate::ast::Meta::new(span),
        },
        "a projected test unit must be closed",
    )
    .expect("reflect projected test unit");
    let source = ProjectedValue(ProjectedValueKind::CheckedRecipe(ProjectedCheckedRecipe {
        ty: ProjectedType {
            seal,
            specialization: unit.clone(),
            ty: unit,
            closed: Some(reflected_unit),
            focus: None,
            contains_focus: false,
            producers: ProducerSet::leaf(0),
        },
        node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TemplateValue(0)),
    }));
    let source = crate::normalization::projected_value(source);
    let checked_error = |kind, message| {
        crate::normalization::Value::CheckedTerm(crate::normalization::EvalRef::new(
            crate::normalization::CheckedTerm::checked_error_for_test(kind, message),
        ))
    };

    let nested_right = projected_intrinsic_pair(
        &proof,
        &source,
        &checked_error(
            crate::normalization::CheckedTermErrorKind::Elaborator,
            "nested projected elaborator failure",
        ),
    )
    .expect("pair a projected source with a closed checked error");

    let nested_left = projected_intrinsic_pair(
        &proof,
        &checked_error(
            crate::normalization::CheckedTermErrorKind::Type,
            "left projected type failure",
        ),
        &source,
    )
    .expect("pair a closed checked error before a projected source");
    let nested_left = crate::normalization::projected_value(nested_left);
    let ordered = projected_intrinsic_pair(
        &proof,
        &nested_left,
        &checked_error(
            crate::normalization::CheckedTermErrorKind::Elaborator,
            "right projected elaborator failure",
        ),
    )
    .expect("retain checked-error traversal order through nested projected pairs");

    (proof, context, foreign_proof, nested_right, ordered)
}

fn capabilities_for(seal: &InvocationSeal) -> (MarkedComptimeProof, FillContext) {
    (
        MarkedComptimeProof { seal: seal.clone() },
        FillContext {
            seal: seal.clone(),
            head: None,
            len: 0,
        },
    )
}

impl FillContext {
    pub(crate) fn release_children(self, pending: &mut Vec<crate::normalization::ownership::Node>) {
        if let Some(head) = self.head {
            crate::normalization::ownership::shared(head, pending);
        }
    }

    pub(crate) fn matches_proof(&self, proof: &MarkedComptimeProof) -> bool {
        self.seal.matches(&proof.seal)
    }

    pub(crate) fn append(
        &self,
        proof: &MarkedComptimeProof,
        destination: Value,
        candidate: Value,
    ) -> Option<Self> {
        if !proof.seal.matches(&self.seal) {
            return None;
        }
        let Value::Projected(destination_handle) = &destination else {
            return None;
        };
        if !destination_handle.authenticates_fill_destination(proof) {
            return None;
        }
        let destination_producers = destination_handle.authenticated_type_producers(proof)?;
        let candidate_producers = match &candidate {
            Value::Projected(candidate) => candidate.authenticated_type_producers(proof)?,
            Value::ReflType(_) => ProducerSet::default(),
            _ => return None,
        };
        Some(Self {
            seal: self.seal.clone(),
            head: Some(crate::normalization::EvalRef::new(FillRelation {
                previous: self.head.clone(),
                destination,
                candidate,
                producers: destination_producers.union(&candidate_producers),
            })),
            len: self
                .len
                .checked_add(1)
                .expect("fill transcript length overflow"),
        })
    }

    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.seal.matches(&other.seal)
    }

    pub(crate) fn same_snapshot(&self, other: &Self) -> bool {
        self.matches(other)
            && self.len == other.len
            && match (&self.head, &other.head) {
                (None, None) => true,
                (Some(left), Some(right)) => crate::normalization::EvalRef::ptr_eq(left, right),
                _ => false,
            }
    }

    pub(crate) fn relations_in_source_order(&self) -> Vec<(&Value, &Value)> {
        let mut relations = Vec::with_capacity(self.len);
        let mut current = self.head.as_deref();
        while let Some(relation) = current {
            relations.push((&relation.destination, &relation.candidate));
            current = relation.previous.as_deref();
        }
        relations.reverse();
        relations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    use crate::ast::{CallArg, Expr, Lowered, Meta, PathSegment};
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::parse;
    use crate::pass::resolve::Package;
    use crate::pass::typecheck_full::{Elaborations, ResolvedBinder};
    use crate::pipeline::Pipeline;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn boxed_fills_state_and_scheduler_payload_layouts_stay_exact() {
        assert_eq!(std::mem::size_of::<MarkedProjection<'_>>(), 440);
        assert_eq!(std::mem::size_of::<MarkedCallState<'_>>(), 1264);
        assert_eq!(std::mem::size_of::<FillProducerRequest>(), 128);
        assert_eq!(std::mem::size_of::<FillAdoptionAdvance>(), 128);
    }

    #[test]
    fn deep_projected_carrier_measure_is_stack_safe() {
        const DEPTH: usize = 20_000;
        let mut ty: crate::ast::Type<Lowered> = crate::ast::Type::Unit {
            meta: Meta::new(crate::span::Span::new(0, 0)),
        };
        for _ in 0..DEPTH {
            ty = crate::ast::Type::Product {
                left: Box::new(crate::ast::Type::Unit {
                    meta: Meta::new(crate::span::Span::new(0, 0)),
                }),
                right: Box::new(ty),
                meta: Meta::new(crate::span::Span::new(0, 0)),
            };
        }
        assert_eq!(projected_type_measure(&ty, false), Some(DEPTH * 2 + 1));
        assert_eq!(projected_type_measure(&ty, true), Some(DEPTH * 2 + 1));
        std::mem::forget(ty);
    }

    #[test]
    fn modeled_wide_relation_retry_work_has_an_explicit_quadratic_bound() {
        const PRODUCERS: usize = 512;

        let scheduled_modes = usize::from(super::super::frontier::FrontierMode::Final.rank()) + 1;
        assert_eq!(scheduled_modes, 5);
        let wave_scan_modes = [
            super::super::frontier::FrontierMode::RelationProbe,
            super::super::frontier::FrontierMode::FinalPreflight,
            super::super::frontier::FrontierMode::Final,
        ]
        .len();

        // This deliberately pessimistic model lets every successful wave
        // accept only one new producer while accepted suffix producers remain
        // physically open behind the authored prefix. Every mode may therefore
        // request the full cohort again, and every non-lexical late mode may
        // scan the full relation before the one scan needed to leave the final
        // completed cohort.
        let producer_request_visit_bound = scheduled_modes * PRODUCERS * PRODUCERS;
        let producer_advance_visit_bound = producer_request_visit_bound;
        let relation_wave_scan_visit_bound = wave_scan_modes * PRODUCERS * PRODUCERS + PRODUCERS;
        let committed_wave_bound = PRODUCERS;
        let soft_restart_bound = PRODUCERS - 1;

        assert_eq!(producer_request_visit_bound, 1_310_720);
        assert_eq!(producer_advance_visit_bound, 1_310_720);
        assert_eq!(relation_wave_scan_visit_bound, 786_944);
        assert_eq!(committed_wave_bound, 512);
        assert_eq!(soft_restart_bound, 511);
    }

    fn deferred_tail_test_scoped_type() -> crate::pass::typecheck_core::ScopedType {
        crate::pass::typecheck_core::ScopedType::projected_for_test(crate::ast::Type::Unit {
            meta: Meta::new(crate::span::Span::new(0, 0)),
        })
    }

    fn deferred_tail_test_type() -> ProjectedType {
        let ty = deferred_tail_test_scoped_type();
        ProjectedType {
            seal: InvocationSeal::fresh(),
            ty: ty.clone(),
            specialization: ty,
            closed: None,
            focus: None,
            contains_focus: false,
            producers: ProducerSet::default(),
        }
    }

    #[test]
    fn fill_transcript_owners_release_on_the_default_stack() {
        let (value, weak) = std::thread::spawn(|| {
            let mut head = None;
            for _ in 0..30_000 {
                head = Some(crate::normalization::EvalRef::new(FillRelation {
                    previous: head,
                    destination: Value::Unit,
                    candidate: Value::Unit,
                    producers: ProducerSet::default(),
                }));
            }
            let weak = head.as_ref().expect("nonempty transcript").downgrade();
            let context = FillContext {
                seal: InvocationSeal::fresh(),
                head,
                len: 30_000,
            };
            (crate::normalization::fill_context_value(context), weak)
        })
        .join()
        .expect("transcript owner can escape its creating thread");
        drop(value);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn projected_recipe_and_producer_owners_release_on_the_default_stack() {
        let (value, recipe_weak, producers_weak) = std::thread::spawn(|| {
            let leaf = ProducerSet::leaf(0);
            let mut producers = ProducerSet::leaf(1);
            for _ in 0..30_000 {
                producers = producers.union(&leaf);
            }
            let producers_weak = producers
                .0
                .as_ref()
                .expect("nonempty producers")
                .downgrade();
            let mut ty = deferred_tail_test_type();
            ty.producers = producers;
            let unit = Arc::new(ProjectedCheckedRecipe {
                ty: ty.clone(),
                node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TemplateValue(0)),
            });
            let mut recipe = unit.clone();
            for _ in 0..30_000 {
                recipe = Arc::new(ProjectedCheckedRecipe {
                    ty: ty.clone(),
                    node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Pair(
                        recipe,
                        unit.clone(),
                    )),
                });
            }
            let recipe_weak = recipe.node.downgrade();
            let value = crate::normalization::projected_value(ProjectedValue(
                ProjectedValueKind::CheckedRecipe((*recipe).clone()),
            ));
            (value, recipe_weak, producers_weak)
        })
        .join()
        .expect("projected owners can escape their creating thread");
        let retained = value.clone();
        drop(value);
        assert!(recipe_weak.upgrade().is_some());
        assert!(producers_weak.upgrade().is_some());
        drop(retained);
        assert!(recipe_weak.upgrade().is_none());
        assert!(producers_weak.upgrade().is_none());
    }

    #[test]
    fn projected_local_rejects_a_foreign_peer_before_callback_construction() {
        let (proof, _, _) = specialization_term_for_test();
        let (_, _, foreign) = specialization_term_for_test();
        let ty = Value::ReflType(
            crate::normalization::ReflectedType::new(
                crate::ast::Type::<crate::ast::UncheckedPrime>::Unit {
                    meta: Meta::new(crate::span::Span::new(0, 0)),
                },
                "the foreign projected-local peer test unit must be closed",
            )
            .expect("reflect the foreign projected-local peer test unit"),
        );
        let foreign = crate::normalization::projected_value(foreign);
        assert!(
            projected_term_local(&proof, (&ty, &foreign), "__ct_let_test__".to_owned()).is_err()
        );
    }

    fn deferred_tail_test_recipe(node: ProjectedRecipeNode) -> Arc<ProjectedCheckedRecipe> {
        Arc::new(ProjectedCheckedRecipe {
            ty: deferred_tail_test_type(),
            node: crate::normalization::EvalRef::new(node),
        })
    }

    fn deferred_tail_test_template(index: u32) -> Arc<ProjectedCheckedRecipe> {
        deferred_tail_test_recipe(ProjectedRecipeNode::TemplateValue(index))
    }

    fn deferred_tail_test_local(name: &str) -> Arc<ProjectedCheckedRecipe> {
        deferred_tail_test_recipe(ProjectedRecipeNode::Local {
            name: name.to_owned(),
        })
    }

    fn deferred_tail_test_call(
        function: Arc<ProjectedCheckedRecipe>,
    ) -> Arc<ProjectedCheckedRecipe> {
        deferred_tail_test_recipe(ProjectedRecipeNode::TermCall {
            function_type: deferred_tail_test_type(),
            function,
            argument: deferred_tail_test_local("argument"),
        })
    }

    fn deferred_tail_test_candidate(
        index: u32,
        requirement: RecOrderTailRequirement,
    ) -> Option<DeferredTailCandidate> {
        Some(DeferredTailCandidate {
            token: crate::ast::NodeId(u64::from(index) + 1),
            requirement,
            lifted_flow: RecOrderTypeFlow::SynthesizedValue,
            span: crate::span::Span::new(index, index + 1),
        })
    }

    fn deferred_tail_test_usage(
        recipe: &ProjectedCheckedRecipe,
        candidates: &[Option<DeferredTailCandidate>],
    ) -> Vec<DeferredTailUsage> {
        let mut usage = vec![DeferredTailUsage::default(); candidates.len()];
        DeferredTailClassifier::new(candidates, &mut usage).fold(recipe, DeferredTailRole::Tail);
        usage
    }

    fn deferred_tail_test_resolution(
        decisions: &[Option<DeferredTailDecision>],
        index: usize,
    ) -> super::super::DeferredTailResolution {
        decisions[index]
            .expect("the test candidate must receive a decision")
            .1
    }

    #[test]
    fn deferred_tail_nested_type_apps_preserve_exact_candidate_identity() {
        let specialized = deferred_tail_test_recipe(ProjectedRecipeNode::TermTypeApp {
            term: deferred_tail_test_recipe(ProjectedRecipeNode::TermTypeApp {
                term: deferred_tail_test_template(0),
                argument: deferred_tail_test_scoped_type(),
            }),
            argument: deferred_tail_test_scoped_type(),
        });
        let recipe = deferred_tail_test_call(specialized);
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(usage[0].terminal);
        assert!(!usage[0].escape);
        let decisions = resolve_deferred_tail_decisions(&candidates, &usage).unwrap();
        assert_eq!(
            deferred_tail_test_resolution(&decisions, 0),
            super::super::DeferredTailResolution::Lifted(RecOrderTypeFlow::SynthesizedValue)
        );
    }

    #[test]
    fn deferred_tail_optional_escape_only_candidate_uses_ordinary_bypass() {
        let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "ignored".to_owned(),
            value: deferred_tail_test_call(deferred_tail_test_template(1)),
            body: deferred_tail_test_call(deferred_tail_test_template(0)),
        });
        let candidates = vec![
            deferred_tail_test_candidate(0, RecOrderTailRequirement::Required),
            deferred_tail_test_candidate(1, RecOrderTailRequirement::Optional),
        ];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(usage[0].terminal);
        assert!(!usage[0].escape);
        assert!(!usage[1].terminal);
        assert!(usage[1].escape);
        let decisions = resolve_deferred_tail_decisions(&candidates, &usage).unwrap();
        assert_eq!(
            deferred_tail_test_resolution(&decisions, 1),
            super::super::DeferredTailResolution::OrdinaryBypass
        );
    }

    #[test]
    fn deferred_tail_terminal_and_escape_use_is_rejected() {
        let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "ignored".to_owned(),
            value: deferred_tail_test_call(deferred_tail_test_template(0)),
            body: deferred_tail_test_call(deferred_tail_test_template(0)),
        });
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(usage[0].terminal);
        assert!(usage[0].escape);
        assert!(resolve_deferred_tail_decisions(&candidates, &usage).is_err());
    }

    #[test]
    fn deferred_tail_pair_alias_preserves_distinct_terminal_and_escape_roles() {
        let pair = deferred_tail_test_recipe(ProjectedRecipeNode::Pair(
            deferred_tail_test_template(0),
            deferred_tail_test_local("empty"),
        ));
        let escape = deferred_tail_test_call(deferred_tail_test_local("pair"));
        let tail = deferred_tail_test_call(deferred_tail_test_recipe(
            ProjectedRecipeNode::ProductProjection {
                product_type: deferred_tail_test_type(),
                product: deferred_tail_test_local("pair"),
                left: true,
            },
        ));
        let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "pair".to_owned(),
            value: pair,
            body: deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
                value_type: deferred_tail_test_type(),
                name: "ignored".to_owned(),
                value: escape,
                body: tail,
            }),
        });
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(usage[0].terminal);
        assert!(usage[0].escape);
        assert!(resolve_deferred_tail_decisions(&candidates, &usage).is_err());
    }

    #[test]
    fn deferred_tail_pair_arena_marks_each_role_once() {
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];
        let mut usage = vec![DeferredTailUsage::default(); candidates.len()];
        reset_deferred_tail_classifier_work();
        let mut classifier = DeferredTailClassifier::new(&candidates, &mut usage);
        let pair = classifier.pair(DeferredTailSymbol::Candidate(0), DeferredTailSymbol::Empty);
        classifier.mark(pair, DeferredTailRole::Tail);
        classifier.mark(pair, DeferredTailRole::Tail);
        classifier.mark(pair, DeferredTailRole::Escape);
        classifier.mark(pair, DeferredTailRole::Escape);
        drop(classifier);

        assert!(usage[0].terminal);
        assert!(usage[0].escape);
        assert!(resolve_deferred_tail_decisions(&candidates, &usage).is_err());
        let work = deferred_tail_classifier_work();
        assert_eq!(work.pair_nodes_allocated, 1);
        assert_eq!(work.pair_mark_visits, 2);
        assert_eq!(work.tail_pair_mark_visits, 1);
        assert_eq!(work.escape_pair_mark_visits, 1);
    }

    #[test]
    fn deferred_tail_pair_alias_marks_opaque_sibling_before_exact_projection() {
        let pair = deferred_tail_test_recipe(ProjectedRecipeNode::Pair(
            deferred_tail_test_template(0),
            deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicInjection {
                sum_type: deferred_tail_test_type(),
                value: deferred_tail_test_template(1),
                left: true,
            }),
        ));
        let projection = deferred_tail_test_recipe(ProjectedRecipeNode::ProductProjection {
            product_type: deferred_tail_test_type(),
            product: deferred_tail_test_local("pair"),
            left: true,
        });
        let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "pair".to_owned(),
            value: pair,
            body: deferred_tail_test_call(projection),
        });
        let candidates = vec![
            deferred_tail_test_candidate(0, RecOrderTailRequirement::Required),
            deferred_tail_test_candidate(1, RecOrderTailRequirement::Optional),
        ];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(usage[0].terminal);
        assert!(!usage[0].escape);
        assert!(!usage[1].terminal);
        assert!(usage[1].escape);
        let decisions = resolve_deferred_tail_decisions(&candidates, &usage).unwrap();
        assert_eq!(
            deferred_tail_test_resolution(&decisions, 0),
            super::super::DeferredTailResolution::Lifted(RecOrderTypeFlow::SynthesizedValue)
        );
        assert_eq!(
            deferred_tail_test_resolution(&decisions, 1),
            super::super::DeferredTailResolution::OrdinaryBypass
        );
    }

    #[test]
    fn deferred_tail_let_shadowing_restores_the_outer_exact_alias() {
        let shadowed_condition = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "candidate".to_owned(),
            value: deferred_tail_test_local("condition"),
            body: deferred_tail_test_local("candidate"),
        });
        let branches = deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicIfThenElse {
            condition: shadowed_condition,
            true_body: deferred_tail_test_call(deferred_tail_test_local("candidate")),
            false_body: deferred_tail_test_call(deferred_tail_test_local("candidate")),
        });
        let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "candidate".to_owned(),
            value: deferred_tail_test_template(0),
            body: branches,
        });
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(usage[0].terminal);
        assert!(!usage[0].escape);
        assert!(resolve_deferred_tail_decisions(&candidates, &usage).is_ok());
    }

    #[test]
    fn deferred_tail_cloned_children_cannot_contain_a_lifted_candidate() {
        let ordinary = || deferred_tail_test_local("ordinary");
        let escape_contexts = vec![
            (
                "call argument",
                deferred_tail_test_recipe(ProjectedRecipeNode::TermCall {
                    function_type: deferred_tail_test_type(),
                    function: ordinary(),
                    argument: deferred_tail_test_template(0),
                }),
            ),
            (
                "either scrutinee",
                deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicEither {
                    sum_type: deferred_tail_test_type(),
                    value: deferred_tail_test_template(0),
                    left_name: "left".to_owned(),
                    left_body: ordinary(),
                    right_name: "right".to_owned(),
                    right_body: ordinary(),
                }),
            ),
            (
                "if condition",
                deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicIfThenElse {
                    condition: deferred_tail_test_template(0),
                    true_body: ordinary(),
                    false_body: ordinary(),
                }),
            ),
            (
                "absurd bottom",
                deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicAbsurd {
                    bottom: deferred_tail_test_template(0),
                }),
            ),
        ];
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];

        for (context, escape) in escape_contexts {
            let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
                value_type: deferred_tail_test_type(),
                name: "ignored".to_owned(),
                value: escape,
                body: deferred_tail_test_call(deferred_tail_test_template(0)),
            });
            let usage = deferred_tail_test_usage(&recipe, &candidates);
            assert!(usage[0].terminal, "{context} fixture lost its terminal use");
            assert!(usage[0].escape, "{context} did not mark the cloned child");
            assert!(
                resolve_deferred_tail_decisions(&candidates, &usage).is_err(),
                "{context} admitted a candidate that the runtime adapter leaves unchanged"
            );
        }
    }

    #[test]
    fn deferred_tail_ordinary_terminal_nodes_cannot_hide_a_lifted_candidate() {
        let ordinary = || deferred_tail_test_local("ordinary");
        let escape_contexts = vec![
            ("bare value", deferred_tail_test_template(0)),
            (
                "pair",
                deferred_tail_test_recipe(ProjectedRecipeNode::Pair(
                    deferred_tail_test_template(0),
                    ordinary(),
                )),
            ),
            (
                "function body",
                deferred_tail_test_recipe(ProjectedRecipeNode::TermFn {
                    function_type: deferred_tail_test_type(),
                    names: vec!["argument".to_owned()],
                    body: deferred_tail_test_template(0),
                }),
            ),
            (
                "type-function body",
                deferred_tail_test_recipe(ProjectedRecipeNode::TermTypeFn {
                    param: crate::ast::TypeParam {
                        name: "A".to_owned(),
                        span: crate::span::Span::new(0, 0),
                        kind: None,
                    },
                    body: deferred_tail_test_template(0),
                }),
            ),
            (
                "type application",
                deferred_tail_test_recipe(ProjectedRecipeNode::TermTypeApp {
                    term: deferred_tail_test_template(0),
                    argument: deferred_tail_test_scoped_type(),
                }),
            ),
            (
                "injection payload",
                deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicInjection {
                    sum_type: deferred_tail_test_type(),
                    value: deferred_tail_test_template(0),
                    left: true,
                }),
            ),
        ];
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];

        for (context, escape) in escape_contexts {
            let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicIfThenElse {
                condition: ordinary(),
                true_body: deferred_tail_test_call(deferred_tail_test_template(0)),
                false_body: escape,
            });
            let usage = deferred_tail_test_usage(&recipe, &candidates);
            assert!(usage[0].terminal, "{context} fixture lost its terminal use");
            assert!(usage[0].escape, "{context} hid a non-call terminal use");
            assert!(
                resolve_deferred_tail_decisions(&candidates, &usage).is_err(),
                "{context} admitted a candidate under an ordinary right-wrapped node"
            );
        }
    }

    #[test]
    fn deferred_tail_branch_binders_shadow_and_restore_an_outer_alias() {
        let ordinary = || deferred_tail_test_local("ordinary");
        let shadowed = deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicEither {
            sum_type: deferred_tail_test_type(),
            value: ordinary(),
            left_name: "candidate".to_owned(),
            left_body: deferred_tail_test_call(deferred_tail_test_local("candidate")),
            right_name: "candidate".to_owned(),
            right_body: deferred_tail_test_call(deferred_tail_test_local("candidate")),
        });
        let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "candidate".to_owned(),
            value: deferred_tail_test_template(0),
            body: deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
                value_type: deferred_tail_test_type(),
                name: "ignored".to_owned(),
                value: shadowed,
                body: deferred_tail_test_call(deferred_tail_test_local("candidate")),
            }),
        });
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(usage[0].terminal);
        assert!(!usage[0].escape);
        assert!(resolve_deferred_tail_decisions(&candidates, &usage).is_ok());
    }

    #[test]
    fn deferred_tail_required_unused_alias_is_rejected() {
        let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "candidate".to_owned(),
            value: deferred_tail_test_template(0),
            body: deferred_tail_test_local("ordinary"),
        });
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(!usage[0].terminal);
        assert!(!usage[0].escape);
        assert!(resolve_deferred_tail_decisions(&candidates, &usage).is_err());
    }

    #[test]
    fn deferred_tail_unused_callee_alias_does_not_hide_a_terminal_use() {
        let public_callee = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "unused".to_owned(),
            value: deferred_tail_test_template(0),
            body: deferred_tail_test_local("ordinary_callback"),
        });
        let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicIfThenElse {
            condition: deferred_tail_test_local("condition"),
            true_body: deferred_tail_test_call(deferred_tail_test_template(0)),
            false_body: deferred_tail_test_call(public_callee),
        });
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(usage[0].terminal);
        assert!(!usage[0].escape);
        let decisions = resolve_deferred_tail_decisions(&candidates, &usage).unwrap();
        assert_eq!(
            deferred_tail_test_resolution(&decisions, 0),
            super::super::DeferredTailResolution::Lifted(RecOrderTypeFlow::SynthesizedValue)
        );
    }

    #[test]
    fn deferred_tail_unselected_pair_member_does_not_hide_a_terminal_use() {
        let pair = deferred_tail_test_recipe(ProjectedRecipeNode::Pair(
            deferred_tail_test_template(0),
            deferred_tail_test_local("ordinary_callback"),
        ));
        let public_callee = deferred_tail_test_recipe(ProjectedRecipeNode::ProductProjection {
            product_type: deferred_tail_test_type(),
            product: pair,
            left: false,
        });
        let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicIfThenElse {
            condition: deferred_tail_test_local("condition"),
            true_body: deferred_tail_test_call(deferred_tail_test_template(0)),
            false_body: deferred_tail_test_call(public_callee),
        });
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];
        let usage = deferred_tail_test_usage(&recipe, &candidates);

        assert!(usage[0].terminal);
        assert!(!usage[0].escape);
        let decisions = resolve_deferred_tail_decisions(&candidates, &usage).unwrap();
        assert_eq!(
            deferred_tail_test_resolution(&decisions, 0),
            super::super::DeferredTailResolution::Lifted(RecOrderTypeFlow::SynthesizedValue)
        );
    }

    #[test]
    fn deferred_tail_discarded_candidates_in_opaque_children_do_not_hide_a_terminal_use() {
        let hidden = || {
            deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
                value_type: deferred_tail_test_type(),
                name: "unused".to_owned(),
                value: deferred_tail_test_template(0),
                body: deferred_tail_test_local("ordinary_value"),
            })
        };
        let families = [
            (
                "call argument",
                deferred_tail_test_recipe(ProjectedRecipeNode::TermCall {
                    function_type: deferred_tail_test_type(),
                    function: deferred_tail_test_local("ordinary_function"),
                    argument: hidden(),
                }),
            ),
            (
                "either scrutinee",
                deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicEither {
                    sum_type: deferred_tail_test_type(),
                    value: hidden(),
                    left_name: "left".to_owned(),
                    left_body: deferred_tail_test_local("left_body"),
                    right_name: "right".to_owned(),
                    right_body: deferred_tail_test_local("right_body"),
                }),
            ),
            (
                "if condition",
                deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicIfThenElse {
                    condition: hidden(),
                    true_body: deferred_tail_test_local("true_body"),
                    false_body: deferred_tail_test_local("false_body"),
                }),
            ),
            (
                "absurd bottom",
                deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicAbsurd {
                    bottom: hidden(),
                }),
            ),
            (
                "function body",
                deferred_tail_test_recipe(ProjectedRecipeNode::TermFn {
                    function_type: deferred_tail_test_type(),
                    names: vec!["parameter".to_owned()],
                    body: hidden(),
                }),
            ),
            (
                "type-function body",
                deferred_tail_test_recipe(ProjectedRecipeNode::TermTypeFn {
                    param: crate::ast::TypeParam {
                        name: "A".to_owned(),
                        span: crate::span::Span::new(0, 0),
                        kind: None,
                    },
                    body: hidden(),
                }),
            ),
            (
                "injection value",
                deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicInjection {
                    sum_type: deferred_tail_test_type(),
                    value: hidden(),
                    left: true,
                }),
            ),
        ];
        let candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Required,
        )];

        for (label, opaque) in families {
            let recipe = deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicIfThenElse {
                condition: deferred_tail_test_local("outer_condition"),
                true_body: deferred_tail_test_call(deferred_tail_test_template(0)),
                false_body: opaque,
            });
            let usage = deferred_tail_test_usage(&recipe, &candidates);

            assert!(usage[0].terminal, "{label}");
            assert!(!usage[0].escape, "{label}");
            let decisions = resolve_deferred_tail_decisions(&candidates, &usage).unwrap();
            assert_eq!(
                deferred_tail_test_resolution(&decisions, 0),
                super::super::DeferredTailResolution::Lifted(RecOrderTypeFlow::SynthesizedValue),
                "{label}"
            );
        }
    }

    #[test]
    fn deferred_tail_classifier_work_is_linear_in_alias_depth_and_scope_width() {
        const PAIR_DEPTH: usize = 12;
        let final_pair = format!("pair{PAIR_DEPTH}");
        let mut pair_recipe = deferred_tail_test_local(&final_pair);
        for index in (0..=PAIR_DEPTH).rev() {
            let value = if index == 0 {
                deferred_tail_test_template(0)
            } else {
                let previous = format!("pair{}", index - 1);
                deferred_tail_test_recipe(ProjectedRecipeNode::Pair(
                    deferred_tail_test_local(&previous),
                    deferred_tail_test_local(&previous),
                ))
            };
            pair_recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
                value_type: deferred_tail_test_type(),
                name: format!("pair{index}"),
                value,
                body: pair_recipe,
            });
        }
        let pair_candidates = vec![deferred_tail_test_candidate(
            0,
            RecOrderTailRequirement::Optional,
        )];
        reset_deferred_tail_classifier_work();
        let pair_usage = deferred_tail_test_usage(&pair_recipe, &pair_candidates);
        let pair_work = deferred_tail_classifier_work();
        assert!(!pair_usage[0].terminal);
        assert!(pair_usage[0].escape);

        const SCOPE_WIDTH: usize = 64;
        const FN_DEPTH: usize = 64;
        let mut nested_fn = deferred_tail_test_local("shadow");
        for _ in 0..FN_DEPTH {
            nested_fn = deferred_tail_test_recipe(ProjectedRecipeNode::TermFn {
                function_type: deferred_tail_test_type(),
                names: vec!["shadow".to_owned()],
                body: nested_fn,
            });
        }
        let either = deferred_tail_test_recipe(ProjectedRecipeNode::IntrinsicEither {
            sum_type: deferred_tail_test_type(),
            value: deferred_tail_test_local("alias0"),
            left_name: "shadow".to_owned(),
            left_body: nested_fn,
            right_name: "shadow".to_owned(),
            right_body: deferred_tail_test_local("shadow"),
        });
        let mut scoped_recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "ignored".to_owned(),
            value: either,
            body: deferred_tail_test_call(deferred_tail_test_local("shadow")),
        });
        scoped_recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
            value_type: deferred_tail_test_type(),
            name: "shadow".to_owned(),
            value: deferred_tail_test_template(1),
            body: scoped_recipe,
        });
        for index in (0..SCOPE_WIDTH).rev() {
            scoped_recipe = deferred_tail_test_recipe(ProjectedRecipeNode::TermLet {
                value_type: deferred_tail_test_type(),
                name: format!("alias{index}"),
                value: deferred_tail_test_template(0),
                body: scoped_recipe,
            });
        }
        let scoped_candidates = vec![
            deferred_tail_test_candidate(0, RecOrderTailRequirement::Optional),
            deferred_tail_test_candidate(1, RecOrderTailRequirement::Required),
        ];
        reset_deferred_tail_classifier_work();
        let scoped_usage = deferred_tail_test_usage(&scoped_recipe, &scoped_candidates);
        let scoped_work = deferred_tail_classifier_work();
        assert!(!scoped_usage[0].terminal);
        assert!(scoped_usage[0].escape);
        assert!(scoped_usage[1].terminal);
        assert!(!scoped_usage[1].escape);

        assert_eq!(
            (
                pair_work.owned_symbol_nodes_cloned,
                scoped_work.alias_snapshot_entries,
            ),
            (0, 0),
            "alias handles and lexical scopes must never clone owned classifier state: pair={pair_work:?}, scoped={scoped_work:?}"
        );
        assert_eq!(pair_work.pair_nodes_allocated, PAIR_DEPTH);
        assert_eq!(
            pair_work.pair_mark_visits, PAIR_DEPTH,
            "the Escape-only fixture must mark each pair alias exactly once: {pair_work:?}"
        );
        assert_eq!(
            (
                pair_work.tail_pair_mark_visits,
                pair_work.escape_pair_mark_visits,
            ),
            (0, PAIR_DEPTH),
            "the pair fixture must exercise only the Escape role: {pair_work:?}"
        );
        assert!(
            pair_work.recipe_visits <= PAIR_DEPTH * 4 + 3,
            "the doubling alias recipe must stay a linear recipe walk: {pair_work:?}"
        );
        assert!(
            scoped_work.recipe_visits <= (SCOPE_WIDTH + FN_DEPTH) * 4 + 32,
            "nested lexical scopes must stay a linear recipe walk: {scoped_work:?}"
        );
        assert!(
            scoped_work.scope_binding_updates <= (SCOPE_WIDTH + FN_DEPTH) * 2 + 8,
            "nested lexical scopes must update only their authored bindings: {scoped_work:?}"
        );
    }

    #[test]
    fn deferred_tail_failure_returns_no_installable_partial_decisions() {
        let candidates = vec![
            deferred_tail_test_candidate(0, RecOrderTailRequirement::Optional),
            deferred_tail_test_candidate(1, RecOrderTailRequirement::Required),
        ];
        let usage = vec![
            DeferredTailUsage {
                terminal: true,
                escape: false,
            },
            DeferredTailUsage::default(),
        ];

        assert!(resolve_deferred_tail_decisions(&candidates, &usage).is_err());
    }

    #[test]
    fn retained_lambda_header_peel_confines_openness_to_the_body_result() {
        let span = crate::span::Span::new(0, 0);
        let unit = || crate::ast::Type::Unit {
            meta: Meta::new(span),
        };
        let goal = || crate::ast::Type::Goal {
            goal: crate::ast::TypeGoalRef::for_test(19, 0, 0),
            args: Vec::new(),
            meta: Meta::new(span),
            ext: (),
        };
        let infer = || crate::ast::Type::Infer {
            meta: Meta::new(span),
            ext: (),
        };
        let value = |name: &str, ty| crate::ast::Param {
            name: name.to_owned(),
            ty: Some(ty),
            pattern: (),
            meta: Meta::new(span),
        };

        let implicit_empty = crate::ast::Signature::from_groups(Vec::new());
        let implicit_ty = implicit_empty.signature_ty(goal(), span);
        let crate::ast::Type::Function {
            param, abi_arity, ..
        } = &implicit_ty
        else {
            panic!("a signature without value groups needs its implicit function layer")
        };
        assert!(matches!(param.as_ref(), crate::ast::Type::Unit { .. }));
        assert_eq!(*abi_arity, 0);
        assert!(lambda_header_leaves_only_open_result(
            &implicit_empty,
            &implicit_ty,
        ));

        let explicit_empty =
            crate::ast::Signature::from_groups(vec![crate::ast::SignatureGroup::Value(Vec::new())]);
        let explicit_empty_ty = explicit_empty.signature_ty(goal(), span);
        assert!(lambda_header_leaves_only_open_result(
            &explicit_empty,
            &explicit_empty_ty,
        ));

        let unit_param = crate::ast::Signature::from_groups(vec![
            crate::ast::SignatureGroup::Value(vec![value("unit", unit())]),
        ]);
        let unit_param_ty = unit_param.signature_ty(goal(), span);
        let crate::ast::Type::Function { abi_arity, .. } = &unit_param_ty else {
            panic!("a sole Unit parameter needs one function layer")
        };
        assert_eq!(*abi_arity, 0);
        assert!(lambda_header_leaves_only_open_result(
            &unit_param,
            &unit_param_ty,
        ));

        let product = crate::ast::Type::Product {
            left: Box::new(unit()),
            right: Box::new(unit()),
            meta: Meta::new(span),
        };
        let product_param = crate::ast::Signature::from_groups(vec![
            crate::ast::SignatureGroup::Value(vec![value("pair", product)]),
        ]);
        let product_param_ty = product_param.signature_ty(goal(), span);
        let crate::ast::Type::Function { abi_arity, .. } = &product_param_ty else {
            panic!("a product-valued parameter needs one function layer")
        };
        assert_eq!(*abi_arity, 1);
        assert!(lambda_header_leaves_only_open_result(
            &product_param,
            &product_param_ty,
        ));

        let interleaved = crate::ast::Signature::from_groups(vec![
            crate::ast::SignatureGroup::Type(vec![crate::ast::TypeParam {
                name: "F".to_owned(),
                span,
                kind: Some(crate::ast::Kind::arrow_chain(1)),
            }]),
            crate::ast::SignatureGroup::Value(vec![value("first", unit())]),
            crate::ast::SignatureGroup::Type(vec![crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            }]),
            crate::ast::SignatureGroup::Value(vec![
                value("second", unit()),
                value("third", unit()),
            ]),
        ]);
        let interleaved_ty = interleaved.signature_ty(goal(), span);
        assert!(lambda_header_leaves_only_open_result(
            &interleaved,
            &interleaved_ty,
        ));
        let mut wrong_hkt_kind = interleaved_ty.clone();
        let crate::ast::Type::Forall { param, .. } = &mut wrong_hkt_kind else {
            panic!("the interleaved signature lost its leading HKT binder")
        };
        param.kind = None;
        assert!(!lambda_header_leaves_only_open_result(
            &interleaved,
            &wrong_hkt_kind,
        ));

        let mut wrong_unit_abi = unit_param_ty.clone();
        let crate::ast::Type::Function { abi_arity, .. } = &mut wrong_unit_abi else {
            panic!("the Unit parameter signature lost its function layer")
        };
        *abi_arity = 1;
        assert!(!lambda_header_leaves_only_open_result(
            &unit_param,
            &wrong_unit_abi,
        ));

        let open_param = crate::ast::Signature::from_groups(vec![
            crate::ast::SignatureGroup::Value(vec![value("open", goal())]),
        ]);
        assert!(!lambda_header_leaves_only_open_result(
            &open_param,
            &open_param.signature_ty(goal(), span),
        ));
        let later_open_param = crate::ast::Signature::from_groups(vec![
            crate::ast::SignatureGroup::Value(vec![value("closed", unit()), value("open", goal())]),
        ]);
        assert!(!lambda_header_leaves_only_open_result(
            &later_open_param,
            &later_open_param.signature_ty(goal(), span),
        ));
        assert!(!lambda_header_leaves_only_open_result(
            &implicit_empty,
            &implicit_empty.signature_ty(infer(), span),
        ));
        assert!(!lambda_header_leaves_only_open_result(
            &implicit_empty,
            &implicit_empty.signature_ty(unit(), span),
        ));
    }

    fn fixture(source: &str, module_name: &str) -> Package<Lowered> {
        let parsed = parse(source).expect("fills certificate fixture parses");
        let (modules, _) = FullPipeline::lower_package(
            vec![(PathBuf::from(format!("{module_name}.kio")), parsed)],
            None,
        )
        .expect("fills certificate fixture lowers");
        let package =
            Package::build(Path::new(""), modules, None).expect("fills certificate package");
        package
            .resolve_imports()
            .expect("fills certificate fixture resolves");
        crate::pass::alpha_normalize::normalize_package(&package)
            .into_parts()
            .0
    }

    #[test]
    fn canonical_pair_certificate_is_unique_in_the_intrinsic_catalogue() {
        let package = fixture(
            "module certificate_catalogue; \
             import __intrinsics__; \
             host type Bool role(bool);",
            "certificate_catalogue",
        );
        let module = &package
            .module("certificate_catalogue")
            .expect("certificate catalogue module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("certificate catalogue environment");
        let span = crate::span::Span::new(0, 0);
        let paths = crate::pass::resolve::PRIME_INTRINSICS
            .iter()
            .map(|name| Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new((*name).to_owned(), span)],
                meta: Meta::new(span),
                ext: (),
            })
            .collect::<Vec<_>>();
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let certified = crate::pass::resolve::PRIME_INTRINSICS
            .iter()
            .zip(&paths)
            .filter(|(_, path)| {
                let direct = super::super::PreparedDirectCallee::path(path, &mut tcx)
                    .expect("resolve intrinsic catalogue entry")
                    .expect("intrinsic catalogue entries are direct");
                canonical_pair_certificate(&direct, &tcx)
            })
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        assert_eq!(certified, ["__pair__"]);
    }

    #[test]
    fn ordinary_functions_never_gain_pair_transparency_from_their_scheme_or_body() {
        let package = fixture(
            "module ordinary_pair_controls; \
             fn exact[A][B](left: A, right: B) -> A & B { (left, right) } \
             fn reordered[A](left: A, right: A) -> A & A { (right, left) } \
             fn duplicated[A](left: A, _right: A) -> A & A { (left, left) }",
            "ordinary_pair_controls",
        );
        let module = &package
            .module("ordinary_pair_controls")
            .expect("ordinary pair control module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("ordinary pair control environment");
        let paths = ["exact", "reordered", "duplicated"].map(|name| {
            let def = env.fn_defs[name];
            Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new(name.to_owned(), def.meta.span)],
                meta: Meta::new(def.meta.span),
                ext: (),
            }
        });
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        for (name, path) in ["exact", "reordered", "duplicated"].into_iter().zip(&paths) {
            let direct = super::super::PreparedDirectCallee::path(path, &mut tcx)
                .expect("resolve ordinary pair control")
                .expect("ordinary pair control is direct");
            assert!(matches!(
                direct.binders.last(),
                Some((_, ResolvedBinder::Fn { .. }))
            ));
            assert!(
                !canonical_pair_certificate(&direct, &tcx),
                "ordinary `{name}` gained intrinsic pair transparency"
            );
            if name == "exact" {
                assert!(
                    canonical_pair_scheme(&direct, &tcx),
                    "the exact-scheme control must make the resolved-kind gate causal"
                );
            }
        }
    }

    #[test]
    fn projected_composition_uses_the_seal_and_exact_scope_not_creation_owner() {
        let package = fixture(
            "module projected_creation_owner; fn witness() -> . { () }",
            "projected_creation_owner",
        );
        let module = &package
            .module("projected_creation_owner")
            .expect("projected creation-owner module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected creation-owner environment");
        let source = &env.fn_defs["witness"].body;
        let span = source.span();
        let mut elaborations = Elaborations::new();
        let tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            span,
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open projected creation-owner root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                span,
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve projected creation-owner premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open projected creation-owner premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let unit = crate::ast::Type::Unit {
            meta: crate::ast::Meta::new(span),
        };
        let function = crate::ast::Type::Function {
            param: Box::new(unit.clone()),
            ret: Box::new(unit.clone()),
            abi_arity: 1,
            caps: (),
            meta: crate::ast::Meta::new(span),
        };
        let (_function_reservation, (function_owner, function_ty)) = frontier
            .reserve_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                span,
                &tcx,
                |store, owner| {
                    let ty = store.scoped_type(
                        owner,
                        crate::pass::typecheck_core::InternedType::fresh_canonical(function),
                        span,
                    )?;
                    Ok((owner, ty))
                },
            )
            .expect("scope function in its retained source owner");
        let (_argument_reservation, (argument_owner, argument_ty)) = frontier
            .reserve_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                span,
                &tcx,
                |store, owner| {
                    let ty = store.scoped_type(
                        owner,
                        crate::pass::typecheck_core::InternedType::fresh_canonical(unit),
                        span,
                    )?;
                    Ok((owner, ty))
                },
            )
            .expect("scope argument in another retained source owner");
        assert_ne!(function_owner, argument_owner);

        let seal = InvocationSeal::fresh();
        let proof = MarkedComptimeProof { seal: seal.clone() };
        let projected_type =
            |seal: InvocationSeal, ty: crate::pass::typecheck_core::ScopedType| ProjectedType {
                seal,
                specialization: ty.clone(),
                ty,
                closed: None,
                focus: None,
                contains_focus: false,
                producers: ProducerSet::default(),
            };
        let function_type = projected_type(seal.clone(), function_ty.clone());
        let function_term = ProjectedCheckedRecipe {
            ty: function_type.clone(),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                name: "function".to_owned(),
            }),
        };
        let argument_type = projected_type(seal.clone(), argument_ty);
        let argument_term = ProjectedCheckedRecipe {
            ty: argument_type.clone(),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                name: "argument".to_owned(),
            }),
        };
        let call = projected_term_call(
            &proof,
            &crate::normalization::projected_value(ProjectedValue(ProjectedValueKind::Type(
                function_type.clone(),
            ))),
            &crate::normalization::projected_value(ProjectedValue(
                ProjectedValueKind::CheckedRecipe(function_term),
            )),
            &crate::normalization::projected_value(ProjectedValue(
                ProjectedValueKind::CheckedRecipe(argument_term),
            )),
        )
        .expect("compose same-seal exact snapshots from distinct creation owners");
        assert!(matches!(
            call.0,
            ProjectedValueKind::CheckedRecipe(ProjectedCheckedRecipe {
                node,
                ..
            }) if matches!(node.as_ref(), ProjectedRecipeNode::TermCall { .. })
        ));

        let (_foreign_store, foreign_store_ty) =
            super::super::frontier::OrdinaryFrontier::begin_root_with(
                span,
                &tcx,
                |store, owner| {
                    store.scoped_type(
                        owner,
                        crate::pass::typecheck_core::InternedType::fresh_canonical(
                            crate::ast::Type::Unit {
                                meta: crate::ast::Meta::new(span),
                            },
                        ),
                        span,
                    )
                },
            )
            .expect("scope an argument in another goal store");
        let cross_store_argument = ProjectedCheckedRecipe {
            ty: projected_type(seal.clone(), foreign_store_ty),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                name: "cross_store_argument".to_owned(),
            }),
        };
        assert!(
            projected_term_call(
                &proof,
                &crate::normalization::projected_value(ProjectedValue(ProjectedValueKind::Type(
                    function_type.clone(),
                ))),
                &crate::normalization::projected_value(ProjectedValue(
                    ProjectedValueKind::CheckedRecipe(ProjectedCheckedRecipe {
                        ty: function_type.clone(),
                        node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                            name: "function".to_owned(),
                        }),
                    }),
                )),
                &crate::normalization::projected_value(ProjectedValue(
                    ProjectedValueKind::CheckedRecipe(cross_store_argument),
                )),
            )
            .is_err(),
            "a matching invocation seal must not authorize a type from another goal store"
        );

        let foreign_argument = ProjectedCheckedRecipe {
            ty: projected_type(InvocationSeal::fresh(), argument_type.ty.clone()),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                name: "foreign_argument".to_owned(),
            }),
        };
        assert!(
            projected_term_call(
                &proof,
                &crate::normalization::projected_value(ProjectedValue(ProjectedValueKind::Type(
                    function_type.clone(),
                ))),
                &crate::normalization::projected_value(ProjectedValue(
                    ProjectedValueKind::CheckedRecipe(ProjectedCheckedRecipe {
                        ty: function_type,
                        node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                            name: "function".to_owned(),
                        }),
                    }),
                )),
                &crate::normalization::projected_value(ProjectedValue(
                    ProjectedValueKind::CheckedRecipe(foreign_argument),
                )),
            )
            .is_err()
        );

        let left_scope = crate::pass::typecheck_core::ScopedType::projected_noncanonical_for_test(
            crate::pass::typecheck_core::ty_path("A", span),
            "projected_creation_owner",
            vec![("A".to_owned(), crate::ast::Kind::Star, 7)],
        );
        let right_scope = crate::pass::typecheck_core::ScopedType::projected_noncanonical_for_test(
            crate::pass::typecheck_core::ty_path("A", span),
            "projected_creation_owner",
            vec![("A".to_owned(), crate::ast::Kind::Star, 8)],
        );
        assert!(
            projected_type_product(
                &proof,
                &crate::normalization::projected_value(ProjectedValue(ProjectedValueKind::Type(
                    projected_type(seal.clone(), left_scope),
                ))),
                &crate::normalization::projected_value(ProjectedValue(ProjectedValueKind::Type(
                    projected_type(seal, right_scope),
                ))),
            )
            .is_err()
        );
    }

    #[test]
    fn projected_type_layout_drops_the_redundant_creation_owner() {
        enum LegacyCreationOwner {
            Live(crate::ast::TypeGoalOwner),
            Test,
        }
        struct LegacyProjectedType {
            seal: InvocationSeal,
            owner: LegacyCreationOwner,
            ty: crate::pass::typecheck_core::ScopedType,
            specialization: crate::pass::typecheck_core::ScopedType,
            closed: Option<crate::normalization::ReflectedType>,
            focus: Option<ProjectedFocus>,
            contains_focus: bool,
            producers: ProducerSet,
        }
        fn read_legacy_projected_type(value: LegacyProjectedType) -> bool {
            let LegacyProjectedType {
                seal,
                owner,
                ty,
                specialization,
                closed,
                focus,
                contains_focus,
                producers,
            } = value;
            let owner_is_live = match owner {
                LegacyCreationOwner::Live(owner) => {
                    std::hint::black_box(owner);
                    true
                }
                LegacyCreationOwner::Test => false,
            };
            std::hint::black_box(seal);
            std::hint::black_box(ty);
            std::hint::black_box(specialization);
            std::hint::black_box(closed);
            std::hint::black_box(focus);
            std::hint::black_box(contains_focus);
            std::hint::black_box(producers);
            owner_is_live
        }

        let span = crate::span::Span::new(0, 0);
        let scoped =
            crate::pass::typecheck_core::ScopedType::projected_for_test(crate::ast::Type::Unit {
                meta: crate::ast::Meta::new(span),
            });
        let reflected = crate::normalization::ReflectedType::new(
            crate::ast::Type::Unit {
                meta: crate::ast::Meta::new(span),
            },
            "legacy projected-type layout fixture",
        )
        .expect("reflect an infer-free test type");
        let focus = ProjectedFocus {
            result_root: ProjectedResultRoot(0),
            path: Arc::from([ProjectedTypeEdge::FunctionReturn]),
        };
        let legacy = |owner| LegacyProjectedType {
            seal: InvocationSeal::fresh(),
            owner,
            ty: scoped.clone(),
            specialization: scoped.clone(),
            closed: Some(reflected.clone()),
            focus: Some(focus.clone()),
            contains_focus: true,
            producers: ProducerSet::leaf(0),
        };
        assert!(read_legacy_projected_type(legacy(
            LegacyCreationOwner::Live(crate::ast::TypeGoalRef::for_test(0, 0, 0).owner()),
        )));
        assert!(!read_legacy_projected_type(legacy(
            LegacyCreationOwner::Test,
        )));
        assert!(
            std::mem::size_of::<ProjectedType>() < std::mem::size_of::<LegacyProjectedType>(),
            "removing creation-owner bookkeeping must shrink each projected type"
        );
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(std::mem::size_of::<ProjectedType>(), 248);
            assert_eq!(std::mem::size_of::<ProjectedCheckedRecipe>(), 256);
            assert_eq!(std::mem::size_of::<ProjectedCheckedResult>(), 256);
            assert_eq!(std::mem::size_of::<ProjectedValue>(), 256);
        }
    }

    #[test]
    fn projected_open_function_closes_only_its_goal_free_parameter_view() {
        let span = crate::span::Span::new(0, 0);
        let reflected_param = crate::normalization::ReflectedType::new(
            crate::ast::Type::synth_path(vec!["A".to_owned()], Vec::new(), span),
            "the projected parameter fixture must be infer-free",
        )
        .expect("reflect projected parameter fixture");
        let (proof, foreign, parent) = open_function_reflection_for_test(reflected_param);
        assert!(parent.reflection_candidate(&proof).is_none());
        let ProjectedTypeView::Function { param, ret, .. } =
            projected_type_view(&proof, &parent).expect("view projected open function")
        else {
            panic!("the projected open function lost its structural shell")
        };
        assert!(param.reflection_candidate(&proof).is_some());
        assert!(param.reflection_candidate(&foreign).is_none());
        assert!(ret.reflection_candidate(&proof).is_none());
        assert!(open_function_reflection_evidence_for_test(
            &parent, &param, &ret
        ));
    }

    #[test]
    fn projected_either_defers_authenticated_open_branch_coherence_until_close() {
        let (proof, sum_type, result_type, scrutinee, left_body, right_body) =
            open_either_reflection_for_test();
        let (prepared, _left_payload, _right_payload) = projected_intrinsic_either_prepare(
            &proof,
            &crate::normalization::projected_value(sum_type),
            &crate::normalization::projected_value(result_type),
            &crate::normalization::projected_value(scrutinee),
            "left".to_owned(),
            "right".to_owned(),
        )
        .expect("prepare authenticated open projected either");
        let either = projected_intrinsic_either_finish(
            &proof,
            prepared,
            &crate::normalization::projected_value(left_body),
            &crate::normalization::projected_value(right_body),
        )
        .expect("retain open branch coherence until the live goals close");
        assert!(deferred_either_evidence_for_test(&proof, &either));
    }

    #[test]
    fn projected_either_closes_live_branch_goals_and_rejects_a_closed_mismatch() {
        let package = fixture(
            "module projected_either_close; fn witness() -> . { () }",
            "projected_either_close",
        );
        let module = &package
            .module("projected_either_close")
            .expect("projected either close module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected either close environment");
        let source = &env.fn_defs["witness"].body;
        let span = source.span();
        let mut elaborations = Elaborations::new();
        let tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            span,
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open projected either close root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                span,
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve projected either close premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open projected either close premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let left_goal = frontier
            .reserve_projected_header_hole(span)
            .expect("reserve left branch result");
        let right_goal = frontier
            .reserve_projected_header_hole(span)
            .expect("reserve right branch result");
        let unit_ty = crate::ast::Type::Unit {
            meta: crate::ast::Meta::new(span),
        };
        let bottom_ty = crate::ast::Type::Bottom {
            meta: crate::ast::Meta::new(span),
        };
        let sum_ty = crate::ast::Type::Sum {
            left: Box::new(unit_ty.clone()),
            right: Box::new(unit_ty.clone()),
            meta: crate::ast::Meta::new(span),
        };
        let mut scoped = |ty| {
            let (store, owner, _delta, _publication) = frontier.parts_mut();
            store
                .scoped_type(
                    owner,
                    crate::pass::typecheck_core::InternedType::fresh_canonical(ty),
                    span,
                )
                .expect("scope projected either close type")
        };
        let unit = scoped(unit_ty);
        let bottom = scoped(bottom_ty);
        let sum = scoped(sum_ty);
        {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store
                .constrain(delta, left_goal.clone(), unit.clone(), span, &tcx)
                .expect("close left branch result");
            store
                .constrain(delta, right_goal.clone(), unit.clone(), span, &tcx)
                .expect("close right branch result");
        }
        let seal = InvocationSeal::fresh();
        let projected_type = |ty: crate::pass::typecheck_core::ScopedType| ProjectedType {
            seal: seal.clone(),
            specialization: ty.clone(),
            ty,
            closed: None,
            focus: None,
            contains_focus: false,
            producers: ProducerSet::default(),
        };
        let local = |name: &str, ty| ProjectedCheckedRecipe {
            ty: projected_type(ty),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                name: name.to_owned(),
            }),
        };
        let either = |left_ty, right_ty| ProjectedCheckedRecipe {
            ty: projected_type(unit.clone()),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::IntrinsicEither {
                sum_type: projected_type(sum.clone()),
                value: Arc::new(local("sum", sum.clone())),
                left_name: "left".to_owned(),
                left_body: Arc::new(local("left_result", left_ty)),
                right_name: "right".to_owned(),
                right_body: Arc::new(local("right_result", right_ty)),
            }),
        };
        let checked = close_projected_recipe(
            &either(left_goal, right_goal),
            Vec::new(),
            span,
            &mut frontier,
            &tcx,
        )
        .expect("close projected either after both live branch goals resolve");
        assert!(matches!(checked.ty(), crate::ast::Type::Unit { .. }));

        let error = close_projected_recipe(
            &either(unit.clone(), bottom),
            Vec::new(),
            span,
            &mut frontier,
            &tcx,
        )
        .expect_err("reject a closed projected either branch mismatch");
        assert!(
            format!("{error:?}")
                .contains("__intrinsic_either__ branch did not have the supplied result type")
        );
    }

    #[test]
    fn projected_recipe_close_visits_each_occurrence_once_preserves_type_fn_and_checks_kind() {
        let package = fixture(
            "module projected_recipe_close; fn witness() -> . { () }",
            "projected_recipe_close",
        );
        let module = &package
            .module("projected_recipe_close")
            .expect("projected recipe close module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected recipe close environment");
        let source = &env.fn_defs["witness"].body;
        let span = source.span();
        let mut elaborations = Elaborations::new();
        let tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            span,
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open projected recipe close root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                span,
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve projected recipe close premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open projected recipe close premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let unit_ty = crate::ast::Type::Unit {
            meta: crate::ast::Meta::new(span),
        };
        let function_ty = crate::ast::Type::Function {
            param: Box::new(unit_ty.clone()),
            ret: Box::new(unit_ty.clone()),
            abi_arity: 1,
            caps: (),
            meta: crate::ast::Meta::new(span),
        };
        let sum_ty = crate::ast::Type::Sum {
            left: Box::new(unit_ty.clone()),
            right: Box::new(unit_ty.clone()),
            meta: crate::ast::Meta::new(span),
        };
        let forall_ty = crate::ast::Type::Forall {
            param: crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            },
            body: Box::new(unit_ty.clone()),
            meta: crate::ast::Meta::new(span),
        };
        let (unit, function, sum, forall) = {
            let mut scoped = |ty| {
                let (store, owner, _delta, _publication) = frontier.parts_mut();
                store
                    .scoped_type(
                        owner,
                        crate::pass::typecheck_core::InternedType::fresh_canonical(ty),
                        span,
                    )
                    .expect("scope projected recipe close type")
            };
            (
                scoped(unit_ty),
                scoped(function_ty),
                scoped(sum_ty),
                scoped(forall_ty),
            )
        };

        let seal = InvocationSeal::fresh();
        let projected_type = |ty: crate::pass::typecheck_core::ScopedType| ProjectedType {
            seal: seal.clone(),
            specialization: ty.clone(),
            ty,
            closed: None,
            focus: None,
            contains_focus: false,
            producers: ProducerSet::default(),
        };
        let local = |name: &str, ty| ProjectedCheckedRecipe {
            ty: projected_type(ty),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::Local {
                name: name.to_owned(),
            }),
        };
        let call_body = ProjectedCheckedRecipe {
            ty: projected_type(unit.clone()),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermCall {
                function_type: projected_type(function.clone()),
                function: Arc::new(local("callee", function.clone())),
                argument: Arc::new(local("argument", unit.clone())),
            }),
        };
        let left_body = ProjectedCheckedRecipe {
            ty: projected_type(function.clone()),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermFn {
                function_type: projected_type(function.clone()),
                names: vec!["left_arg".to_owned()],
                body: Arc::new(call_body),
            }),
        };
        let right_body = ProjectedCheckedRecipe {
            ty: projected_type(function.clone()),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermFn {
                function_type: projected_type(function.clone()),
                names: vec!["right_arg".to_owned()],
                body: Arc::new(local("right_result", unit.clone())),
            }),
        };
        let either = ProjectedCheckedRecipe {
            ty: projected_type(function.clone()),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::IntrinsicEither {
                sum_type: projected_type(sum.clone()),
                value: Arc::new(local("scrutinee", sum)),
                left_name: "left".to_owned(),
                left_body: Arc::new(left_body),
                right_name: "right".to_owned(),
                right_body: Arc::new(right_body),
            }),
        };
        PROJECTED_RECIPE_CLOSE_WORK.with(|work| work.set((0, 0)));
        close_projected_recipe(&either, Vec::new(), span, &mut frontier, &tcx)
            .expect("close nested projected Either/Fn/Call recipe");
        PROJECTED_RECIPE_CLOSE_WORK.with(|work| {
            assert_eq!(
                work.get(),
                (12, 12),
                "each recipe root and retained declared type closes and reflects once",
            );
        });

        let type_fn = ProjectedCheckedRecipe {
            ty: projected_type(forall.clone()),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermTypeFn {
                param: crate::ast::TypeParam {
                    name: "_ct_type_n99".to_owned(),
                    span,
                    kind: None,
                },
                body: Arc::new(local("type_fn_body", unit.clone())),
            }),
        };
        let checked_type_fn =
            close_projected_recipe(&type_fn, Vec::new(), span, &mut frontier, &tcx)
                .expect("close projected type function");
        assert!(matches!(
            checked_type_fn.ty(),
            crate::ast::Type::Forall { body, .. }
                if matches!(body.as_ref(), crate::ast::Type::Unit { .. })
        ));
        let crate::ast::Expr::Call { callee, args, .. } = checked_type_fn.clone_expr() else {
            panic!("projected type function did not close through its Unit call")
        };
        assert!(args.is_empty());
        let crate::ast::Expr::FnExpr { sig, .. } = callee.as_ref() else {
            panic!("projected type-function call did not retain its inner function")
        };
        assert_eq!(
            sig.groups,
            vec![
                crate::ast::SignatureGroupKind::Value { len: 0 },
                crate::ast::SignatureGroupKind::Type { len: 1 },
            ]
        );
        let pre_prime = crate::ast::convert_expr::<
            crate::ast::UncheckedPrime,
            crate::pass::substitute::PrePrime,
        >(&checked_type_fn.clone_expr());
        let replayed = crate::pass::substitute::finish_replayed_elaboration_for_test(pre_prime);
        let crate::ast::Expr::Call { callee, .. } = replayed else {
            panic!("projected type function did not survive replay")
        };
        let crate::ast::Expr::FnExpr { sig, .. } = callee.as_ref() else {
            panic!("replayed projected type function lost its inner function")
        };
        let crate::ast::SignatureParam::Type(param) = &sig.params[0] else {
            panic!("replayed projected type function lost its binder")
        };
        assert_eq!(param.name, "Ct");

        let wrong_kind = ProjectedCheckedRecipe {
            ty: projected_type(forall),
            node: crate::normalization::EvalRef::new(ProjectedRecipeNode::TermTypeFn {
                param: crate::ast::TypeParam {
                    name: "B".to_owned(),
                    span,
                    kind: Some(crate::ast::Kind::arrow_chain(1)),
                },
                body: Arc::new(local("unused_binder_body", unit)),
            }),
        };
        let error = close_projected_recipe(&wrong_kind, Vec::new(), span, &mut frontier, &tcx)
            .expect_err("reject a mismatched kind even when the type binder is unused");
        assert!(format!("{error:?}").contains("type function binder had the wrong kind"));
    }

    #[test]
    fn direct_lambda_projection_seals_its_header_before_entering_the_body() {
        let package = fixture(
            "module direct_lambda_projection; \
             fn witness() -> . -> . { .(value: .) -> . { value } }",
            "direct_lambda_projection",
        );
        let module = &package
            .module("direct_lambda_projection")
            .expect("direct-lambda projection module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("direct-lambda projection environment");
        let source = &env.fn_defs["witness"].body;
        let expected = env.fn_defs["witness"].ret.clone();
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            source.span(),
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open direct-lambda projection root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve projected call premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open projected call premise");
        let mut frontier = root.retained_value_frontier(&mut call);

        super::super::reset_direct_lambda_work();
        let mut staged = stage_operand(
            source,
            super::super::OwnerBoundType::Plain(crate::pass::typecheck_core::InternedType::fresh(
                expected.clone(),
            )),
            &mut frontier,
            &mut tcx,
        )
        .expect("stage direct lambda header");
        assert!(matches!(staged, StagedOperand::Lambda(..)));
        assert_eq!(
            super::super::direct_lambda_work().body_entries,
            0,
            "projecting a direct lambda must not enter its body"
        );

        let mut next_template = 0;
        let mut next_producer = 0;
        let mut locations = Vec::new();
        reserve_projected_source(
            &mut staged,
            0,
            &mut next_template,
            &mut next_producer,
            &mut locations,
        );
        let seal = InvocationSeal::fresh();
        let mut specialization =
            crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(&tcx);
        let fallback = frontier
            .store()
            .scoped_type(
                frontier.owner(),
                crate::pass::typecheck_core::InternedType::fresh(expected),
                source.span(),
            )
            .expect("scope direct-lambda public header");
        let recipe = seal_projected_operand_for_test(
            &mut staged,
            fallback,
            &seal,
            &mut specialization,
            &mut frontier,
            &mut tcx,
        )
        .expect("seal direct-lambda projected recipe");
        assert!(recipe.ty.producers.0.is_some());
        assert_eq!(
            super::super::direct_lambda_work().body_entries,
            0,
            "sealing the projected recipe must still leave the body parked"
        );

        let (staged, complete) = advance_staged_producer(
            staged,
            super::super::frontier::FrontierMode::ExpectedOnly,
            false,
            &mut frontier,
            &mut tcx,
        )
        .expect("resume the direct-lambda producer");
        assert!(complete);
        assert!(
            matches!(staged, StagedOperand::OpenCompleted { .. }),
            "the parked lambda must complete without closing through the later action frontier"
        );
    }

    #[test]
    fn synthesized_projected_lambda_keeps_qualified_provider_identity_through_sealing() {
        let parsed = [
            (
                "dep/core.kio",
                "module dep/core; \
                 pub newtype Box[A] : A { pub constructor make; pub projector get; };",
            ),
            (
                "dep/core/core.kio",
                "module dep/core/core; \
                 pub newtype Box[A] : . { pub constructor make; pub projector get; };",
            ),
            (
                "consumer.kio",
                "module consumer; \
                 import dep/core as dep; \
                 fn witness() -> dep.Box(.) -> dep.Box(.) { .(boxed: dep.Box(.)) { boxed } }",
            ),
        ]
        .into_iter()
        .map(|(path, source)| {
            (
                PathBuf::from(path),
                parse(source).expect("parse fixture module"),
            )
        })
        .collect::<Vec<_>>();
        let (modules, _) =
            FullPipeline::lower_package(parsed, None).expect("lower qualified-provider fixture");
        let package =
            Package::build(Path::new(""), modules, None).expect("build qualified-provider package");
        package
            .resolve_imports()
            .expect("resolve qualified-provider uses");
        let module = &package.module("consumer").expect("consumer module").module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("qualified-provider environment");
        let source = &env.fn_defs["witness"].body;
        let expected = env.fn_defs["witness"].ret.clone();
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            source.span(),
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open qualified-provider root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve qualified-provider premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open qualified-provider premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let continuation =
            super::super::OrdinaryValueCursor::begin_child(source, None, &mut frontier, &mut tcx)
                .expect("plan expectation-free qualified-provider lambda");
        let staged = stage_existing_operand(continuation, &mut frontier, &mut tcx)
            .expect("stage qualified-provider lambda header");
        let mut staged = prepare_staged_header(
            staged,
            super::super::frontier::FrontierMode::LexicalFallback,
            &mut frontier,
            &mut tcx,
        )
        .expect("select synthesized qualified-provider lambda header");
        let StagedOperand::Lambda(lambda, _) = &staged else {
            panic!("the qualified-provider source did not stage as a lambda")
        };
        assert!(matches!(
            lambda.prepared.selection,
            super::super::OrdinaryLambdaSelection::Synthesizing {
                annotated_return: None
            }
        ));

        let mut next_template = 0;
        let mut next_producer = 0;
        let mut locations = Vec::new();
        reserve_projected_source(
            &mut staged,
            0,
            &mut next_template,
            &mut next_producer,
            &mut locations,
        );
        assert_eq!((next_template, next_producer), (1, 1));

        let owner = frontier.owner();
        let fallback = frontier
            .store()
            .scoped_type(
                owner,
                crate::pass::typecheck_core::InternedType::fresh(expected),
                source.span(),
            )
            .expect("scope the declared qualified-provider function type");
        let mut exports = Vec::new();
        let reserved = reserve_projected_header(
            &mut staged,
            fallback.clone(),
            &mut exports,
            &mut frontier,
            &mut tcx,
        )
        .expect("reserve synthesized qualified-provider header");
        assert!(
            reserved.ty().identity_is_canonical(),
            "the synthesized lambda wrapper must preserve its component-wise canonical identity"
        );
        let crate::ast::Type::Function { param, .. } = reserved.ty().as_type() else {
            panic!("the reserved qualified-provider header is not a function")
        };
        assert!(matches!(
            param.as_ref(),
            crate::ast::Type::Path { segments, .. }
                if segments.iter().map(crate::ast::PathSegment::as_str)
                    .eq(["dep", "core", "Box"])
        ));

        let mut exports = exports.into_iter();
        install_projected_header_exports(&mut staged, &mut exports, &mut frontier, &mut tcx)
            .expect("install synthesized qualified-provider header exports");
        assert!(exports.next().is_none());
        {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store
                .constrain(delta, fallback, reserved.clone(), source.span(), &tcx)
                .expect("link the provider header without capturing the decoy module");
        }
        let progressed = {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store
                .zonk_for_progress(delta, reserved, &tcx)
                .expect("zonk the linked qualified-provider header")
        };
        assert!(progressed.ty().identity_is_canonical());
        let assert_provider_function = |ty: &crate::ast::Type<crate::ast::Lowered>| {
            let crate::ast::Type::Function { param, ret, .. } = ty else {
                panic!("the qualified-provider header is not a function")
            };
            for slot in [param.as_ref(), ret.as_ref()] {
                assert!(matches!(
                    slot,
                    crate::ast::Type::Path { segments, .. }
                        if segments.iter().map(crate::ast::PathSegment::as_str)
                            .eq(["dep", "core", "Box"])
                ));
            }
        };
        assert_provider_function(progressed.ty().as_type());

        let seal = InvocationSeal::fresh();
        let mut specialization =
            crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(&tcx);
        let recipe = project_operand_recipe_from_header(
            &staged,
            progressed,
            &seal,
            &mut specialization,
            &tcx,
        )
        .expect("seal the qualified-provider recipe");
        assert_provider_function(recipe.ty.specialization.ty().as_type());
    }

    #[test]
    fn projected_source_eligibility_skips_a_collapsed_producerless_pair_subtree() {
        let package = fixture(
            "module projected_source_eligibility_pair; \
             host fn make[A]() -> A; \
             fn witness() -> . { (((), ()), .(_unit: .) { make() }) }",
            "projected_source_eligibility_pair",
        );
        let module = &package
            .module("projected_source_eligibility_pair")
            .expect("projected source-eligibility pair module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected source-eligibility pair environment");
        let source = &env.fn_defs["witness"].body;
        let span = source.span();
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            span,
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open projected source-eligibility pair root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                span,
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve projected source-eligibility pair premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open projected source-eligibility pair premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let result = frontier
            .reserve_projected_header_hole(span)
            .expect("reserve retained lambda result hole");
        let unit = result.projected_closed_peer(
            crate::pass::typecheck_core::InternedType::fresh_canonical(crate::ast::Type::Unit {
                meta: Meta::new(span),
            }),
        );
        let left = unit
            .projected_binary_peer(
                &unit,
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
                span,
            )
            .expect("compose the producerless pair header");
        let lambda = unit
            .projected_binary_peer(
                &result,
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Function { abi_arity: 0 },
                span,
            )
            .expect("compose the retained lambda header");
        let public = left
            .projected_binary_peer(
                &lambda,
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
                span,
            )
            .expect("compose the mixed projected source header");
        let owner = frontier.owner();
        super::super::reset_direct_lambda_work();
        let mut staged = stage_operand(
            source,
            super::super::OwnerBoundType::Retained {
                owner,
                ty: public.clone(),
            },
            &mut frontier,
            &mut tcx,
        )
        .expect("stage the mixed projected source");
        let mut next_template = 0;
        let mut next_producer = 0;
        let mut locations = Vec::new();
        reserve_projected_source(
            &mut staged,
            0,
            &mut next_template,
            &mut next_producer,
            &mut locations,
        );
        assert_eq!(next_producer, 1, "only the retained lambda is a producer");
        staged = prepare_projected_source_header(
            staged,
            super::super::frontier::FrontierMode::LexicalFallback,
            &mut frontier,
            &mut tcx,
        )
        .expect("prepare the mixed projected source headers");

        let seal = InvocationSeal::fresh();
        let mut specialization =
            crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(&tcx);
        let mut recipe = seal_projected_operand_for_test(
            &mut staged,
            public,
            &seal,
            &mut specialization,
            &mut frontier,
            &mut tcx,
        )
        .expect("seal the mixed projected source recipe");
        snapshot_projected_recipe(&mut recipe, &mut specialization, &mut frontier, &tcx)
            .expect("snapshot the mixed projected source recipe");
        let StagedOperand::Pair(pair) = &mut staged else {
            panic!("the mixed source lost its top-level pair")
        };
        collapse_initial_completed_pair_children(pair, &mut frontier, &mut tcx)
            .expect("collapse the producerless nested pair");
        assert!(matches!(
            pair.operands[0].state.as_ref(),
            Some(StagedOperand::Completed { leaf: None, .. })
        ));
        let ProjectedRecipeNode::Pair(left_recipe, right_recipe) = recipe.node.as_ref() else {
            panic!("the sealed mixed source lost its pair recipe")
        };
        assert!(left_recipe.ty.producers.is_empty());
        assert!(!right_recipe.ty.producers.is_empty());
        assert!(right_recipe.ty.closed.is_none());
        let Some(StagedOperand::Lambda(right_lambda, _)) = pair.operands[1].state.as_ref() else {
            panic!("the producer-bearing sibling lost its retained lambda state")
        };
        let crate::ast::Expr::FnExpr { sig, .. } = right_lambda.source else {
            panic!("the retained lambda sibling changed source family")
        };
        assert!(
            matches!(
                right_lambda.prepared.selection,
                super::super::OrdinaryLambdaSelection::Check { .. }
                    | super::super::OrdinaryLambdaSelection::Synthesizing {
                        annotated_return: None
                    }
            ) && lambda_header_leaves_only_open_result(
                sig,
                right_recipe.ty.specialization.ty().as_type(),
            ),
            "the producer-bearing sibling must retain a complete header and only an open body \
             result: type={:?}, sig={:?}",
            right_recipe.ty.specialization.ty().as_type(),
            sig,
        );

        validate_projected_source_before_action(&staged, &recipe)
            .expect("the collapsed producerless subtree must not hide an eligible lambda sibling");
        assert_eq!(
            super::super::direct_lambda_work().body_entries,
            0,
            "source eligibility must not enter the retained lambda body"
        );
    }

    #[test]
    fn projected_header_replay_observes_only_during_reservation_before_install_and_link() {
        let package = fixture(
            "module header_replay; \
             host fn make[A]() -> A; \
             host fn known() -> . -> .; \
             fn opaque() -> . { make()(()) } \
             fn observed() -> . -> . { known() }",
            "header_replay",
        );
        let module = &package
            .module("header_replay")
            .expect("header replay module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("header replay environment");
        let span = crate::span::Span::new(0, 0);
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            span,
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open header replay root");
        let (parent_reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                span,
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve header replay parent");
        let mut parent = root
            .open_reserved_child(parent_reservation)
            .expect("open header replay parent");
        let mut frontier = root.retained_value_frontier(&mut parent);
        let shared = frontier
            .reserve_projected_header_hole(span)
            .expect("reserve shared opaque header");
        let known = crate::ast::Type::Function {
            param: Box::new(crate::ast::Type::Unit {
                meta: Meta::new(span),
            }),
            ret: Box::new(crate::ast::Type::Unit {
                meta: Meta::new(span),
            }),
            meta: Meta::new(span),
            abi_arity: 1,
            caps: (),
        };
        let owner = frontier.owner();
        let mut opaque = stage_operand(
            &env.fn_defs["opaque"].body,
            super::super::OwnerBoundType::Retained {
                owner,
                ty: shared.clone(),
            },
            &mut frontier,
            &mut tcx,
        )
        .expect("stage formerly opaque source");
        let mut observed = stage_operand(
            &env.fn_defs["observed"].body,
            super::super::OwnerBoundType::Plain(
                crate::pass::typecheck_core::InternedType::fresh_canonical(known),
            ),
            &mut frontier,
            &mut tcx,
        )
        .expect("stage observed source");
        PROJECTED_HEADER_OBSERVER_CALLS.with(|calls| calls.set(0));
        let mut opaque_exports = Vec::new();
        let opaque_header = reserve_projected_header(
            &mut opaque,
            shared.clone(),
            &mut opaque_exports,
            &mut frontier,
            &mut tcx,
        )
        .expect("reserve formerly opaque source");
        assert!(
            opaque_exports.is_empty(),
            "the first source must reserve no owner edge"
        );
        let mut observed_exports = Vec::new();
        let observed_header = reserve_projected_header(
            &mut observed,
            shared.clone(),
            &mut observed_exports,
            &mut frontier,
            &mut tcx,
        )
        .expect("reserve observed source");
        assert_eq!(
            observed_exports.len(),
            1,
            "the observed source reserves one owner edge"
        );
        let reserve_observations = PROJECTED_HEADER_OBSERVER_CALLS.with(std::cell::Cell::get);
        assert_eq!(
            reserve_observations, 1,
            "the observable authored source must be inspected during the reservation sweep"
        );
        let mut opaque_exports = opaque_exports.into_iter();
        install_projected_header_exports(&mut opaque, &mut opaque_exports, &mut frontier, &mut tcx)
            .expect("replay formerly opaque source through the production entry");
        assert!(opaque_exports.next().is_none());

        let mut observed_exports = observed_exports.into_iter();
        install_projected_header_exports(
            &mut observed,
            &mut observed_exports,
            &mut frontier,
            &mut tcx,
        )
        .expect("replay observed source through the production entry");
        assert!(
            observed_exports.next().is_none(),
            "header replay must exhaust exactly"
        );
        assert_eq!(
            PROJECTED_HEADER_OBSERVER_CALLS.with(std::cell::Cell::get),
            reserve_observations,
            "the topology-only install sweep must not observe either source again"
        );
        {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store
                .constrain(delta, shared.clone(), opaque_header.clone(), span, &tcx)
                .expect("link formerly opaque public header");
            store
                .constrain(delta, shared.clone(), observed_header.clone(), span, &tcx)
                .expect("later source links the shared parent header");
        }
    }

    #[test]
    fn projected_written_type_tail_batches_a_sixty_four_binder_prefix_once() {
        const BINDERS: usize = 64;

        let package = fixture("module header_batch;", "header_batch");
        let module = &package
            .module("header_batch")
            .expect("header batch module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("header batch environment");
        let span = crate::span::Span::new(0, 0);
        let mut elaborations = Elaborations::new();
        let tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            span,
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open header batch root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                span,
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve header batch child");
        let mut child = root
            .open_reserved_child(reservation)
            .expect("open header batch child");
        let mut frontier = root.retained_value_frontier(&mut child);

        let mut scheme = crate::ast::Type::Unit {
            meta: Meta::new(span),
        };
        for index in (0..BINDERS).rev() {
            scheme = crate::ast::Type::Forall {
                param: crate::ast::TypeParam {
                    name: format!("T{index}"),
                    span,
                    kind: None,
                },
                body: Box::new(scheme),
                meta: Meta::new(span),
            };
        }
        let scheme = frontier
            .store()
            .scoped_type(
                frontier.owner(),
                crate::pass::typecheck_core::InternedType::fresh_canonical(scheme),
                span,
            )
            .expect("scope the retained public forall prefix");
        let arguments = (0..BINDERS)
            .map(|_| crate::ast::Type::Unit {
                meta: Meta::new(span),
            })
            .collect::<Vec<crate::ast::Type<crate::ast::Lowered>>>();
        let arguments = arguments
            .iter()
            .map(super::super::CallArgRef::Type)
            .collect::<Vec<_>>();

        PROJECTED_PUBLIC_FORALL_BATCH_CALLS.with(|calls| calls.set(0));
        let result = projected_written_type_tail(scheme, &arguments, span, &mut frontier, &tcx)
            .expect("observe the written forall prefix")
            .expect("a complete explicit prefix has a public result");
        assert!(matches!(
            result.ty().as_type(),
            crate::ast::Type::Unit { .. }
        ));
        assert_eq!(
            PROJECTED_PUBLIC_FORALL_BATCH_CALLS.with(std::cell::Cell::get),
            1,
            "the production observer must submit the whole explicit prefix in one batch"
        );
    }

    #[test]
    fn projected_producer_physical_close_drains_only_the_authored_ready_prefix() {
        check_projected_producer_ready_prefix(true, false);
    }

    #[test]
    fn projected_untyped_producer_keeps_wrapper_equations_private_until_acceptance() {
        check_projected_producer_ready_prefix(false, false);
    }

    #[test]
    fn projected_header_can_retain_an_ancestor_equation_with_host_local_goals() {
        check_projected_producer_ready_prefix(false, true);
    }

    fn check_projected_producer_ready_prefix(typed: bool, outer_public: bool) {
        let annotation = if typed { " -> ." } else { "" };
        let package = fixture(
            &format!(
                "module producer_close_prefix; \
             fn witness() -> ((. -> .) & .) & (. -> .) {{ \
                 ((.(left: .){annotation} {{ left }}, ()), .(right: .){annotation} {{ right }}) \
             }}"
            ),
            "producer_close_prefix",
        );
        let module = &package
            .module("producer_close_prefix")
            .expect("producer close-prefix module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("producer close-prefix environment");
        let source = &env.fn_defs["witness"].body;
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, outside) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            source.span(),
            &tcx,
            |store, owner| {
                if !outer_public {
                    return Ok(None);
                }
                let goal = store.alloc_goal(
                    owner,
                    crate::ast::Kind::Star,
                    crate::pass::typecheck_core::GoalSolutionPolicy::PolytypeAllowed,
                    crate::pass::typecheck_core::GoalOrigin::named(
                        source.span(),
                        crate::pass::typecheck_core::GoalRole::TypeArgument,
                        "outer public type",
                    ),
                )?;
                Ok(Some((
                    owner,
                    store.scoped_goal_at(goal, Vec::new(), owner, source.span())?,
                )))
            },
        )
        .expect("open producer close-prefix root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve producer close-prefix premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open producer close-prefix premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let result = frontier
            .reserve_projected_header_hole(source.span())
            .expect("reserve shared producer close-prefix result");
        let owner = frontier.owner();
        let unit = result.projected_closed_peer(
            crate::pass::typecheck_core::InternedType::fresh_canonical(crate::ast::Type::Unit {
                meta: Meta::new(source.span()),
            }),
        );
        let function = unit
            .projected_binary_peer(
                &result,
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Function { abi_arity: 0 },
                source.span(),
            )
            .expect("compose open producer close-prefix function");
        let left = function
            .projected_binary_peer(
                &unit,
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
                source.span(),
            )
            .expect("compose producer close-prefix left pair");
        let public = left
            .projected_binary_peer(
                &function,
                crate::pass::typecheck_core::ScopedTypeBinaryKind::Product,
                source.span(),
            )
            .expect("compose producer close-prefix root pair");
        let public = if let Some((parent, outside)) = outside {
            frontier
                .store()
                .use_retained_type_at(parent, owner, outside, source.span())
                .expect("use the outer public goal at the marked call host")
        } else {
            public
        };
        let staged = stage_operand(
            source,
            super::super::OwnerBoundType::Retained {
                owner,
                ty: public.clone(),
            },
            &mut frontier,
            &mut tcx,
        )
        .expect("stage producer close-prefix pair");
        let mut next_template = 0;
        let mut next_producer = 0;
        let mut producer_locations = Vec::new();
        let mut staged = staged;
        reserve_projected_source(
            &mut staged,
            0,
            &mut next_template,
            &mut next_producer,
            &mut producer_locations,
        );
        assert_eq!(next_producer, 2);
        assert_eq!(producer_locations[0].path.as_ref(), &[0, 0]);
        assert_eq!(producer_locations[1].path.as_ref(), &[1]);
        let seal = InvocationSeal::fresh();
        let mut specialization =
            crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(&tcx);
        let recipe = seal_projected_operand_for_test(
            &mut staged,
            public.clone(),
            &seal,
            &mut specialization,
            &mut frontier,
            &mut tcx,
        )
        .expect("seal producer close-prefix recipe");
        if outer_public {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            assert!(
                !store.delta_bindings_transport_ready_for_test(delta),
                "a public ancestor goal may retain the projected structure's host-local holes"
            );
            return;
        }
        let mut projection = MarkedProjection {
            seal,
            specialization: Some(specialization),
            header_cursor: ProjectedHeaderCursor::default(),
            terminal_result: None,
            transcript: None,
            sources: vec![RetainedProjectedSource {
                state: Some(staged),
                header: RetainedProjectedHeader::Sealed(recipe),
                inherited_mode: super::super::frontier::FrontierMode::ExpectedOnly,
                layer: 0,
                expanded_start: 0,
                expanded_width: 1,
            }],
            next_template,
            next_producer,
            next_close_producer: 0,
            producer_locations,
            producer_completed: vec![false; next_producer as usize],
            template_slots: (0..next_template)
                .map(|_| ProjectedTemplateSlot::default())
                .collect(),
        };
        let mode = super::super::frontier::FrontierMode::Final;

        reset_fill_relation_work();
        assert!(
            !projection
                .accept_relation_producer_wave(&[0, 1], source.span(), &mut frontier, &tcx)
                .expect("an incomplete relation cohort has no wave to accept")
        );
        assert_eq!(
            fill_relation_work(),
            FillRelationWork {
                batch_attempts: 0,
                batch_commits: 0,
                equations_submitted: 0,
                accepted_producers: 0,
                soft_restarts: 0,
                physical_closes: 0,
                producer_request_visits: 0,
                producer_advance_visits: 0,
                relation_wave_scan_visits: 2,
            }
        );
        let initial_earlier_expected = projection
            .producer_expected_snapshot(0, source.span(), &mut frontier, &tcx)
            .expect("inspect the earlier recipe");
        assert_eq!(initial_earlier_expected.is_some(), typed);
        let wrapper_deltas_before = projection
            .pair_wrapper_deltas()
            .iter()
            .map(|delta| format!("{delta:?}"))
            .collect::<Vec<_>>();

        let parent = projection.producer_parent(1);
        let later = projection
            .resume_producer(
                FillProducerRequest {
                    producer: 1,
                    parent,
                    producer_mode: mode,
                    inherited_mode: mode,
                    relation_probe: true,
                    expected: None,
                },
                &mut frontier,
                &mut tcx,
            )
            .expect("complete the later producer first");
        assert!(later.newly_complete);
        assert_eq!(
            projection
                .pair_wrapper_deltas()
                .iter()
                .map(|delta| format!("{delta:?}"))
                .collect::<Vec<_>>(),
            wrapper_deltas_before,
            "a completed producer must retain its operand equation until wave acceptance"
        );
        let before_accept = projection
            .producer_expected_snapshot(0, source.span(), &mut frontier, &tcx)
            .expect("inspect the earlier recipe before accepting the later equation");
        assert_eq!(before_accept.is_some(), typed);
        if let (Some(before_accept), Some(initial)) = (&before_accept, &initial_earlier_expected) {
            crate::pass::typecheck_core::require_type_equiv_state(
                before_accept.ty().as_type(),
                initial.ty().as_type(),
                source.span(),
                &tcx.env.alias_ctx(),
                before_accept.ty().identity_is_canonical(),
                initial.ty().identity_is_canonical(),
            )
            .expect("a completed child must not update its retained parent before acceptance");
        }
        let later_preview = {
            let StagedOperand::OpenCompleted {
                preview, accepted, ..
            } = projection.producer_state_mut(1)
            else {
                panic!("the completed later producer lost its open completion")
            };
            assert!(!*accepted);
            std::mem::replace(preview, unit.clone())
        };
        let error = projection
            .accept_relation_producer_wave(&[0, 1], source.span(), &mut frontier, &tcx)
            .expect_err("an incompatible relation wave must roll back");
        assert!(error.diag().1.contains("type mismatch"));
        assert_eq!(
            fill_relation_work(),
            FillRelationWork {
                batch_attempts: 1,
                batch_commits: 0,
                equations_submitted: 1,
                accepted_producers: 0,
                soft_restarts: 0,
                physical_closes: 0,
                producer_request_visits: 0,
                producer_advance_visits: 1,
                relation_wave_scan_visits: 4,
            }
        );
        let StagedOperand::OpenCompleted {
            preview, accepted, ..
        } = projection.producer_state_mut(1)
        else {
            panic!("the rejected later producer lost its open completion")
        };
        assert!(!*accepted, "a rejected relation wave became close-ready");
        *preview = later_preview;
        assert!(projection.producer_completed.iter().all(|closed| !closed));
        let after_rollback = projection
            .producer_expected_snapshot(0, source.span(), &mut frontier, &tcx)
            .expect("reinspect the earlier recipe after relation rollback");
        assert_eq!(after_rollback.is_some(), typed);
        if let (Some(after_rollback), Some(initial)) = (&after_rollback, &initial_earlier_expected)
        {
            crate::pass::typecheck_core::require_type_equiv_state(
                after_rollback.ty().as_type(),
                initial.ty().as_type(),
                source.span(),
                &tcx.env.alias_ctx(),
                after_rollback.ty().identity_is_canonical(),
                initial.ty().identity_is_canonical(),
            )
            .expect("a rejected relation wave changed a sibling recipe");
        }
        assert!(
            projection
                .accept_relation_producer_wave(&[0, 1], source.span(), &mut frontier, &tcx)
                .expect("accept the first completed relation wave")
        );
        assert_eq!(
            fill_relation_work(),
            FillRelationWork {
                batch_attempts: 2,
                batch_commits: 1,
                equations_submitted: 2,
                accepted_producers: 1,
                soft_restarts: 0,
                physical_closes: 0,
                producer_request_visits: 0,
                producer_advance_visits: 1,
                relation_wave_scan_visits: 6,
            }
        );
        let earlier_expected = projection
            .producer_expected_snapshot(0, source.span(), &mut frontier, &tcx)
            .expect("reinspect the earlier recipe after later action equality")
            .expect("the accepted relation wave lost the earlier concrete recipe");
        if let Some(initial) = &initial_earlier_expected {
            crate::pass::typecheck_core::require_type_equiv_state(
                earlier_expected.ty().as_type(),
                initial.ty().as_type(),
                source.span(),
                &tcx.env.alias_ctx(),
                earlier_expected.ty().identity_is_canonical(),
                initial.ty().identity_is_canonical(),
            )
            .expect("the accepted relation wave changed the earlier authored header");
        }
        assert!(matches!(
            projection.producer_state(1),
            StagedOperand::OpenCompleted { accepted: true, .. }
        ));
        assert!(
            projection
                .next_ready_producer_close(source.span(), &mut frontier, &tcx)
                .expect("inspect held later producer")
                .is_none()
        );
        assert_eq!(projection.next_close_producer, 0);

        let parent = projection.producer_parent(0);
        let earlier = projection
            .resume_producer(
                FillProducerRequest {
                    producer: 0,
                    parent,
                    producer_mode: mode,
                    inherited_mode: mode,
                    relation_probe: true,
                    expected: Some(earlier_expected),
                },
                &mut frontier,
                &mut tcx,
            )
            .expect("complete the earlier producer");
        assert!(earlier.newly_complete);
        assert!(matches!(
            projection.producer_state(0),
            StagedOperand::OpenCompleted {
                accepted: false,
                ..
            }
        ));
        assert!(
            projection
                .next_ready_producer_close(source.span(), &mut frontier, &tcx)
                .expect("inspect the unaccepted authored-prefix producer")
                .is_none(),
            "physical close must wait for wave acceptance"
        );
        assert!(projection.producer_completed.iter().all(|closed| !closed));
        assert!(
            projection
                .accept_relation_producer_wave(&[0, 1], source.span(), &mut frontier, &tcx)
                .expect("accept the second completed relation wave")
        );
        assert_eq!(
            fill_relation_work(),
            FillRelationWork {
                batch_attempts: 3,
                batch_commits: 2,
                equations_submitted: 3,
                accepted_producers: 2,
                soft_restarts: 0,
                physical_closes: 0,
                producer_request_visits: 0,
                producer_advance_visits: 2,
                relation_wave_scan_visits: 8,
            }
        );
        assert!(matches!(
            projection.producer_state(0),
            StagedOperand::OpenCompleted { accepted: true, .. }
        ));

        reset_pair_wrapper_finalizations();
        let earlier_close = projection
            .next_ready_producer_close(source.span(), &mut frontier, &tcx)
            .expect("freeze earlier close")
            .expect("earlier producer is now the ready prefix");
        assert_eq!(earlier_close.producer, 0);
        projection
            .close_ready_producer(earlier_close, source.span(), &mut frontier, &mut tcx)
            .expect("close earlier producer first");
        assert!(projection.producer_is_complete(0));
        assert!(!projection.producer_is_complete(1));

        let later_close = projection
            .next_ready_producer_close(source.span(), &mut frontier, &tcx)
            .expect("freeze later close")
            .expect("held later producer follows the closed prefix");
        assert_eq!(later_close.producer, 1);
        projection
            .close_ready_producer(later_close, source.span(), &mut frontier, &mut tcx)
            .expect("close held later producer second");
        assert!(projection.producer_completed.iter().all(|closed| *closed));
        assert_eq!(projection.next_close_producer, 2);
        assert_eq!(
            fill_relation_work(),
            FillRelationWork {
                batch_attempts: 3,
                batch_commits: 2,
                equations_submitted: 3,
                accepted_producers: 2,
                soft_restarts: 0,
                physical_closes: 2,
                producer_request_visits: 0,
                producer_advance_visits: 2,
                relation_wave_scan_visits: 8,
            }
        );
        assert!(matches!(
            projection.sources[0].state,
            Some(StagedOperand::Completed { leaf: None, .. })
        ));
        assert_eq!(
            pair_wrapper_finalizations(),
            2,
            "nested and top pair wrappers must each close once after their authored leaf prefix"
        );

        let Some(StagedOperand::Completed { completed, .. }) = projection.sources[0].state.as_ref()
        else {
            panic!("the closed prefix lost its pair completion")
        };
        let output = completed.scoped().clone();
        projection.terminal_result = Some(output.clone());
        let checked = ProjectedCheckedResult::Recipe(projection.sources[0].header.recipe().clone());
        let mut adoption = FillAdoption {
            relations: Vec::new(),
            relation_producers: vec![0, 1],
            independent_producers: Vec::new(),
            producer_mode: super::super::frontier::FrontierMode::ExpectedOnly,
            producer_index: 0,
            sweep_complete: true,
            barrier_applied: false,
            phase: FillAdoptionPhase::Relations,
        };
        assert!(matches!(
            adoption
                .advance(
                    &mut projection,
                    &checked,
                    &output,
                    mode,
                    source.span(),
                    &mut frontier,
                    &mut tcx,
                )
                .expect("skip physically closed producer slots"),
            FillAdoptionAdvance::Complete
        ));
    }

    #[test]
    fn independent_producer_headers_follow_the_fill_validation_schedule() {
        let package = fixture(
            "module independent_result_header; \
             fn witness() -> . -> . { .(_value: .) { () } }",
            "independent_result_header",
        );
        let module = &package
            .module("independent_result_header")
            .expect("independent result-header module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("independent result-header environment");
        let source = &env.fn_defs["witness"].body;
        let span = source.span();
        for result_closed_by_relation in [false, true] {
            let mut elaborations = Elaborations::new();
            let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
            let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
                span,
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("open independent result-header root");
            let (reservation, ()) = root
                .reserve_root_child_with(
                    crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                    span,
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve independent result-header premise");
            let mut call = root
                .open_reserved_child(reservation)
                .expect("open independent result-header premise");
            let mut frontier = root.retained_value_frontier(&mut call);
            let result = frontier
                .reserve_projected_header_hole(span)
                .expect("reserve the projected call's initially unknown result");
            let unit = result.projected_closed_peer(
                crate::pass::typecheck_core::InternedType::fresh_canonical(
                    crate::ast::Type::Unit {
                        meta: Meta::new(span),
                    },
                ),
            );
            let source_expected = unit
                .projected_binary_peer(
                    &result,
                    crate::pass::typecheck_core::ScopedTypeBinaryKind::Function { abi_arity: 1 },
                    span,
                )
                .expect("compose the source's open function header");
            let owner = frontier.owner();
            let mut staged = stage_operand(
                source,
                super::super::OwnerBoundType::Retained {
                    owner,
                    ty: source_expected.clone(),
                },
                &mut frontier,
                &mut tcx,
            )
            .expect("stage the initially unknown lambda source");
            let mut next_template = 0;
            let mut next_producer = 0;
            let mut producer_locations = Vec::new();
            reserve_projected_source(
                &mut staged,
                0,
                &mut next_template,
                &mut next_producer,
                &mut producer_locations,
            );
            assert_eq!((next_template, next_producer), (1, 1));
            staged = prepare_projected_source_header(
                staged,
                super::super::frontier::FrontierMode::LexicalFallback,
                &mut frontier,
                &mut tcx,
            )
            .expect("prepare the lambda's structural header without entering its body");
            assert!(matches!(staged, StagedOperand::Lambda(..)));

            let seal = InvocationSeal::fresh();
            let mut specialization =
                crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(&tcx);
            let recipe = seal_projected_operand_for_test(
                &mut staged,
                source_expected,
                &seal,
                &mut specialization,
                &mut frontier,
                &mut tcx,
            )
            .expect("seal the independent producer recipe");
            let proof = MarkedComptimeProof { seal: seal.clone() };
            let function_type = crate::normalization::projected_value(ProjectedValue(
                ProjectedValueKind::Type(recipe.ty.clone()),
            ));
            let function = crate::normalization::projected_value(ProjectedValue(
                ProjectedValueKind::CheckedRecipe(recipe.clone()),
            ));
            let argument = Value::CheckedTerm(crate::normalization::EvalRef::new(
                crate::normalization::CheckedTerm::new(
                    crate::ast::Expr::Unit {
                        occurrence: Default::default(),
                        meta: crate::ast::Meta::new(span),
                    },
                    crate::ast::Type::Unit {
                        meta: crate::ast::Meta::new(span),
                    },
                ),
            ));
            let checked = projected_term_call(&proof, &function_type, &function, &argument)
                .expect("compose the evaluator's projected call recipe")
                .authenticated_checked_recipe(&proof)
                .expect("the projected call recipe must keep its invocation proof");
            let checked = ProjectedCheckedResult::Recipe(checked);
            let mut projection = MarkedProjection {
                seal,
                specialization: Some(specialization),
                header_cursor: ProjectedHeaderCursor {
                    phase: ProjectedHeaderPhase::Complete,
                    source: 0,
                },
                terminal_result: Some(result.clone()),
                transcript: None,
                sources: vec![RetainedProjectedSource {
                    state: Some(staged),
                    header: RetainedProjectedHeader::Sealed(recipe),
                    inherited_mode: super::super::frontier::FrontierMode::LexicalFallback,
                    layer: 0,
                    expanded_start: 0,
                    expanded_width: 1,
                }],
                next_template,
                next_producer,
                next_close_producer: 0,
                producer_locations,
                producer_completed: vec![false],
                template_slots: vec![ProjectedTemplateSlot::default()],
            };
            let result_is_open = {
                let (store, _owner, delta, _publication) = frontier.parts_mut();
                store
                    .try_zonk_goal_free(delta, result.clone(), &tcx)
                    .expect("inspect the result before its producer runs")
                    .is_none()
            };
            assert!(result_is_open, "the fixture result must begin unresolved");
            let (projection_proof, empty_context) = projection.capabilities();
            let context = if result_closed_by_relation {
                let destination = crate::normalization::projected_value(ProjectedValue(
                    ProjectedValueKind::Type(ProjectedType {
                        seal: projection.seal.clone(),
                        specialization: result.clone(),
                        ty: result.clone(),
                        closed: None,
                        focus: Some(ProjectedFocus {
                            result_root: ProjectedResultRoot(0),
                            path: Arc::from([]),
                        }),
                        contains_focus: true,
                        producers: ProducerSet::default(),
                    }),
                ));
                let candidate = Value::ReflType(
                    crate::normalization::ReflectedType::new(
                        crate::ast::Type::Unit {
                            meta: crate::ast::Meta::new(span),
                        },
                        "the returned fill candidate is Unit",
                    )
                    .expect("reflect the returned Unit fill candidate"),
                );
                empty_context
                    .append(&projection_proof, destination, candidate)
                    .expect("append the authenticated result fill")
            } else {
                empty_context
            };
            let mut adoption = projection
                .prepare_adoption(&context, span, &mut frontier, &tcx)
                .expect("prepare the returned fill context");
            assert!(adoption.relation_producers.is_empty());
            assert_eq!(adoption.independent_producers, [0]);
            let request = match adoption
                .advance(
                    &mut projection,
                    &checked,
                    &result,
                    super::super::frontier::FrontierMode::Final,
                    span,
                    &mut frontier,
                    &mut tcx,
                )
                .expect("cross into the independent producer sweep")
            {
                FillAdoptionAdvance::Producer(request) => request,
                FillAdoptionAdvance::Complete
                | FillAdoptionAdvance::Pending
                | FillAdoptionAdvance::BarrierApplied => {
                    panic!("the independent producer was not scheduled")
                }
            };
            assert!(!request.relation_probe);
            if result_closed_by_relation {
                let expected = request
                    .expected
                    .expect("the returned fill must supply the independent source header");
                let closed_header = unit
                    .projected_binary_peer(
                        &unit,
                        crate::pass::typecheck_core::ScopedTypeBinaryKind::Function {
                            abi_arity: 1,
                        },
                        span,
                    )
                    .expect("compose the filled source header");
                crate::pass::typecheck_core::require_type_equiv_state(
                    expected.ty().as_type(),
                    closed_header.ty().as_type(),
                    span,
                    &tcx.env.alias_ctx(),
                    expected.ty().identity_is_canonical(),
                    closed_header.ty().identity_is_canonical(),
                )
                .expect("the independent request must receive the filled source header");
                continue;
            }
            assert!(
                request.expected.is_none(),
                "the open source header must not masquerade as a closed expectation"
            );
            let resumed = projection
                .resume_producer(request, &mut frontier, &mut tcx)
                .expect("infer the independent source body");
            assert!(resumed.complete && resumed.newly_complete);
            projection
                .accept_resumed_producer(&resumed, span, &mut frontier, &tcx)
                .expect("accept the source body before validating the returned recipe");
            adoption.record_producer(&resumed);
            let close = projection
                .next_ready_producer_close(span, &mut frontier, &tcx)
                .expect("inspect the accepted producer")
                .expect("the accepted producer must become close-ready");
            projection
                .close_ready_producer(close, span, &mut frontier, &mut tcx)
                .expect("physically close the independent producer");
            assert!(matches!(
                adoption
                    .advance(
                        &mut projection,
                        &checked,
                        &result,
                        super::super::frontier::FrontierMode::Final,
                        span,
                        &mut frontier,
                        &mut tcx,
                    )
                    .expect("validate the returned recipe after producer completion"),
                FillAdoptionAdvance::Complete
            ));
            let result = {
                let (store, _owner, delta, _publication) = frontier.parts_mut();
                store
                    .require_goal_free_after_delta(delta, result, &tcx)
                    .expect("the source body must close the projected result")
            };
            assert!(matches!(
                result.ty().as_type(),
                crate::ast::Type::Unit { .. }
            ));
        }
    }

    #[test]
    fn projected_contextual_lambda_checks_the_complete_interleaved_header_spine() {
        let package = fixture(
            "module contextual_lambda_spine; \
             fn witness() -> [T] (T & .) -> [U] U -> U { \
                 .[T](first: T, token: .)[U](second: U) -> U { second } \
             }",
            "contextual_lambda_spine",
        );
        let module = &package
            .module("contextual_lambda_spine")
            .expect("contextual lambda module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("contextual lambda environment");
        let source = &env.fn_defs["witness"].body;
        let expected_ty = env.fn_defs["witness"].ret.clone();
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            source.span(),
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open contextual lambda root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve contextual lambda premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open contextual lambda premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let continuation =
            super::super::OrdinaryValueCursor::begin_child(source, None, &mut frontier, &mut tcx)
                .expect("plan contextual lambda without an expectation");
        let staged = stage_existing_operand(continuation, &mut frontier, &mut tcx)
            .expect("stage expectation-free lambda header");
        assert!(matches!(staged, StagedOperand::Ordinary(..)));
        let mut staged = prepare_staged_header(
            staged,
            super::super::frontier::FrontierMode::LexicalFallback,
            &mut frontier,
            &mut tcx,
        )
        .expect("select the complete synthesized lambda header");
        assert!(matches!(staged, StagedOperand::Lambda(..)));
        let parent = frontier.owner();
        let expected = frontier
            .store()
            .scoped_type(
                parent,
                crate::pass::typecheck_core::InternedType::fresh(expected_ty.clone()),
                source.span(),
            )
            .expect("scope the late contextual lambda expectation");
        install_staged_expected(
            &mut staged,
            parent,
            expected.clone(),
            &mut frontier,
            &mut tcx,
        )
        .expect("install the complete interleaved contextual header");
        let StagedOperand::Lambda(lambda, _) = &staged else {
            unreachable!("the contextual lambda changed staged family")
        };
        assert!(matches!(
            lambda.prepared.selection,
            super::super::OrdinaryLambdaSelection::Check { .. }
        ));
        assert_eq!(
            lambda.prepared.value_param_types.len(),
            3,
            "both value groups must retain their selected parameter types"
        );
        let (staged, complete) = advance_staged_producer(
            staged,
            super::super::frontier::FrontierMode::Final,
            false,
            &mut frontier,
            &mut tcx,
        )
        .expect("advance the contextual lambda body");
        assert!(complete);
        let StagedOperand::OpenCompleted { preview, .. } = staged else {
            panic!("the contextual lambda must retain an unclosed exact-parent completion")
        };
        crate::pass::typecheck_core::require_type_equiv_state(
            preview.ty().as_type(),
            expected.ty().as_type(),
            source.span(),
            &tcx.env.alias_ctx(),
            preview.ty().identity_is_canonical(),
            expected.ty().identity_is_canonical(),
        )
        .expect("the full interleaved lambda spine must match its contextual expectation");
    }

    #[test]
    fn projected_literal_prepares_a_closed_header_before_its_single_physical_close() {
        let package = fixture(
            "module projected_literal_header; \
             host type String role(str); \
             fn witness() -> String { \"ignored\" }",
            "projected_literal_header",
        );
        let module = &package
            .module("projected_literal_header")
            .expect("projected literal module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected literal environment");
        let source = &env.fn_defs["witness"].body;
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            source.span(),
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open projected literal root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve projected literal action premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open projected literal action premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let public = frontier
            .reserve_projected_header_hole(source.span())
            .expect("reserve open public literal type");
        let parent = frontier.owner();
        let mut staged = stage_operand(
            source,
            super::super::OwnerBoundType::Retained {
                owner: parent,
                ty: public.clone(),
            },
            &mut frontier,
            &mut tcx,
        )
        .expect("stage literal against its open public type");
        assert!(matches!(
            &staged,
            StagedOperand::Ordinary(continuation, None)
                if continuation.next_mode
                    == super::super::frontier::FrontierMode::LexicalFallback
                    && matches!(
                        &continuation.cursor.planned,
                        super::super::PlannedOrdinaryValue::Literal
                    )
        ));

        let mut next_template = 0;
        let mut next_producer = 0;
        let mut producer_locations = Vec::new();
        reserve_projected_source(
            &mut staged,
            0,
            &mut next_template,
            &mut next_producer,
            &mut producer_locations,
        );
        assert_eq!((next_template, next_producer), (1, 1));
        assert_eq!(producer_locations[0].source, 0);
        assert!(producer_locations[0].path.is_empty());
        assert!(matches!(
            &staged,
            StagedOperand::Ordinary(
                _,
                Some(ProjectedLeaf {
                    template_index: 0,
                    producer: Some(0),
                })
            )
        ));

        staged = prepare_staged_header(
            staged,
            super::super::frontier::FrontierMode::LexicalFallback,
            &mut frontier,
            &mut tcx,
        )
        .expect("prepare the literal's closed lexical header");
        let StagedOperand::OpenCompleted {
            child,
            preview,
            leaf: Some(leaf),
            ..
        } = &staged
        else {
            panic!("the prepared literal must retain one open physical completion")
        };
        assert_eq!(child.parent_owner(), parent);
        assert_eq!((leaf.template_index, leaf.producer), (0, Some(0)));
        let preview = {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store
                .require_goal_free_after_delta(delta, preview.clone(), &tcx)
                .expect("the literal preview must be goal-free before header reservation")
        };
        assert!(matches!(
            preview.ty().as_type(),
            crate::ast::Type::Path { segments, .. }
                if segments.last().is_some_and(|segment| segment.name == "String")
        ));

        let mut exports = Vec::new();
        let structural = reserve_projected_header(
            &mut staged,
            public.clone(),
            &mut exports,
            &mut frontier,
            &mut tcx,
        )
        .expect("reserve the prepared literal header");
        assert!(exports.is_empty());
        {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            store
                .constrain(
                    delta,
                    public.clone(),
                    structural.clone(),
                    source.span(),
                    &tcx,
                )
                .expect("link the public literal type to its closed preview");
        }
        let progressed = {
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            let progressed = store
                .zonk_for_progress(delta, public, &tcx)
                .expect("zonk the linked public literal type");
            store
                .require_goal_free_after_delta(delta, progressed, &tcx)
                .expect("the linked public literal type must be closed")
        };
        assert!(matches!(
            progressed.ty().as_type(),
            crate::ast::Type::Path { segments, .. }
                if segments.last().is_some_and(|segment| segment.name == "String")
        ));

        let seal = InvocationSeal::fresh();
        let mut specialization =
            crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(&tcx);
        let recipe = project_operand_recipe_from_header(
            &staged,
            progressed,
            &seal,
            &mut specialization,
            &tcx,
        )
        .expect("seal the closed literal recipe");
        let mut projection = MarkedProjection {
            seal,
            specialization: Some(specialization),
            header_cursor: ProjectedHeaderCursor {
                phase: ProjectedHeaderPhase::Complete,
                source: 0,
            },
            terminal_result: None,
            transcript: None,
            sources: vec![RetainedProjectedSource {
                state: Some(staged),
                header: RetainedProjectedHeader::Sealed(recipe),
                inherited_mode: super::super::frontier::FrontierMode::LexicalFallback,
                layer: 0,
                expanded_start: 0,
                expanded_width: 1,
            }],
            next_template,
            next_producer,
            next_close_producer: 0,
            producer_locations,
            producer_completed: vec![false],
            template_slots: vec![ProjectedTemplateSlot::default()],
        };
        reset_fill_relation_work();
        let resumed = projection
            .resume_producer(
                FillProducerRequest {
                    producer: 0,
                    parent,
                    producer_mode: super::super::frontier::FrontierMode::ExpectedOnly,
                    inherited_mode: super::super::frontier::FrontierMode::LexicalFallback,
                    relation_probe: false,
                    expected: None,
                },
                &mut frontier,
                &mut tcx,
            )
            .expect("resume the independently prepared literal");
        assert!(resumed.complete);
        assert!(!resumed.newly_complete);
        projection
            .accept_resumed_producer(&resumed, source.span(), &mut frontier, &tcx)
            .expect("accept the prepared literal as an independent producer");
        let revisited = projection
            .resume_producer(
                FillProducerRequest {
                    producer: 0,
                    parent,
                    producer_mode: super::super::frontier::FrontierMode::ExpectedOnly,
                    inherited_mode: super::super::frontier::FrontierMode::LexicalFallback,
                    relation_probe: false,
                    expected: None,
                },
                &mut frontier,
                &mut tcx,
            )
            .expect("revisit the accepted literal before its physical close");
        assert!(revisited.complete);
        assert!(!revisited.newly_complete);
        projection
            .accept_resumed_producer(&revisited, source.span(), &mut frontier, &tcx)
            .expect("preserve the accepted literal across a stale revisit");
        assert!(matches!(
            projection.producer_state(0),
            StagedOperand::OpenCompleted {
                accepted: true,
                accepted_by_relation_wave: false,
                ..
            }
        ));
        let close = projection
            .next_ready_producer_close(source.span(), &mut frontier, &tcx)
            .expect("inspect the prepared literal close")
            .expect("the prepared literal must be ready for its one physical close");
        assert_eq!((close.producer, close.parent), (0, parent));
        projection
            .close_ready_producer(close, source.span(), &mut frontier, &mut tcx)
            .expect("physically close the prepared literal exactly once");
        assert_eq!(fill_relation_work(), FillRelationWork::default());
        assert_eq!(projection.next_close_producer, 1);
        assert_eq!(projection.producer_completed, [true]);
        assert!(matches!(
            projection.sources[0].state,
            Some(StagedOperand::Completed { leaf: None, .. })
        ));
        let captured = projection.template_slots[0]
            .source
            .as_ref()
            .expect("the literal close must capture its dense template source");
        assert_eq!(captured.source.span(), source.span());
        assert!(matches!(
            captured.ty.as_type(),
            crate::ast::Type::Path { segments, .. }
                if segments.last().is_some_and(|segment| segment.name == "String")
        ));
    }

    #[test]
    fn projected_fresh_path_call_completes_only_an_unresolved_header() {
        let package = fixture(
            "module projected_path_unit_call_header; \
             host type String role(str); \
             fn identity[A](value: A) -> A { value } \
             host fn fixed[A](value: A) -> String; \
             host fn phantom[A, B](value: A) -> B; \
             fn completed(value: String) -> String { identity(value) } \
             fn producer_free_open(value: String) -> String { phantom(value) } \
             fn closed(value: String) -> String { fixed(value) }",
            "projected_path_unit_call_header",
        );
        let module = &package
            .module("projected_path_unit_call_header")
            .expect("projected path/Unit call module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected path/Unit call environment");
        for (name, expected_term_complete, expected_public_closed, expected_certified) in [
            ("completed", true, true, true),
            ("producer_free_open", true, false, true),
            ("closed", false, false, false),
        ] {
            let (item_index, witness) = module
                .items
                .iter()
                .enumerate()
                .find_map(|(index, item)| match item {
                    crate::ast::Item::FnDef(def) if def.name == name => Some((index, def)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("projected Path call `{name}` witness"));
            let source = &witness.body;
            let mut elaborations = Elaborations::new();
            let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations).at_item(item_index);
            for parameter in witness.sig.value_params() {
                tcx.push_value(
                    parameter.name.clone(),
                    parameter.ty.clone().expect("witness parameter annotation"),
                );
            }
            let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("open projected Path call root");
            let (reservation, ()) = root
                .reserve_root_child_with(
                    crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                    source.span(),
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve projected Path action premise");
            let mut call = root
                .open_reserved_child(reservation)
                .expect("open projected Path action premise");
            let mut frontier = root.retained_value_frontier(&mut call);
            let public = frontier
                .reserve_projected_header_hole(source.span())
                .expect("reserve open public Path call type");
            let parent = frontier.owner();
            let continuation = super::super::OrdinaryValueCursor::begin_child(
                source,
                Some(super::super::OwnerBoundType::Retained {
                    owner: parent,
                    ty: public.clone(),
                }),
                &mut frontier,
                &mut tcx,
            )
            .expect("plan the projected Path call");
            assert!(certified_path_unit_call_tree_source(source, source, &tcx));
            assert_eq!(
                continuation.next_mode,
                super::super::frontier::FrontierMode::ExpectedOnly
            );
            let super::super::PlannedOrdinaryValue::Call(
                super::super::OrdinaryCallContinuation::Selecting(selecting),
            ) = &continuation.cursor.planned
            else {
                panic!("the projected Path call must start in Selecting")
            };
            assert!(selecting.direct.is_some());
            assert!(selecting.regular_ufcs.is_none());
            let raw_result = untouched_projected_selecting_result(selecting)
                .expect("the projected Path call must start untouched");
            if expected_certified {
                assert!(crate::pass::typecheck_core::type_contains_goal(
                    raw_result.as_type()
                ));
                assert!(!crate::pass::typecheck_core::type_contains_infer(
                    raw_result.as_type()
                ));
            } else {
                assert!(!crate::pass::typecheck_core::type_contains_goal(
                    raw_result.as_type()
                ));
                assert!(!crate::pass::typecheck_core::type_contains_infer(
                    raw_result.as_type()
                ));
            }
            assert_eq!(
                certified_fresh_path_unit_call(&continuation, &tcx),
                expected_certified
            );

            let mut staged = stage_projected_source_operand(continuation, &mut frontier, &mut tcx)
                .unwrap_or_else(|error| panic!("stage projected Path call `{name}`: {error:?}"));
            if expected_term_complete {
                let StagedOperand::Completed {
                    completed,
                    leaf: None,
                    ..
                } = &staged
                else {
                    panic!("the Path call must close physically before producer reservation")
                };
                if expected_public_closed {
                    assert!(matches!(
                        completed.scoped().ty().as_type(),
                        crate::ast::Type::Path { segments, .. }
                            if segments.last().is_some_and(|segment| segment.name == "String")
                    ));
                } else {
                    assert!(crate::pass::typecheck_core::type_contains_goal(
                        completed.scoped().ty().as_type()
                    ));
                    assert!(!crate::pass::typecheck_core::type_contains_infer(
                        completed.scoped().ty().as_type()
                    ));
                }
                let progressed = {
                    let (store, _owner, delta, _publication) = frontier.parts_mut();
                    store
                        .zonk_for_progress(delta, public.clone(), &tcx)
                        .expect("zonk the completed Path call's public type")
                };
                if expected_public_closed {
                    let progressed = {
                        let (store, _owner, delta, _publication) = frontier.parts_mut();
                        store
                            .require_goal_free_after_delta(delta, progressed, &tcx)
                            .expect(
                                "the anchored Path call must close its public type at ExpectedOnly",
                            )
                    };
                    assert!(matches!(
                        progressed.ty().as_type(),
                        crate::ast::Type::Path { segments, .. }
                            if segments.last().is_some_and(|segment| segment.name == "String")
                    ));
                } else {
                    assert!(crate::pass::typecheck_core::type_contains_goal(
                        progressed.ty().as_type()
                    ));
                    assert!(!crate::pass::typecheck_core::type_contains_infer(
                        progressed.ty().as_type()
                    ));
                }
            } else {
                assert!(
                    matches!(
                        &staged,
                        StagedOperand::Ordinary(continuation, None)
                            if continuation.next_mode
                                == super::super::frontier::FrontierMode::ExpectedOnly
                    ),
                    "projected Path call `{name}` left an unexpected pre-reserve state"
                );
            }

            let mut next_template = 0;
            let mut next_producer = 0;
            let mut producer_locations = Vec::new();
            reserve_projected_source(
                &mut staged,
                0,
                &mut next_template,
                &mut next_producer,
                &mut producer_locations,
            );
            assert_eq!(next_template, 1);
            assert_eq!(next_producer, usize::from(!expected_term_complete) as u32);
            assert_eq!(producer_locations.len(), next_producer as usize);
            if expected_term_complete {
                assert!(matches!(
                    &staged,
                    StagedOperand::Completed {
                        leaf: Some(ProjectedLeaf {
                            template_index: 0,
                            producer: None,
                        }),
                        ..
                    }
                ));
            } else {
                assert!(matches!(
                    &staged,
                    StagedOperand::Ordinary(
                        _,
                        Some(ProjectedLeaf {
                            template_index: 0,
                            producer: Some(0),
                        })
                    )
                ));
            }
            if name == "producer_free_open" {
                let seal = InvocationSeal::fresh();
                let mut specialization =
                    crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(&tcx);
                let mut recipe = seal_projected_operand_for_test(
                    &mut staged,
                    public,
                    &seal,
                    &mut specialization,
                    &mut frontier,
                    &mut tcx,
                )
                .expect("seal the producer-free open Path recipe");
                snapshot_projected_recipe(&mut recipe, &mut specialization, &mut frontier, &tcx)
                    .expect("snapshot the producer-free open Path recipe");
                assert!(recipe.ty.producers.is_empty());
                assert!(recipe.ty.closed.is_none());
                let error = validate_projected_source_before_action(&staged, &recipe)
                    .expect_err("a producer-free open source must fail before the action");
                assert_eq!(error.diagnostic().span, source.span());
            }
        }
    }

    #[test]
    fn projected_regular_ufcs_flavors_share_path_and_scalar_header_staging() {
        let package = fixture(
            "module projected_regular_ufcs_header; \
             host type String role(str); \
             host fn same[A](first: A, second: A) -> A; \
             fn path_receiver_first(value: String, known: String) -> String { value.>same(known) } \
             fn path_receiver_last(value: String, known: String) -> String { value.>>same(known) } \
             fn path_argument_first(value: String, known: String) -> String { same(known).<<value } \
             fn path_argument_last(value: String, known: String) -> String { same(known).<value } \
             fn scalar_receiver_first(known: String) -> String { \"value\".>same(known) } \
             fn scalar_receiver_last(known: String) -> String { \"value\".>>same(known) } \
             fn scalar_argument_first(known: String) -> String { same(known).<<\"value\" } \
             fn scalar_argument_last(known: String) -> String { same(known).<\"value\" }",
            "projected_regular_ufcs_header",
        );
        let module = &package
            .module("projected_regular_ufcs_header")
            .expect("projected regular UFCS module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected regular UFCS environment");

        for (name, scalar, receiver_first) in [
            ("path_receiver_first", false, true),
            ("path_receiver_last", false, false),
            ("path_argument_first", false, true),
            ("path_argument_last", false, false),
            ("scalar_receiver_first", true, true),
            ("scalar_receiver_last", true, false),
            ("scalar_argument_first", true, true),
            ("scalar_argument_last", true, false),
        ] {
            let (item_index, witness) = module
                .items
                .iter()
                .enumerate()
                .find_map(|(index, item)| match item {
                    crate::ast::Item::FnDef(def) if def.name == name => Some((index, def)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("projected regular UFCS `{name}` witness"));
            let source = &witness.body;
            assert!(matches!(source, crate::ast::Expr::Ufcs { bang: None, .. }));
            let mut elaborations = Elaborations::new();
            let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations).at_item(item_index);
            for parameter in witness.sig.value_params() {
                tcx.push_value(
                    parameter.name.clone(),
                    parameter.ty.clone().expect("UFCS parameter annotation"),
                );
            }
            let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("open projected regular UFCS root");
            let (reservation, ()) = root
                .reserve_root_child_with(
                    crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                    source.span(),
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve projected regular UFCS premise");
            let mut call = root
                .open_reserved_child(reservation)
                .expect("open projected regular UFCS premise");
            let mut frontier = root.retained_value_frontier(&mut call);
            let public = frontier
                .reserve_projected_header_hole(source.span())
                .expect("reserve projected regular UFCS public type");
            let owner = frontier.owner();
            let continuation = super::super::OrdinaryValueCursor::begin_child(
                source,
                Some(super::super::OwnerBoundType::Retained {
                    owner,
                    ty: public.clone(),
                }),
                &mut frontier,
                &mut tcx,
            )
            .unwrap_or_else(|error| panic!("plan projected regular UFCS `{name}`: {error:?}"));
            let super::super::PlannedOrdinaryValue::Call(
                super::super::OrdinaryCallContinuation::Selecting(selecting),
            ) = &continuation.cursor.planned
            else {
                panic!("a regular UFCS source must start in Selecting")
            };
            assert!(selecting.direct.is_some());
            assert!(selecting.regular_ufcs.is_some());
            let normalized = certified_fresh_unresolved_direct_call_source(&continuation)
                .expect("a fresh regular UFCS call must retain its normalized prefix source");
            let crate::ast::Expr::Call { args, .. } = normalized else {
                panic!("a normalized regular UFCS must be a prefix call")
            };
            let [
                crate::ast::CallArg::Value(first),
                crate::ast::CallArg::Value(second),
            ] = args.as_slice()
            else {
                panic!("a normalized regular UFCS must retain two value candidates")
            };
            let is_value_path = |value: &crate::ast::Expr<crate::ast::Lowered>| {
                matches!(
                    value,
                    crate::ast::Expr::Path { segments, .. }
                        if segments.last().is_some_and(|segment| segment.name == "value")
                )
            };
            let is_literal = |value: &crate::ast::Expr<crate::ast::Lowered>| {
                super::super::super::literal_check_parts(value).is_some()
            };
            assert_eq!(
                if scalar {
                    is_literal(first)
                } else {
                    is_value_path(first)
                },
                receiver_first,
                "regular UFCS `{name}` inserted its receiver in the wrong value slot"
            );
            assert_eq!(
                if scalar {
                    is_literal(second)
                } else {
                    is_value_path(second)
                },
                !receiver_first,
                "regular UFCS `{name}` did not retain the peer argument order"
            );
            assert_eq!(
                certified_path_unit_call_tree_source(normalized, source, &tcx),
                !scalar,
                "unexpected normalized Path/Unit classification for `{name}`"
            );
            assert_eq!(
                certified_one_layer_scalar_literal_source(normalized, &tcx),
                scalar,
                "unexpected normalized scalar classification for `{name}`"
            );
            assert_eq!(certified_fresh_path_unit_call(&continuation, &tcx), !scalar);
            assert_eq!(
                certified_fresh_scalar_literal_call(&continuation, &tcx),
                scalar
            );

            let mut staged = stage_projected_source_operand(continuation, &mut frontier, &mut tcx)
                .unwrap_or_else(|error| panic!("stage projected regular UFCS `{name}`: {error:?}"));
            let retains_scalar_fallback = scalar && receiver_first;
            if retains_scalar_fallback {
                assert!(
                    matches!(
                        &staged,
                        StagedOperand::Ordinary(continuation, None)
                            if continuation.next_mode
                                == super::super::frontier::FrontierMode::LexicalFallback
                    ),
                    "scalar-first regular UFCS `{name}` did not retain LexicalFallback work"
                );
            } else {
                assert!(matches!(
                    &staged,
                    StagedOperand::Completed { leaf: None, .. }
                ));
            }

            let mut next_template = 0;
            let mut next_producer = 0;
            let mut producer_locations = Vec::new();
            reserve_projected_source(
                &mut staged,
                0,
                &mut next_template,
                &mut next_producer,
                &mut producer_locations,
            );
            assert_eq!(next_template, 1);
            assert_eq!(next_producer, u32::from(retains_scalar_fallback));
            assert_eq!(
                producer_locations.len(),
                usize::from(retains_scalar_fallback)
            );
            if retains_scalar_fallback {
                staged = prepare_projected_source_header(
                    staged,
                    super::super::frontier::FrontierMode::LexicalFallback,
                    &mut frontier,
                    &mut tcx,
                )
                .unwrap_or_else(|error| {
                    panic!("prepare projected regular scalar UFCS `{name}`: {error:?}")
                });
                assert!(matches!(
                    &staged,
                    StagedOperand::OpenCompleted {
                        leaf: Some(ProjectedLeaf {
                            template_index: 0,
                            producer: Some(0),
                        }),
                        ..
                    }
                ));
            } else {
                assert!(matches!(
                    &staged,
                    StagedOperand::Completed {
                        leaf: Some(ProjectedLeaf {
                            template_index: 0,
                            producer: None,
                        }),
                        ..
                    }
                ));
            }
            let actual = match &staged {
                StagedOperand::Completed { completed, .. } => completed.scoped().clone(),
                StagedOperand::OpenCompleted { preview, .. } => preview.clone(),
                _ => unreachable!("a certified UFCS source must complete its staged header"),
            };
            let (store, _owner, delta, _publication) = frontier.parts_mut();
            let actual = store
                .require_goal_free_after_delta(delta, actual, &tcx)
                .unwrap_or_else(|error| {
                    panic!("projected regular UFCS `{name}` stayed open: {error:?}")
                });
            assert!(matches!(
                actual.ty().as_type(),
                crate::ast::Type::Path { segments, .. }
                    if segments.last().is_some_and(|segment| segment.name == "String")
            ));
            let public = store
                .zonk_for_progress(delta, public, &tcx)
                .expect("zonk projected regular UFCS public type");
            store
                .require_goal_free_after_delta(delta, public, &tcx)
                .expect("projected regular UFCS must close its public type");
        }
    }

    #[test]
    fn projected_regular_ufcs_certificate_preserves_call_shape_boundaries() {
        let package = fixture(
            "module projected_regular_ufcs_controls; \
             host type String role(str); \
             host type Box[A]; \
             type Drop[A] = .; \
             host fn identity[A](value: A) -> A; \
             host fn fixed[A](value: A) -> String; \
             host fn lift[*F][A](value: F(A)) -> A; \
             host fn dropped[A](value: Drop(A)) -> A; \
             fn closed(value: String) -> String { value.>fixed } \
             fn chained(value: String) -> String { value.>identity.>identity } \
             fn nested(value: String) -> String { identity(value.>identity) } \
             fn hkt(value: Box(String)) -> String { value.>lift(Box, _) } \
             fn erased() -> . { ().>dropped } \
             fn ambient_rec_order(value: Box(String)) -> String { value.>lift(Box, _) } \
             fn dependent_rec_order(value: Box(String)) -> String { pending.>lift(Box, _) } \
             fn rec_order(value: Box(String)) -> String { value.>lift(Box, _) }",
            "projected_regular_ufcs_controls",
        );
        let module = &package
            .module("projected_regular_ufcs_controls")
            .expect("projected regular UFCS controls module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected regular UFCS controls environment");
        let pending_source = match &env.fn_defs["hkt"].body {
            crate::ast::Expr::Ufcs { receiver, .. } => receiver.as_ref(),
            _ => panic!("the HKT control must retain its UFCS receiver"),
        };

        for (name, certified, closes_public) in [
            ("closed", false, false),
            ("chained", false, false),
            ("nested", false, false),
            ("hkt", true, true),
            ("erased", true, false),
            ("ambient_rec_order", true, true),
            ("dependent_rec_order", false, false),
            ("rec_order", false, false),
        ] {
            let (item_index, witness) = module
                .items
                .iter()
                .enumerate()
                .find_map(|(index, item)| match item {
                    crate::ast::Item::FnDef(def) if def.name == name => Some((index, def)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("projected regular UFCS control `{name}`"));
            let source = &witness.body;
            let mut elaborations = Elaborations::new();
            let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations).at_item(item_index);
            for parameter in witness.sig.value_params() {
                tcx.push_value(
                    parameter.name.clone(),
                    parameter
                        .ty
                        .clone()
                        .expect("UFCS control parameter annotation"),
                );
            }
            match name {
                "ambient_rec_order" | "dependent_rec_order" => {
                    tcx.push_pending_rec_order(
                        "pending".to_owned(),
                        pending_source,
                        pending_source.span(),
                    );
                }
                "rec_order" => {
                    tcx.push_pending_rec_order("pending".to_owned(), source, source.span());
                }
                _ => {}
            }
            let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("open projected regular UFCS control root");
            let (reservation, ()) = root
                .reserve_root_child_with(
                    crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                    source.span(),
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve projected regular UFCS control premise");
            let mut call = root
                .open_reserved_child(reservation)
                .expect("open projected regular UFCS control premise");
            let mut frontier = root.retained_value_frontier(&mut call);
            let public = frontier
                .reserve_projected_header_hole(source.span())
                .expect("reserve projected regular UFCS control public type");
            let owner = frontier.owner();
            let continuation = super::super::OrdinaryValueCursor::begin_child(
                source,
                Some(super::super::OwnerBoundType::Retained {
                    owner,
                    ty: public.clone(),
                }),
                &mut frontier,
                &mut tcx,
            )
            .unwrap_or_else(|error| {
                panic!("plan projected regular UFCS control `{name}`: {error:?}")
            });
            assert_eq!(
                certified_fresh_path_unit_call(&continuation, &tcx),
                certified,
                "unexpected Path/Unit certificate result for `{name}`"
            );
            assert!(
                !certified_fresh_scalar_literal_call(&continuation, &tcx),
                "`{name}` unexpectedly entered the scalar UFCS family"
            );

            let mut staged = stage_projected_source_operand(continuation, &mut frontier, &mut tcx)
                .unwrap_or_else(|error| {
                    panic!("stage projected regular UFCS control `{name}`: {error:?}")
                });
            if certified {
                assert!(matches!(
                    &staged,
                    StagedOperand::Completed { leaf: None, .. }
                ));
                let StagedOperand::Completed { completed, .. } = &staged else {
                    unreachable!("a certified UFCS control changed staged family")
                };
                let (store, _owner, delta, _publication) = frontier.parts_mut();
                let public = store
                    .zonk_for_progress(delta, public, &tcx)
                    .expect("zonk certified UFCS control public type");
                assert_eq!(
                    store
                        .require_goal_free_after_delta(delta, public, &tcx)
                        .is_ok(),
                    closes_public,
                    "unexpected public closure for `{name}`"
                );
                if closes_public {
                    store
                        .require_goal_free_after_delta(delta, completed.scoped().clone(), &tcx)
                        .expect("HKT UFCS control must close its result");
                }
            } else {
                assert!(matches!(&staged, StagedOperand::Ordinary(_, None)));
            }
            if matches!(
                name,
                "ambient_rec_order" | "dependent_rec_order" | "rec_order"
            ) {
                let mut next_template = 0;
                let mut next_producer = 0;
                let mut producer_locations = Vec::new();
                reserve_projected_source(
                    &mut staged,
                    0,
                    &mut next_template,
                    &mut next_producer,
                    &mut producer_locations,
                );
                assert_eq!(next_template, 1);
                assert_eq!(next_producer, u32::from(!certified));
                assert_eq!(producer_locations.len(), next_producer as usize);
            }
        }
    }

    #[test]
    fn projected_scalar_call_retries_only_when_literals_contribute_every_result_goal() {
        let package = fixture(
            "module projected_scalar_call_header; \
             host type String role(str); \
             host type Box[A]; \
             type Identity[A] = A; \
             type Keep[A][B] = A; \
             type F[A] = .; \
             host fn identity[A](value: A) -> A; \
             host fn boxed[A](value: A) -> Box(A); \
             host fn alias_boxed[A](value: Identity(A)) -> Box(A); \
             host fn phantom[A, B](value: A) -> B; \
             host fn applied[*F][A](value: F(A)) -> A; \
             host fn dropped[A][B](value: Keep(Box(A), B)) -> B; \
             host fn shadowed[*F][A][B](value: A) -> F(B); \
             host fn mixed[A](literal: A, known: A) -> Box(A); \
             fn identity_case() -> String { identity(\"value\") } \
             fn boxed_case() -> Box(String) { boxed(\"value\") } \
             fn alias_case() -> Box(String) { alias_boxed(\"value\") } \
             fn shadowed_case[*F]() -> . { shadowed(F, \"value\") } \
             fn phantom_case() -> . { phantom(\"value\") } \
             fn applied_case() -> . { applied(\"value\") } \
             fn dropped_case() -> . { dropped(\"value\") } \
             fn mixed_case(value: String) -> Box(String) { mixed(\"value\", value) }",
            "projected_scalar_call_header",
        );
        let module = &package
            .module("projected_scalar_call_header")
            .expect("projected scalar call module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected scalar call environment");

        for (name, expected_pre_reserve_complete, expected_lexical_complete) in [
            ("identity_case", false, true),
            ("boxed_case", false, true),
            ("alias_case", false, true),
            ("shadowed_case", false, false),
            ("phantom_case", false, false),
            ("applied_case", false, false),
            ("dropped_case", false, false),
            ("mixed_case", false, true),
        ] {
            let (item_index, witness) = module
                .items
                .iter()
                .enumerate()
                .find_map(|(index, item)| match item {
                    crate::ast::Item::FnDef(def) if def.name == name => Some((index, def)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("projected scalar call `{name}` witness"));
            let source = &witness.body;
            let mut elaborations = Elaborations::new();
            let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations).at_item(item_index);
            for parameter in witness.sig.params.iter() {
                match parameter {
                    crate::ast::SignatureParam::Type(param) => {
                        tcx.push_type_param_kinded(&param.name, param.effective_kind(), param.span);
                    }
                    crate::ast::SignatureParam::Value(param) => tcx.push_value(
                        param.name.clone(),
                        param.ty.clone().expect("witness parameter annotation"),
                    ),
                }
            }
            let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("open projected scalar call root");
            let (reservation, ()) = root
                .reserve_root_child_with(
                    crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                    source.span(),
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve projected scalar action premise");
            let mut call = root
                .open_reserved_child(reservation)
                .expect("open projected scalar action premise");
            let mut frontier = root.retained_value_frontier(&mut call);
            let public = frontier
                .reserve_projected_header_hole(source.span())
                .expect("reserve open public scalar call type");
            let owner = frontier.owner();
            let continuation = super::super::OrdinaryValueCursor::begin_child(
                source,
                Some(super::super::OwnerBoundType::Retained { owner, ty: public }),
                &mut frontier,
                &mut tcx,
            )
            .unwrap_or_else(|error| panic!("plan projected scalar call `{name}`: {error:?}"));
            assert!(certified_one_layer_scalar_literal_source(source, &tcx));
            assert!(certified_fresh_scalar_literal_call(&continuation, &tcx));
            let mut staged = stage_projected_source_operand(continuation, &mut frontier, &mut tcx)
                .unwrap_or_else(|error| panic!("stage projected scalar call `{name}`: {error:?}"));
            assert_eq!(
                matches!(staged, StagedOperand::Completed { .. }),
                expected_pre_reserve_complete,
                "unexpected pre-reserve completion for `{name}`"
            );
            if !expected_pre_reserve_complete {
                assert!(matches!(
                    &staged,
                    StagedOperand::Ordinary(continuation, None)
                        if continuation.next_mode
                            == super::super::frontier::FrontierMode::LexicalFallback
                ));
            }

            let mut next_template = 0;
            let mut next_producer = 0;
            let mut producer_locations = Vec::new();
            reserve_projected_source(
                &mut staged,
                0,
                &mut next_template,
                &mut next_producer,
                &mut producer_locations,
            );
            assert_eq!(next_template, 1);
            assert_eq!(next_producer, u32::from(!expected_pre_reserve_complete));
            if !expected_pre_reserve_complete {
                staged = prepare_projected_source_header(
                    staged,
                    super::super::frontier::FrontierMode::LexicalFallback,
                    &mut frontier,
                    &mut tcx,
                )
                .unwrap_or_else(|error| {
                    panic!("prepare projected scalar call `{name}`: {error:?}")
                });
                assert_eq!(
                    matches!(staged, StagedOperand::OpenCompleted { .. }),
                    expected_lexical_complete,
                    "unexpected lexical completion for `{name}`"
                );
                if !expected_lexical_complete {
                    assert!(matches!(
                        staged,
                        StagedOperand::Ordinary(
                            super::super::OrdinaryChildContinuation {
                                next_mode: super::super::frontier::FrontierMode::LexicalFallback,
                                ..
                            },
                            Some(ProjectedLeaf {
                                template_index: 0,
                                producer: Some(0),
                            })
                        )
                    ));
                }
            }
            if expected_pre_reserve_complete || expected_lexical_complete {
                let actual = match &staged {
                    StagedOperand::Completed { completed, .. } => completed.scoped().clone(),
                    StagedOperand::OpenCompleted { preview, .. } => preview.clone(),
                    _ => unreachable!("a completed scalar causal changed staged family"),
                };
                let (store, _owner, delta, _publication) = frontier.parts_mut();
                store
                    .require_goal_free_after_delta(delta, actual, &tcx)
                    .unwrap_or_else(|error| {
                        panic!("completed projected scalar call `{name}` stayed open: {error:?}")
                    });
            }
        }
    }

    #[test]
    fn projected_path_unit_source_certificate_excludes_other_expression_families() {
        let package = fixture(
            "module projected_path_unit_call_controls; \
             host type String role(str); \
             fn identity[A](value: A) -> A { value } \
             fn unit_value() -> . { identity(()) } \
             fn literal_value() -> String { identity(\"value\") } \
             fn nested_value() -> . { identity(identity(())) } \
             host fn make[A]() -> A; \
             fn empty_value() -> . { make() } \
             fn open_nested_value() -> . { identity(make()) } \
             fn lambda_value() -> . -> . { identity(.(value: .) -> . { value }) } \
             fn type_only_value() -> [A] A { identity(_) } \
             fn concrete_type_only_value() -> . -> . { identity(.) }",
            "projected_path_unit_call_controls",
        );
        let module = &package
            .module("projected_path_unit_call_controls")
            .expect("projected path/Unit call controls module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected path/Unit call controls environment");
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);

        for name in [
            "literal_value",
            "lambda_value",
            "type_only_value",
            "concrete_type_only_value",
        ] {
            let source = &env.fn_defs[name].body;
            assert!(
                !certified_path_unit_call_tree_source(source, source, &tcx),
                "`{name}` unexpectedly entered the projected Path/Unit call family"
            );
        }

        let unit_source = &env.fn_defs["unit_value"].body;
        assert!(certified_path_unit_call_tree_source(
            &env.fn_defs["empty_value"].body,
            &env.fn_defs["empty_value"].body,
            &tcx
        ));
        assert!(certified_path_unit_call_tree_source(
            &env.fn_defs["open_nested_value"].body,
            &env.fn_defs["open_nested_value"].body,
            &tcx
        ));
        assert!(certified_path_unit_call_tree_source(
            &env.fn_defs["nested_value"].body,
            &env.fn_defs["nested_value"].body,
            &tcx
        ));
        assert!(certified_path_unit_call_tree_source(
            unit_source,
            unit_source,
            &tcx
        ));
        tcx.push_pending_rec_order("pending".to_owned(), unit_source, unit_source.span());
        assert!(
            !certified_path_unit_call_tree_source(unit_source, unit_source, &tcx),
            "pending recursive-order work must exclude the call-header fast path"
        );
    }

    #[test]
    fn projected_path_unit_call_tree_uses_iterative_nested_work() {
        let package = fixture(
            "module projected_nested_path_unit; \
             fn identity[A](value: A) -> A { value } \
             fn pair[A][B](left: A, right: B) -> A & B { (left, right) } \
             fn chain() -> . { identity(()) } \
             fn branch() -> . & . { pair(identity(()), identity(())) }",
            "projected_nested_path_unit",
        );
        let module = &package.module("projected_nested_path_unit").unwrap().module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .unwrap();
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let branch = &env.fn_defs["branch"].body;
        assert!(certified_path_unit_call_tree_source(branch, branch, &tcx));
        let seed = &env.fn_defs["chain"].body;
        let mut nested = seed.clone();
        for _ in 0..4096 {
            let mut outer = seed.clone();
            let Expr::Call { args, .. } = &mut outer else {
                panic!("the source seed must retain a direct call")
            };
            args[0] = crate::ast::CallArg::Value(nested);
            nested = outer;
        }
        assert!(certified_path_unit_call_tree_source(&nested, &nested, &tcx));
        let Expr::Call { args, .. } = &nested else {
            unreachable!()
        };
        let crate::ast::CallArg::Value(inner) = &args[0] else {
            unreachable!()
        };
        tcx.push_pending_rec_order("pending".to_owned(), inner, inner.span());
        assert!(!certified_path_unit_call_tree_source(
            &nested, &nested, &tcx
        ));
        drop(tcx);
        // Consume this deliberately deep private AST without recursive drop glue.
        while let Expr::Call { mut args, .. } = nested {
            let crate::ast::CallArg::Value(inner) = args.remove(0) else {
                unreachable!()
            };
            nested = inner;
        }
    }

    #[test]
    fn projected_computed_field_certificate_has_a_closed_recursive_syntax_boundary() {
        let span = crate::span::Span::new(10, 20);
        let meta = || Meta::<Lowered>::new(span);
        let segment = |name: &str| PathSegment {
            name: name.to_owned(),
            span,
        };
        let path = |name: &str| Expr::Path {
            occurrence: Default::default(),
            segments: vec![segment(name)],
            meta: meta(),
            ext: (),
        };
        let unit = || Expr::Unit {
            occurrence: Default::default(),
            meta: meta(),
        };
        let literal = |annotation| Expr::StrLit {
            occurrence: Default::default(),
            value: "value".to_owned(),
            annotation,
            meta: meta(),
        };
        let call = |callee, args| Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(callee),
            args,
            meta: meta(),
            ext: (),
        };
        let access = |receiver| Expr::Elaborator {
            occurrence: Default::default(),
            kind: crate::ast::ElaboratorKind::Access,
            call: crate::ast::ElaboratorCall::FieldAccess {
                receiver: Box::new(receiver),
                labels: vec![crate::ast::FieldAccessLabel {
                    label: "value".to_owned(),
                    label_span: span,
                    label_type: Some(vec![segment("Value")]),
                    meta: meta(),
                }],
            },
            meta: meta(),
            ext: crate::ast::NodeId(10),
        };
        let root = |argument| {
            call(
                access(path("surface")),
                vec![crate::ast::CallArg::Value(argument)],
            )
        };
        let concrete = crate::ast::Type::Unit { meta: meta() };
        let inferred = crate::ast::Type::Infer {
            meta: meta(),
            ext: (),
        };
        let goal = crate::ast::Type::Goal {
            goal: crate::ast::TypeGoalRef::for_test(23, 0, 0),
            args: Vec::new(),
            meta: meta(),
            ext: (),
        };
        let structured_infer = crate::ast::Type::Function {
            param: Box::new(inferred.clone()),
            ret: Box::new(concrete.clone()),
            meta: meta(),
            abi_arity: 1,
            caps: (),
        };

        for source in [
            root(path("value")),
            root(unit()),
            root(literal(None)),
            root(access(path("surface"))),
            root(call(
                path("identity"),
                vec![crate::ast::CallArg::Value(path("value"))],
            )),
            root(call(
                path("pair"),
                vec![
                    crate::ast::CallArg::Type(concrete.clone()),
                    crate::ast::CallArg::Value(access(path("surface"))),
                    crate::ast::CallArg::Value(call(
                        access(path("surface")),
                        vec![crate::ast::CallArg::Value(literal(Some(concrete.clone())))],
                    )),
                ],
            )),
            root(call(
                path("pair"),
                vec![
                    crate::ast::CallArg::Type(inferred.clone()),
                    crate::ast::CallArg::Type(inferred.clone()),
                    crate::ast::CallArg::Value(path("value")),
                    crate::ast::CallArg::Value(access(path("surface"))),
                ],
            )),
        ] {
            assert!(
                certified_computed_field_root(&source),
                "a bounded computed-field source was rejected: {source:#?}"
            );
        }

        let computed_receiver = access(call(
            path("identity"),
            vec![crate::ast::CallArg::Value(path("surface"))],
        ));
        assert!(path_receiver_field_access(&access(path("surface"))));
        assert!(!path_receiver_field_access(&computed_receiver));
        let update = Expr::Elaborator {
            occurrence: Default::default(),
            kind: crate::ast::ElaboratorKind::Filtered,
            call: crate::ast::ElaboratorCall::FieldUpdate {
                receiver: Box::new(path("surface")),
                updates: vec![crate::ast::FieldUpdateLabel {
                    label: "value".to_owned(),
                    label_span: span,
                    label_type: Some(vec![segment("Value")]),
                    value: path("value"),
                    meta: meta(),
                }],
            },
            meta: meta(),
            ext: crate::ast::NodeId(11),
        };
        assert!(!path_receiver_field_access(&update));
        let lambda = Expr::FnExpr {
            occurrence: Default::default(),
            sig: crate::ast::Signature::from_groups(Vec::new()),
            ret_ty: None,
            body: Box::new(unit()),
            meta: meta(),
            caps: (),
        };
        let let_value = Expr::Let {
            occurrence: Default::default(),
            name: "local".to_owned(),
            name_span: span,
            ty: None,
            pattern: (),
            value: Box::new(path("value")),
            body: Box::new(path("local")),
            meta: meta(),
        };
        let sequence = Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(unit()),
            body: Box::new(path("value")),
            meta: meta(),
        };
        let bang = Expr::UserElaborator {
            occurrence: Default::default(),
            form: crate::ast::UserElaboratorCallForm::Ordinary,
            name: "user".to_owned(),
            args: Vec::new(),
            meta: meta(),
            ext: crate::ast::NodeId(13),
        };
        let rec_order = Expr::RecOrder {
            occurrence: Default::default(),
            plan: Box::new(crate::ast::RecOrderPlan {
                tail_continuation: None,
                name: "ordered".to_owned(),
                disposition: crate::ast::RecOrderDisposition::Ordered(
                    crate::ast::RecOrderTypeFlow::SynthesizedValue,
                ),
                annotation: None,
                value: Box::new(path("value")),
                body: Box::new(path("value")),
                runtime_ty: concrete.clone(),
            }),
            meta: meta(),
            ext: crate::ast::NodeId(14),
        };

        for source in [
            call(
                computed_receiver,
                vec![crate::ast::CallArg::Value(path("value"))],
            ),
            call(update, vec![crate::ast::CallArg::Value(path("value"))]),
            root(lambda),
            root(let_value),
            root(sequence),
            root(bang),
            root(rec_order),
            call(
                access(path("surface")),
                vec![crate::ast::CallArg::Type(inferred.clone())],
            ),
            root(call(
                access(path("surface")),
                vec![crate::ast::CallArg::Type(inferred.clone())],
            )),
            call(
                access(path("surface")),
                vec![crate::ast::CallArg::Type(goal.clone())],
            ),
            root(call(
                path("ordinary"),
                vec![crate::ast::CallArg::Type(goal.clone())],
            )),
            root(call(
                path("ordinary"),
                vec![crate::ast::CallArg::Type(structured_infer)],
            )),
            root(literal(Some(inferred))),
            root(literal(Some(goal))),
            call(
                path("ordinary"),
                vec![crate::ast::CallArg::Value(path("value"))],
            ),
        ] {
            assert!(
                !certified_computed_field_root(&source),
                "an excluded computed-field source was admitted: {source:#?}"
            );
        }
    }

    #[test]
    fn projected_field_sources_complete_or_retain_producer_work_at_expected_only() {
        let package = fixture(
            "module projected_computed_field_header; \
             host type String role(str); \
             pub labels { lookup[P]: String -> P | . }; \
             pub labels { inject[P]: String -> P }; \
             pub labels { apply[P]: (P & P) -> P | . }; \
             pub labels { unit_in[P]: P }; \
             pub labels { identity: [A] A -> A }; \
             type Surface[P] = \
               Lookup(P) & Inject(P) & Apply(P) & Unit_in(P) & Identity; \
             fn known() -> String { \"known\"(String) } \
             fn identity[A](value: A) -> A { value } \
             fn projection[P](surface: Surface(P)) -> P { surface.?{unit_in} } \
             fn nonpath[P](surface: Surface(P)) -> P { identity(surface).?{unit_in} } \
             fn simple[P](surface: Surface(P), name: String) -> P | . { \
               surface.?{lookup}(name) \
             } \
             fn direct[P](surface: Surface(P)) -> P | . { \
               surface.?{lookup}(known()) \
             } \
             fn nested[P](surface: Surface(P), value: P, name: String) -> P | . { \
               surface.?{apply}((value, surface.?{inject}(name))) \
             } \
             fn bare[P](surface: Surface(P), value: P) -> P | . { \
               surface.?{apply}((value, surface.?{unit_in})) \
             } \
             fn pending[P](surface: Surface(P)) -> String { \
               surface.?{identity}(\"value\") \
             }",
            "projected_computed_field_header",
        );
        let module = &package
            .module("projected_computed_field_header")
            .expect("projected computed-field module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected computed-field environment");

        for (name, expected_complete) in [
            ("projection", true),
            ("nonpath", true),
            ("simple", true),
            ("direct", true),
            ("nested", true),
            ("bare", true),
            ("pending", false),
        ] {
            let (item_index, witness) = module
                .items
                .iter()
                .enumerate()
                .find_map(|(index, item)| match item {
                    crate::ast::Item::FnDef(def) if def.name == name => Some((index, def)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("projected computed-field `{name}` witness"));
            let source = &witness.body;
            let mut elaborations = Elaborations::new();
            let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations).at_item(item_index);
            for parameter in &witness.sig.params {
                match parameter {
                    crate::ast::SignatureParam::Type(parameter) => tcx.push_type_param_kinded(
                        &parameter.name,
                        parameter.effective_kind(),
                        parameter.span,
                    ),
                    crate::ast::SignatureParam::Value(parameter) => tcx.push_value(
                        parameter.name.clone(),
                        parameter.ty.clone().expect("witness parameter annotation"),
                    ),
                }
            }
            let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("open projected computed-field root");
            let (reservation, ()) = root
                .reserve_root_child_with(
                    crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                    source.span(),
                    &tcx,
                    |_store, _owner| Ok(()),
                )
                .expect("reserve projected computed-field premise");
            let mut call = root
                .open_reserved_child(reservation)
                .expect("open projected computed-field premise");
            let mut frontier = root.retained_value_frontier(&mut call);
            let public = frontier
                .reserve_projected_header_hole(source.span())
                .expect("reserve projected computed-field public type");
            let owner = frontier.owner();
            let mut continuation = super::super::OrdinaryValueCursor::begin_child(
                source,
                Some(super::super::OwnerBoundType::Retained {
                    owner,
                    ty: public.clone(),
                }),
                &mut frontier,
                &mut tcx,
            )
            .unwrap_or_else(|error| panic!("plan projected computed-field `{name}`: {error:?}"));
            assert_eq!(
                continuation.next_mode,
                super::super::frontier::FrontierMode::ExpectedOnly
            );
            assert!(matches!(
                &continuation.cursor.planned,
                super::super::PlannedOrdinaryValue::Recursive(
                    super::super::OrdinaryRecursiveFamily::ComputedCall
                        | super::super::OrdinaryRecursiveFamily::Field
                )
            ));
            if name == "nonpath" {
                assert!(
                    !certified_fresh_field_source(&continuation, &tcx),
                    "a computed receiver must remain outside the private field certificate"
                );
                continue;
            }
            assert!(
                certified_fresh_field_source(&continuation, &tcx),
                "projected computed-field `{name}` missed the fresh certificate"
            );
            if matches!(name, "simple" | "projection") {
                continuation.next_mode = super::super::frontier::FrontierMode::LexicalFallback;
                assert!(
                    !certified_fresh_field_source(&continuation, &tcx),
                    "a progressed computed-field continuation was re-certified"
                );
                continuation.next_mode = super::super::frontier::FrontierMode::ExpectedOnly;
                let mark = tcx.save();
                tcx.push_pending_rec_order("pending".to_owned(), source, source.span());
                assert!(
                    !certified_fresh_field_source(&continuation, &tcx),
                    "pending recursive-order work must block computed-field staging"
                );
                tcx.restore(mark);
                assert!(certified_fresh_field_source(&continuation, &tcx));
            }

            let mut staged = stage_projected_source_operand(continuation, &mut frontier, &mut tcx)
                .unwrap_or_else(|error| {
                    panic!("stage projected computed-field `{name}`: {error:?}")
                });
            assert_eq!(
                matches!(staged, StagedOperand::Completed { .. }),
                expected_complete,
                "unexpected ExpectedOnly completion for `{name}`"
            );
            if expected_complete {
                let StagedOperand::Completed { completed, .. } = &staged else {
                    unreachable!("a completed computed-field source changed family")
                };
                assert!(!crate::pass::typecheck_core::type_contains_goal(
                    completed.scoped().ty().as_type()
                ));
                assert!(!crate::pass::typecheck_core::type_contains_infer(
                    completed.scoped().ty().as_type()
                ));
                let progressed = {
                    let (store, _owner, delta, _publication) = frontier.parts_mut();
                    store
                        .zonk_for_progress(delta, public.clone(), &tcx)
                        .expect("zonk the computed-field public type")
                };
                let (store, _owner, delta, _publication) = frontier.parts_mut();
                store
                    .require_goal_free_after_delta(delta, progressed, &tcx)
                    .expect("a completed computed-field source must close its public header");
            } else {
                assert!(matches!(
                    &staged,
                    StagedOperand::Ordinary(continuation, None)
                        if continuation.next_mode
                            == super::super::frontier::FrontierMode::LexicalFallback
                ));
            }

            let mut next_template = 0;
            let mut next_producer = 0;
            let mut producer_locations = Vec::new();
            reserve_projected_source(
                &mut staged,
                0,
                &mut next_template,
                &mut next_producer,
                &mut producer_locations,
            );
            assert_eq!(next_template, 1);
            assert_eq!(next_producer, u32::from(!expected_complete));
            if expected_complete {
                assert!(matches!(
                    staged,
                    StagedOperand::Completed {
                        leaf: Some(ProjectedLeaf {
                            template_index: 0,
                            producer: None,
                        }),
                        ..
                    }
                ));
            } else {
                assert_eq!(producer_locations.len(), 1);
                assert_eq!(producer_locations[0].source, 0);
                assert!(producer_locations[0].path.is_empty());
                assert!(matches!(
                    &staged,
                    StagedOperand::Ordinary(
                        _,
                        Some(ProjectedLeaf {
                            template_index: 0,
                            producer: Some(0),
                        })
                    )
                ));
                let seal = InvocationSeal::fresh();
                let mut specialization =
                    crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(&tcx);
                let mut recipe = seal_projected_operand_for_test(
                    &mut staged,
                    public,
                    &seal,
                    &mut specialization,
                    &mut frontier,
                    &mut tcx,
                )
                .expect("seal the pending computed-field recipe");
                snapshot_projected_recipe(&mut recipe, &mut specialization, &mut frontier, &tcx)
                    .expect("snapshot the pending computed-field recipe");
                assert!(!recipe.ty.producers.is_empty());
                assert!(recipe.ty.closed.is_none());
                let error = validate_projected_source_before_action(&staged, &recipe)
                    .expect_err("a pending computed-field producer must fail before the action");
                assert_eq!(error.diagnostic().span, source.span());
                assert!(
                    error
                        .diagnostic()
                        .message
                        .contains("value source needs a complete type")
                );
                assert_eq!(
                    error
                        .diagnostic()
                        .extra
                        .as_ref()
                        .and_then(|extra| extra.help.as_deref()),
                    Some(
                        "add a type argument or annotation, or complete the value in an ordinary `let` first"
                    )
                );
            }
        }
    }

    #[test]
    fn projected_path_call_fast_path_does_not_complete_a_nested_pair_leaf() {
        let package = fixture(
            "module projected_nested_path_call; \
             host type String role(str); \
             host fn phantom[A, B](value: A) -> B; \
             fn witness(value: String) -> . & . { (value.>phantom, ()) }",
            "projected_nested_path_call",
        );
        let module = &package
            .module("projected_nested_path_call")
            .expect("projected nested Path call module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected nested Path call environment");
        let (item_index, witness) = module
            .items
            .iter()
            .enumerate()
            .find_map(|(index, item)| match item {
                crate::ast::Item::FnDef(def) if def.name == "witness" => Some((index, def)),
                _ => None,
            })
            .expect("projected nested Path call witness");
        let source = &witness.body;
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations).at_item(item_index);
        for parameter in witness.sig.value_params() {
            tcx.push_value(
                parameter.name.clone(),
                parameter.ty.clone().expect("witness parameter annotation"),
            );
        }
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            source.span(),
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open projected nested Path call root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve projected nested Path action premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open projected nested Path action premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let public = frontier
            .reserve_projected_header_hole(source.span())
            .expect("reserve projected nested Path public type");
        let owner = frontier.owner();
        let mut staged = stage_operand(
            source,
            super::super::OwnerBoundType::Retained { owner, ty: public },
            &mut frontier,
            &mut tcx,
        )
        .expect("stage projected nested Path pair");
        let StagedOperand::Pair(pair) = &staged else {
            panic!("the projected nested Path source must retain its pair wrapper")
        };
        let left = pair.operands[0]
            .state
            .as_ref()
            .expect("the projected nested Path pair lost its left operand");
        let StagedOperand::Ordinary(continuation, None) = left else {
            panic!("the nested Path call must retain ordinary producer work")
        };
        assert!(
            certified_fresh_path_unit_call(continuation, &tcx),
            "the nested call must be a genuine fast-path counterfactual"
        );
        assert!(matches!(
            pair.operands[1]
                .state
                .as_ref()
                .expect("the projected nested Path pair lost its right operand"),
            StagedOperand::Completed { .. }
        ));

        let mut next_template = 0;
        let mut next_producer = 0;
        let mut producer_locations = Vec::new();
        reserve_projected_source(
            &mut staged,
            0,
            &mut next_template,
            &mut next_producer,
            &mut producer_locations,
        );
        assert_eq!((next_template, next_producer), (2, 1));
        assert_eq!(producer_locations[0].path.as_ref(), &[0]);
        let StagedOperand::Pair(pair) = &mut staged else {
            unreachable!("the nested Path pair changed staged family")
        };
        let mut captured = (0..next_template)
            .map(|_| ProjectedTemplateSlot::default())
            .collect::<Vec<_>>();
        capture_completed_pair_sources(pair, 0, &mut captured, &mut frontier, &tcx)
            .expect("capture only the producer-free nested pair prefix");
        assert!(captured[0].source.is_none());
        assert!(captured[1].source.is_some());
    }

    #[test]
    fn projected_nested_pair_scalar_call_closes_in_its_lexical_pass() {
        let package = fixture(
            "module projected_nested_scalar_call; \
             host type String role(str); \
             host fn identity[A](value: A) -> A; \
             fn witness() -> . & . { (\"value\".>identity, ()) }",
            "projected_nested_scalar_call",
        );
        let module = &package
            .module("projected_nested_scalar_call")
            .expect("projected nested scalar call module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected nested scalar call environment");
        let source = &env.fn_defs["witness"].body;
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            source.span(),
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open projected nested scalar call root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve projected nested scalar action premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open projected nested scalar action premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let public = frontier
            .reserve_projected_header_hole(source.span())
            .expect("reserve projected nested scalar public type");
        let owner = frontier.owner();
        let mut staged = stage_operand(
            source,
            super::super::OwnerBoundType::Retained { owner, ty: public },
            &mut frontier,
            &mut tcx,
        )
        .expect("stage projected nested scalar pair");
        let StagedOperand::Pair(pair) = &staged else {
            panic!("the projected nested scalar source must retain its pair wrapper")
        };
        let left = pair.operands[0]
            .state
            .as_ref()
            .expect("the projected nested scalar pair lost its left operand");
        let StagedOperand::Ordinary(continuation, None) = left else {
            panic!("the nested scalar call must retain ordinary producer work")
        };
        assert_eq!(
            continuation.next_mode,
            super::super::frontier::FrontierMode::ExpectedOnly
        );
        assert!(
            certified_fresh_scalar_literal_call(continuation, &tcx),
            "the nested scalar call must be a genuine fast-path counterfactual"
        );

        let mut next_template = 0;
        let mut next_producer = 0;
        let mut producer_locations = Vec::new();
        reserve_projected_source(
            &mut staged,
            0,
            &mut next_template,
            &mut next_producer,
            &mut producer_locations,
        );
        assert_eq!((next_template, next_producer), (2, 1));
        assert_eq!(producer_locations[0].path.as_ref(), &[0]);
        staged = prepare_staged_header(
            staged,
            super::super::frontier::FrontierMode::LexicalFallback,
            &mut frontier,
            &mut tcx,
        )
        .expect("prepare the nested pair through its ordinary lexical pass");
        let StagedOperand::Pair(pair) = &mut staged else {
            unreachable!("the nested scalar pair changed staged family")
        };
        let StagedOperand::OpenCompleted {
            preview,
            leaf:
                Some(ProjectedLeaf {
                    template_index: 0,
                    producer: Some(0),
                }),
            ..
        } = pair.operands[0]
            .state
            .as_ref()
            .expect("the nested scalar pair lost its retained producer")
        else {
            panic!("the nested scalar call must complete before source eligibility")
        };
        assert!(!crate::pass::typecheck_core::type_contains_goal(
            preview.ty().as_type()
        ));
        assert!(!crate::pass::typecheck_core::type_contains_infer(
            preview.ty().as_type()
        ));
        let mut captured = (0..next_template)
            .map(|_| ProjectedTemplateSlot::default())
            .collect::<Vec<_>>();
        capture_completed_pair_sources(pair, 0, &mut captured, &mut frontier, &tcx)
            .expect("capture only the producer-free nested scalar pair prefix");
        assert!(captured[0].source.is_none());
        assert!(captured[1].source.is_some());
    }

    #[test]
    fn projected_header_prepare_does_not_retry_a_deferred_leaf() {
        let package = fixture("module projected_leaf_header;", "projected_leaf_header");
        let module = &package
            .module("projected_leaf_header")
            .expect("projected leaf module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected leaf environment");
        let span = crate::span::Span::new(40, 41);
        let source = Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(span),
        };
        let mut elaborations = Elaborations::new();
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let goal_type = |goal| crate::ast::Type::Goal {
            goal,
            args: Vec::new(),
            meta: Meta::new(span),
            ext: (),
        };
        let (mut root, ancestor) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            span,
            &tcx,
            |store, owner| {
                store.alloc_goal(
                    owner,
                    crate::ast::Kind::Star,
                    crate::pass::typecheck_core::GoalSolutionPolicy::Monotype,
                    crate::pass::typecheck_core::GoalOrigin::named(
                        span,
                        crate::pass::typecheck_core::GoalRole::TypeArgument,
                        "Source",
                    ),
                )
            },
        )
        .expect("open projected leaf root");
        let (parent_reservation, descendant) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                span,
                &tcx,
                |store, owner| {
                    store.alloc_goal(
                        owner,
                        crate::ast::Kind::Star,
                        crate::pass::typecheck_core::GoalSolutionPolicy::Monotype,
                        crate::pass::typecheck_core::GoalOrigin::named(
                            span,
                            crate::pass::typecheck_core::GoalRole::TypeArgument,
                            "Expected",
                        ),
                    )
                },
            )
            .expect("reserve projected leaf parent");
        let mut parent_child = root
            .open_reserved_child(parent_reservation)
            .expect("open projected leaf parent");
        let parent = parent_child.owner();
        let expected = root
            .store()
            .scoped_type(
                parent,
                crate::pass::typecheck_core::InternedType::fresh_canonical(
                    crate::ast::Type::Product {
                        left: Box::new(goal_type(descendant)),
                        right: Box::new(crate::ast::Type::Unit {
                            meta: Meta::new(span),
                        }),
                        meta: Meta::new(span),
                    },
                ),
                span,
            )
            .expect("scope descendant-dependent projected leaf expectation");
        let prepared = super::super::PreparedOrdinaryValue {
            source: &source,
            synth: crate::pass::typecheck_core::Synth::value_interned(
                crate::pass::typecheck_core::InternedType::fresh_canonical(goal_type(ancestor)),
            ),
            binders: Vec::new(),
            retained_type: None,
        };
        let mut frontier = root.retained_value_frontier(&mut parent_child);
        let (leaf_reservation, planned) = frontier
            .reserve_ordinary_child_with(span, &tcx, |store, owner| {
                super::super::OrdinaryValueCursor::plan(
                    &source,
                    super::super::OrdinaryValueSeed::Leaf(prepared),
                    store,
                    owner,
                    &tcx,
                )
            })
            .expect("reserve projected deferred leaf");
        let cursor = super::super::OrdinaryValueCursor::bind(
            &source,
            super::super::OwnerBoundType::Retained {
                owner: parent,
                ty: expected,
            },
            planned,
        );
        let child = frontier
            .open_ordinary_child(leaf_reservation)
            .expect("open projected deferred leaf");
        let continuation = super::super::OrdinaryChildContinuation {
            child,
            cursor: Box::new(cursor),
            next_mode: super::super::frontier::FrontierMode::ExpectedOnly,
            suspension: super::super::OrdinarySuspension::Mode,
        };
        let mut staged = stage_existing_operand(continuation, &mut frontier, &mut tcx)
            .expect("stage the deferred leaf's first equation attempt");
        assert!(matches!(
            &staged,
            StagedOperand::Ordinary(continuation, None)
                if continuation.next_mode
                    == super::super::frontier::FrontierMode::LexicalFallback
                    && matches!(
                        &continuation.cursor.planned,
                        super::super::PlannedOrdinaryValue::Leaf(_)
                    )
        ));
        let mut next_template = 0;
        let mut next_producer = 0;
        let mut producer_locations = Vec::new();
        reserve_projected_source(
            &mut staged,
            0,
            &mut next_template,
            &mut next_producer,
            &mut producer_locations,
        );
        staged = prepare_staged_header(
            staged,
            super::super::frontier::FrontierMode::LexicalFallback,
            &mut frontier,
            &mut tcx,
        )
        .expect("prepare headers without retrying the deferred leaf");
        assert_eq!((next_template, next_producer), (1, 1));
        assert!(matches!(
            staged,
            StagedOperand::Ordinary(
                super::super::OrdinaryChildContinuation {
                    next_mode: super::super::frontier::FrontierMode::LexicalFallback,
                    ..
                },
                Some(ProjectedLeaf {
                    template_index: 0,
                    producer: Some(0),
                })
            )
        ));
    }

    #[test]
    fn canonical_pair_projection_preserves_source_order_and_closes_its_wrapper_once() {
        canonical_pair_projection_capture(false);
    }

    #[test]
    fn recursive_quotation_certified_pair_keeps_one_capture_ticket_per_wrapper() {
        canonical_pair_projection_capture(true);
    }

    fn canonical_pair_projection_capture(capture: bool) {
        let package = fixture(
            "module pair_projection_order; \
             fn witness() -> . & (. & .) { ((), ((), ())) }",
            "pair_projection_order",
        );
        let module = &package
            .module("pair_projection_order")
            .expect("pair projection module")
            .module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("pair projection environment");
        let source = &env.fn_defs["witness"].body;
        let expected = env.fn_defs["witness"].ret.clone();
        let mut elaborations = Elaborations::new();
        if capture {
            elaborations.register_rec_quote_sources(source);
        }
        let mut tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let (mut root, ()) = super::super::frontier::OrdinaryFrontier::begin_root_with(
            source.span(),
            &tcx,
            |_store, _owner| Ok(()),
        )
        .expect("open pair projection root");
        let (reservation, ()) = root
            .reserve_root_child_with(
                crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
                source.span(),
                &tcx,
                |_store, _owner| Ok(()),
            )
            .expect("reserve pair projection premise");
        let mut call = root
            .open_reserved_child(reservation)
            .expect("open pair projection premise");
        let mut frontier = root.retained_value_frontier(&mut call);
        let mut staged = stage_operand(
            source,
            super::super::OwnerBoundType::Plain(crate::pass::typecheck_core::InternedType::fresh(
                expected.clone(),
            )),
            &mut frontier,
            &mut tcx,
        )
        .expect("stage canonical pair wrapper");
        assert!(matches!(staged, StagedOperand::Pair(..)));

        let mut next_template = 0;
        let mut next_producer = 0;
        let mut locations = Vec::new();
        reserve_projected_source(
            &mut staged,
            0,
            &mut next_template,
            &mut next_producer,
            &mut locations,
        );
        assert_eq!(next_template, 3);
        assert_eq!(next_producer, 0, "closed leaves must not mint producers");

        let seal = InvocationSeal::fresh();
        let mut specialization =
            crate::pass::typecheck_core::IsolatedSpecializationContextBuilder::new(&tcx);
        let fallback = frontier
            .store()
            .scoped_type(
                frontier.owner(),
                crate::pass::typecheck_core::InternedType::fresh(expected),
                source.span(),
            )
            .expect("scope canonical pair public header");
        let recipe = seal_projected_operand_for_test(
            &mut staged,
            fallback,
            &seal,
            &mut specialization,
            &mut frontier,
            &mut tcx,
        )
        .expect("seal canonical pair recipe");
        let ProjectedRecipeNode::Pair(left, right) = recipe.node.as_ref() else {
            panic!("canonical pair lost its ordered recipe wrapper")
        };
        assert!(matches!(
            left.node.as_ref(),
            ProjectedRecipeNode::TemplateValue(0)
        ));
        let ProjectedRecipeNode::Pair(right_left, right_right) = right.node.as_ref() else {
            panic!("nested canonical pair lost its ordered recipe wrapper")
        };
        assert!(matches!(
            right_left.node.as_ref(),
            ProjectedRecipeNode::TemplateValue(1)
        ));
        assert!(matches!(
            right_right.node.as_ref(),
            ProjectedRecipeNode::TemplateValue(2)
        ));

        reset_pair_wrapper_finalizations();
        let StagedOperand::Pair(mut pair) = staged else {
            unreachable!("the canonical pair changed staged shape")
        };
        let mut template_slots = Vec::new();
        template_slots.resize_with(next_template as usize, ProjectedTemplateSlot::default);
        capture_completed_pair_sources(&mut pair, 0, &mut template_slots, &mut frontier, &tcx)
            .expect("capture producer-free pair sources");
        collapse_initial_completed_pair_children(&mut pair, &mut frontier, &mut tcx)
            .expect("close nested producer-free pair");
        assert!(staged_pair_is_closed(&pair));
        let completed = finalize_staged_pair(*pair, &mut frontier, &mut tcx)
            .expect("finalize top producer-free pair");
        let output = completed.scoped().clone();
        let projection = MarkedProjection {
            seal,
            specialization: None,
            header_cursor: ProjectedHeaderCursor::default(),
            terminal_result: Some(output.clone()),
            transcript: Some(super::super::UserElaboratorCallTranscript {
                action: crate::ast::NodeId(1),
                source_span: source.span(),
                layers: Vec::new(),
                next_source_index: 0,
                terminal_type_layer: None,
            }),
            sources: vec![RetainedProjectedSource {
                state: Some(StagedOperand::Completed {
                    source,
                    completed,
                    leaf: None,
                }),
                header: RetainedProjectedHeader::Sealed(recipe.clone()),
                inherited_mode: super::super::frontier::FrontierMode::ExpectedOnly,
                layer: 0,
                expanded_start: 0,
                expanded_width: 1,
            }],
            next_template,
            next_producer,
            next_close_producer: 0,
            producer_locations: locations,
            producer_completed: Vec::new(),
            template_slots,
        };
        crate::normalization::reset_runtime_callback_adapter_work();
        let materialized = projection
            .materialize(
                ProjectedCheckedResult::Recipe(recipe),
                output,
                Vec::new(),
                source.span(),
                &mut frontier,
                &mut tcx,
            )
            .expect("materialize producer-free nested pair projection");
        assert_eq!(
            crate::normalization::runtime_callback_adapter_work(),
            (0, 0, 0),
            "a projection without lifted callbacks must not invoke the runtime adapter"
        );
        assert_eq!(materialized.sources.len(), 3);
        assert!(
            materialized
                .sources
                .windows(2)
                .all(|pair| pair[0].source.span().start < pair[1].source.span().start)
        );
        assert_eq!(
            pair_wrapper_finalizations(),
            2,
            "each retained canonical __pair__ wrapper must close exactly once"
        );
        root.close_root_child(call, Vec::new(), &tcx)
            .expect("close the projection owner with its source publications");
        root.close_root_with(Vec::new(), &mut tcx, |_, _| Ok(()))
            .expect("publish the captured canonical pair types");
        let mut pending = vec![source];
        let mut source_count = 0;
        while let Some(source) = pending.pop() {
            source_count += 1;
            assert_eq!(
                tcx.elaborations
                    .rec_quote_source_type(source.site().id)
                    .is_some(),
                capture,
                "capture selection must preserve wrapper and leaf occurrences"
            );
            if let Expr::Call { args, .. } = source {
                pending.extend(args.iter().filter_map(|arg| match arg {
                    CallArg::Value(value) => Some(value),
                    CallArg::Type(_) => None,
                }));
            }
        }
        assert_eq!(source_count, 5, "two wrappers and three leaves");
    }

    #[test]
    fn producer_advancement_is_mode_major_even_when_the_first_producer_is_pending() {
        use super::super::frontier::FrontierMode;

        let modes = [
            FrontierMode::ExpectedOnly,
            FrontierMode::LexicalFallback,
            FrontierMode::FinalPreflight,
            FrontierMode::Final,
        ];
        assert_eq!(modes.map(FrontierMode::rank), [0, 1, 3, 4]);
        for adjacent in modes.windows(2) {
            assert_eq!(adjacent[0].successor(false), Some(adjacent[1]));
        }
        assert_eq!(FrontierMode::Final.successor(false), None);
        let mut visits = Vec::new();
        for mode in modes {
            let complete = advance_mode_major_sweep(&[0, 1, 2], |producer| {
                visits.push((mode.rank(), producer));
                Ok(producer != 0 || mode == FrontierMode::Final)
            })
            .expect("mode-major producer sweep");
            assert_eq!(complete, mode == FrontierMode::Final);
        }
        assert_eq!(
            visits,
            vec![
                (0, 0),
                (0, 1),
                (0, 2),
                (1, 0),
                (1, 1),
                (1, 2),
                (3, 0),
                (3, 1),
                (3, 2),
                (4, 0),
                (4, 1),
                (4, 2),
            ],
            "a pending earlier producer must not block later siblings in the same mode"
        );
    }

    #[test]
    fn relation_producer_schedule_includes_one_distinct_probe_sweep() {
        use super::super::frontier::FrontierMode;

        let modes = [
            FrontierMode::ExpectedOnly,
            FrontierMode::LexicalFallback,
            FrontierMode::RelationProbe,
            FrontierMode::FinalPreflight,
            FrontierMode::Final,
        ];
        assert_eq!(modes.map(FrontierMode::rank), [0, 1, 2, 3, 4]);
        for adjacent in modes.windows(2) {
            assert_eq!(adjacent[0].successor(true), Some(adjacent[1]));
        }
        assert_eq!(FrontierMode::Final.successor(true), None);
        assert_eq!(
            FrontierMode::LexicalFallback.successor(false),
            Some(FrontierMode::FinalPreflight)
        );
    }

    #[test]
    fn relation_producer_partition_is_dense_ordered_and_duplicate_free() {
        let leaves = (0..64).map(ProducerSet::leaf).collect::<Vec<_>>();
        let union = |remainder| {
            leaves
                .iter()
                .enumerate()
                .filter(|(producer, _)| producer % 4 == remainder)
                .fold(ProducerSet::default(), |union, producer| {
                    union.union(producer.1)
                })
        };
        let even = union(0).union(&union(2));
        let repeated = even.union(&even).union(&leaves[0]);
        let (relation_producers, independent_producers) =
            freeze_relation_producers([&repeated, &even], 64);
        assert_eq!(relation_producers, (0..64).step_by(2).collect::<Vec<_>>());
        assert_eq!(
            independent_producers,
            (0..64)
                .filter(|producer| producer % 2 == 1)
                .collect::<Vec<_>>()
        );
    }
}
