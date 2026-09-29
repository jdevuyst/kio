//! Standalone Kio' checker shared by the full-Kio and kio-prime pipelines.
//!
//! Walks `Package<Prime>` and validates it under Kio's type system.
//! Surface-form-bearing expression variants are uninhabited at `Prime`, so this
//! slice runs no surface `substitute` pass (the full pipeline does).
//! Literal typing reduces to a single premise per
//! `specs/formal/prime.md` § 2.2: each literal carries its
//! `(Type)` host-type annotation directly on the AST (the Kio'
//! grammar requires it — see `specs/grammar.md` § Kio' grammar),
//! and the typer just checks the annotation matches the literal's lexical
//! shape via [`check_literal_annotation`]. Literal typing therefore records
//! nothing; the bake step below handles call/lambda completion and intrinsic values.
//!
//! The rewrites the Prime typer still performs are **call-arg
//! canonicalization**, checked-lambda parameter annotation, and typed function
//! materialization of intrinsic values,
//! recorded in a [`PrimeElaborations`] side-channel and stamped back into the AST by
//! [`bake_prime_elaborations`]. Per `specs/grammar.md`
//! (`CallArg ::= Expr | Type`), type-args and value-args share one
//! comma-separated list at the surface. Identifier casing fixes a
//! bare path's role before name lookup; the callee's signature does
//! not reinterpret a lowercase value path as a type. The typer
//! validates parsed type arguments and any structurally value-shaped
//! type candidates, then records the resolution
//! (`record_type_resolution` + `mark_value_at_type_slot`). Newtype
//! constructors additionally
//! infer their existential-witness suffix under Kio's introduction
//! rule. The bake pass rewrites each affected `Expr::Call`'s args
//! list into fully-explicit canonical form (every type-arg slot
//! present as `CallArg::Type`, every value-arg as `CallArg::Value`)
//! so downstream passes — structural recovery,
//! `recover_to_low::lower`, and the per-backend emitters — see one
//! shape.
//!
//! The walker drives the shared scoped checker over each module of the package.
//! It delegates every recursion through
//! `synth_expr` / `check_value_against` to
//! [`Typer<Prime>`](crate::pass::typecheck_core::Typer), wired by
//! [`PrimeTyper`] in this module — its trait methods dispatch every
//! core arm to the same shared synth/check helpers the kio binary uses, while
//! surface-bearing extension variants discharge via `match *ext {}` because
//! they are uninhabited at Prime. Literal arms route through the
//! Prime-local helper [`synth_prime_literal`] rather than the
//! shared `synth_literal`, because Kio' admits no bare-literal form
//! that would need tier-2/3 resolution.
//!
//! ## Where the divergence lives
//!
//! Unlike the full pipeline's `typecheck_full::Elaborations`, this entry point's
//! side-channel ([`PrimeElaborations`]) records only ordinary-core completions: type-arg
//! and implicit-Unit slots on `Expr::Call`, checked `Expr::FnExpr`
//! parameter types, and checked intrinsic-value schemes. There is no Lowered → Prime
//! substitution because surface-form-bearing extension variants are
//! statically uninhabited at Prime. The bake pass materializes only the
//! Prime-local completions; the later canonicalizer normalizes the
//! statement spine. The kio binary's `check_package`, by contrast,
//! consumes its proof-bearing checked Lowered package into a fresh
//! `Package<Prime>`, then calls this standalone Prime typer to validate the
//! substituted artifact before a backend can consume it.

use crate::ast::Prime;
use crate::error::Error;
use crate::pass::alpha_normalize::{AlphaNormalizedPackage, normalize_package};
use crate::pass::resolve::{LocatedError, Package};
use crate::pass::typecheck_core::{
    InternedType, Synth, TypeCtx, Typer, TyperPhase, check_literal_annotation,
    check_package_modules_with_typecheck_scope_collect_errors_with_execution, synth_path,
};

/// `Prime`'s `Typer<Prime>` impl — wires the trait's two dispatch
/// methods to the shared synth/check helpers in
/// [`crate::pass::typecheck_core`]. Every surface-bearing extension variant is
/// uninhabited at Prime, so each corresponding match arm discharges via
/// `match *ext {}`.
/// Non-elaboration arms route to the same phase-polymorphic
/// helpers (`synth_path`, `synth_call`, `synth_fn`, `synth_let`,
/// `check_fn_against`, `synth_value_arg`, `require_type_equiv`) that the Lowered
/// typer uses, instantiated at `P = Prime`; literal arms use the Prime-local
/// helper described above.
pub struct PrimeTyper;

impl Typer<Prime> for PrimeTyper {
    const RECORD_NOMINAL_TYPE_BINDERS: bool = false;

    // Kio' is Church-style: every universal call type argument is
    // written explicitly. Newtype constructor existentials remain
    // inference-driven under their separate introduction rule.
    const ALLOW_ELIDED_UNIVERSAL_CALL_TYPE_ARGS: bool = false;

    // Kio' rejects the multi-layer flat-call form per
    // `specs/language.md` § 4c. Each application consumes at most
    // one `Function` layer (plus its preceding `Forall` binders);
    // the only spelling is the explicit curried form
    // `foo(T1, v1)(T2, v2)`. Prime declarations preserve that same
    // ordered interleaving of `Forall` and `Function` layers.
    const ALLOW_MULTI_LAYER_FLAT_CALL: bool = false;

    fn synth_call<'m>(
        callee: &'m crate::ast::Expr<Prime>,
        args: &'m [crate::ast::CallArg<Prime>],
        site: crate::ast::ExpressionSite,
        tcx: &mut TypeCtx<'m, '_, Prime>,
    ) -> Result<Synth<Prime>, Error> {
        crate::pass::typecheck_core::apply::synth_prime_call(callee, args, site, tcx)
    }

    fn check_call_against_interned<'m>(
        callee: &'m crate::ast::Expr<Prime>,
        args: &'m [crate::ast::CallArg<Prime>],
        expected: &crate::pass::typecheck_core::InternedType<Prime>,
        site: crate::ast::ExpressionSite,
        tcx: &mut TypeCtx<'m, '_, Prime>,
    ) -> Result<crate::pass::typecheck_core::InternedType<Prime>, Error> {
        crate::pass::typecheck_core::apply::check_prime_call_against_interned(
            callee, args, expected, site, tcx,
        )
    }

    fn synth_expr<'m>(
        e: &'m crate::ast::Expr<Prime>,
        tcx: &mut TypeCtx<'m, '_, Prime>,
    ) -> Result<Synth<Prime>, Error> {
        match e {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            crate::ast::Expr::Unit {
                occurrence: _,
                meta,
            } => Ok(Synth::value(crate::ast::Type::Unit {
                meta: crate::ast::Meta::new(meta.span),
            })),
            crate::ast::Expr::Path {
                occurrence: _,
                segments,
                meta,
                ext: _,
            } => {
                let synthesized = synth_path(segments, meta.span, tcx)?;
                record_intrinsic_value(e, &synthesized.ty, tcx);
                Ok(synthesized)
            }
            crate::ast::Expr::Let {
                name,
                name_span,
                ty,
                value,
                body,
                ..
            } => crate::pass::typecheck_core::annotation_plan::synth_prime_let(
                name,
                *name_span,
                ty.as_ref(),
                value,
                body,
                tcx,
            ),
            crate::ast::Expr::Seq { value, body, .. } => {
                crate::pass::typecheck_core::synth_seq(value, body, tcx)
            }
            crate::ast::Expr::FnExpr {
                sig, ret_ty, body, ..
            } => {
                #[cfg(test)]
                crate::pass::typecheck_core::annotation_plan::record_prime_synth_consumer();
                let plan = crate::pass::typecheck_core::annotation_plan::plan_source_lambda_header(
                    sig,
                    ret_ty.as_ref(),
                    tcx,
                )?;
                let inferred = vec![None; plan.value_slots.len()];
                crate::pass::typecheck_core::synth_closed_fn_with_source_plan(
                    e, plan, body, &inferred, tcx,
                )
            }
            crate::ast::Expr::Call {
                callee,
                args,
                meta: _,
                ..
            } => <Self as Typer<Prime>>::synth_call(callee, args, e.site(), tcx),
            crate::ast::Expr::StrLit { annotation, .. } => {
                synth_prime_literal(crate::ast::RoleShape::Str, annotation, tcx).map(Synth::value)
            }
            crate::ast::Expr::IntLit { annotation, .. } => {
                synth_prime_literal(crate::ast::RoleShape::Int, annotation, tcx).map(Synth::value)
            }
            crate::ast::Expr::FloatLit { annotation, .. } => {
                synth_prime_literal(crate::ast::RoleShape::Float, annotation, tcx).map(Synth::value)
            }
            crate::ast::Expr::BoolLit { annotation, .. } => {
                synth_prime_literal(crate::ast::RoleShape::Bool, annotation, tcx).map(Synth::value)
            }
            // Surface-only variants — uninhabited at Prime per the
            // ResolvePhase bound.
            crate::ast::Expr::Tuple { ext, .. } => match *ext {},
            crate::ast::Expr::LabelValue { ext, .. } => match *ext {},
            crate::ast::Expr::RowLet { ext, .. } => match *ext {},
            crate::ast::Expr::FnPlaceholder { ext, .. } => match *ext {},
            crate::ast::Expr::OpChain { ext, .. } => match *ext {},
            crate::ast::Expr::RecCall { ext, .. } => match *ext {},
            // Elaboration-bearing variants — uninhabited at Prime
            // per the Phase shape (Lowered → Prime substitution
            // rewrote them out).
            crate::ast::Expr::Elaborator { ext, .. } => match *ext {},
            crate::ast::Expr::RecOrder { ext, .. } | crate::ast::Expr::RecQuote { ext, .. } => {
                match *ext {}
            }
            crate::ast::Expr::UserElaborator { ext, .. } => match *ext {},
            crate::ast::Expr::Ufcs { ext, .. } => match *ext {},
            // Statically uninhabited at `Prime` — the enriched
            // structural variants are produced *after* the typer, by
            // the structural-recovery pass at the `Enriched` phase.
            crate::ast::Expr::EnrichedTuple { ext, .. }
            | crate::ast::Expr::EnrichedProject { ext, .. }
            | crate::ast::Expr::EnrichedInject { ext, .. }
            | crate::ast::Expr::EnrichedMatch { ext, .. }
            | crate::ast::Expr::EnrichedConditional { ext, .. }
            | crate::ast::Expr::EnrichedRecord { ext, .. }
            | crate::ast::Expr::EnrichedFieldGet { ext, .. } => match *ext {},
            crate::ast::Expr::LowHostCall { ext, .. }
            | crate::ast::Expr::LowModuleCall { ext, .. }
            | crate::ast::Expr::LowQualifiedModuleCall { ext, .. }
            | crate::ast::Expr::LowQualifiedNewtypeMember { ext, .. }
            | crate::ast::Expr::LowNewtypeCtor { ext, .. }
            | crate::ast::Expr::LowNewtypeProj { ext, .. }
            | crate::ast::Expr::LowClosureCall { ext, .. }
            | crate::ast::Expr::LowIndirectCall { ext, .. }
            | crate::ast::Expr::LowTypeApplication { ext, .. }
            | crate::ast::Expr::LowAbsurdCall { ext, .. }
            | crate::ast::Expr::LowCpsProjectorApply { ext, .. }
            | crate::ast::Expr::LowBoundRef { ext, .. }
            | crate::ast::Expr::LowHostFnValueRef { ext, .. }
            | crate::ast::Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
        }
    }

    fn check_value_against_interned<'m>(
        e: &'m crate::ast::Expr<Prime>,
        expected: &crate::pass::typecheck_core::InternedType<Prime>,
        tcx: &mut TypeCtx<'m, '_, Prime>,
    ) -> Result<(), Error> {
        if let crate::ast::Expr::FnExpr {
            sig,
            ret_ty,
            body,
            meta: _,
            ..
        } = e
        {
            return crate::pass::typecheck_core::check_prime_fn_against(
                sig,
                ret_ty.as_ref(),
                body,
                expected,
                e.site(),
                tcx,
            );
        }
        if matches!(e, crate::ast::Expr::Let { .. }) {
            return crate::pass::typecheck_core::annotation_plan::check_prime_let_against(
                e, expected, tcx,
            );
        }
        // Surface-bearing extension variants are uninhabited at Prime, so the
        // shared bidirectional function/path/literal handling and
        // synth-and-equiv fallback need no phase-specific pre-handling.
        crate::pass::typecheck_core::default_check_value_against(e, expected, tcx)
    }

    fn check_user_elaborator_item<'m>(
        _elaborator: &'m crate::ast::UserElaboratorDef<Prime>,
        _item_index: usize,
        _env: &crate::pass::typecheck_core::ModuleEnv<'m, Prime>,
        _elaborations: &mut <Prime as TyperPhase>::Elaborations,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn record_type_resolution<'m>(
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
        resolved: crate::pass::typecheck_core::InternedType<Prime>,
        tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // The grammar (`specs/grammar.md`, `CallArg ::= Expr | Type`)
        // does not distinguish type-args from value-args at parse
        // time; type-args land here when (a) the typer reclassified
        // a `CallArg::Value` at a type-arg slot via `expr_to_type_arg`
        // (see also `mark_value_at_type_slot` below) or (b) the
        // typer solved an inference-driven existential-witness slot
        // on a newtype constructor. Either way, the bake pass needs
        // the resolved type to rewrite the slot into canonical
        // `CallArg::Type` form.
        let mp = tcx.env.module_path.clone();
        tcx.elaborations
            .record_type_resolution(&mp, call_site.id, slot_index, resolved);
    }

    fn mark_value_at_type_slot<'m>(
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
        tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Per `specs/grammar.md`'s call-arg production
        // (`CallArg ::= Expr | Type`), type-args and value-args
        // share one comma-separated list. Identifier casing fixes a
        // bare path's role before name lookup; the callee's signature
        // never reclassifies a lowercase value path. This hook records
        // a structurally value-shaped argument accepted as a type by
        // `expr_to_type_arg`, so the bake pass knows the user *consumed*
        // a slot at this position
        // (vs. an omitted existential witness inferred for a
        // newtype constructor, which inserts a fresh slot without
        // consuming a user arg).
        let mp = tcx.env.module_path.clone();
        tcx.elaborations
            .mark_value_at_type_slot(&mp, call_site.id, slot_index);
    }

    fn record_call_split<'m>(
        _call_site: crate::ast::ExpressionSite,
        _split_after_slot: usize,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Kio' does not admit Full Kio's multi-layer flat-call surface
        // rule, so Prime never needs a post-typer call-shape rewrite.
    }

    fn record_call_implicit_unit<'m>(
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
        tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        let module_path = tcx.env.module_path.clone();
        tcx.elaborations
            .record_call_implicit_unit(&module_path, call_site.id, slot_index);
    }

    fn record_regular_ufcs_elaboration<'m>(
        original: &'m crate::ast::Expr<Prime>,
        _normalized_call: crate::ast::Expr<Prime>,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        let crate::ast::Expr::Ufcs { ext, .. } = original else {
            unreachable!("regular UFCS recording was requested for a non-UFCS expression");
        };
        match *ext {}
    }

    fn record_literal_resolution<'m>(
        _literal_site: crate::ast::ExpressionSite,
        _resolved: crate::pass::typecheck_core::InternedType<Prime>,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Kio' mandates the `(Type)` annotation on every literal (see
        // `specs/grammar.md` § Kio' grammar and `specs/prime.md`) —
        // Prime synthesis reads the type off the AST directly via
        // [`synth_prime_literal`] and bypasses the shared `synth_literal`
        // helper. Generic checking can still invoke this publication hook, but
        // the mandatory annotation is already present, so Prime records
        // nothing. Keeping this as an infallible no-op leaves trait dispatch
        // total without a phase-conditional path.
    }

    fn record_fn_param_type<'m>(
        fn_site: crate::ast::ExpressionSite,
        value_param_index: usize,
        resolved: crate::pass::typecheck_core::InternedType<Prime>,
        tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        let mp = tcx.env.module_path.clone();
        tcx.elaborations
            .record_fn_param_type(&mp, fn_site.id, value_param_index, resolved);
    }

    fn record_binder_at<'m>(
        _segment_span: crate::span::Span,
        _kind: crate::pass::typecheck_core::ResolvedBinderKind,
        _name: &str,
        _parent: Option<&str>,
        _qualifier: Option<&str>,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Position-keyed index is Full-only: the `kio-prime` binary
        // processes Kio' input that has no surface forms for an
        // editor to query against, so there's no consumer for a
        // position index here. Implementing the trait method (rather
        // than punting to `unreachable!`) keeps the trait dispatch
        // infallible and makes the shared `synth_path` call site a
        // no-op at Prime without conditional code.
    }

    fn record_nominal_type_binder_at<'m>(
        _segment_span: crate::span::Span,
        _kind: crate::pass::typecheck_core::NominalTypeBinderKind,
        _declaring_module_path: &str,
        _name: &str,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Kio' has no editor-facing position index; see `record_binder_at`.
    }

    fn record_newtype_member_binder_at<'m>(
        _segment_span: crate::span::Span,
        _declaring_module_path: &str,
        _newtype: &str,
        _member: &str,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Kio' has no editor-facing position index; see `record_binder_at`.
    }

    fn record_binder_decl_at<'m>(
        _decl_span: crate::span::Span,
        _kind: crate::pass::typecheck_core::ResolvedBinderKind,
        _name: &str,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Same rationale as `record_binder_at`: Prime has no
        // position-index consumer, so the binder-introduction sites'
        // declaration recording no-ops here.
    }

    fn record_position_type<'m>(
        _expr_span: crate::span::Span,
        _resolved: InternedType<Prime>,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Same rationale as `record_binder_at`: Prime has no
        // position-index consumer; the trait method exists so the
        // shared `default_check_value_against` Let/Seq arms compile
        // for both phases. No-op here.
    }

    fn record_inlay_let_type<'m>(
        _name_span: crate::span::Span,
        _name: &str,
        _resolved: crate::ast::Type<Prime>,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Same rationale as `record_position_type`: Prime has no LSP
        // position-index consumer.
    }

    fn record_inlay_type_args<'m>(
        _call_site: crate::ast::ExpressionSite,
        _callee_span: crate::span::Span,
        _slot_offset: usize,
        _resolved: Vec<crate::ast::Type<Prime>>,
        _tcx: &mut TypeCtx<'m, '_, Prime>,
    ) {
        // Same rationale as `record_position_type`: Prime has no LSP
        // position-index consumer.
    }

    fn discharge_module_deferrals(
        _env: &crate::pass::typecheck_core::ModuleEnv<'_, Prime>,
        _elaborations: &mut <Prime as TyperPhase>::Elaborations,
    ) -> Result<(), crate::error::Error> {
        // Kio' carries no surface elaborator forms (every elaboration-
        // bearing variant is uninhabited at Prime), so there is no
        // deferred-elaboration queue to drain. The trait method exists
        // so the shared per-module checker is phase-polymorphic;
        // no-op here.
        Ok(())
    }
}

/// Type a Kio' literal expression. The `(Type)` annotation is
/// mandatory per the Kio' grammar (`specs/grammar.md` § Kio'
/// grammar — `LiteralCall ::= LITERAL '(' Type ')'`); it is
/// non-optional on the AST at `Prime` ([`crate::ast::Phase::LitAnnotation`]),
/// so a bare Kio' literal is statically unrepresentable and this
/// helper takes the annotation type directly. The annotation must
/// itself be a single role-bearing host type whose role the literal's
/// shape admits per the admission relation in `specs/language.md`
/// § Literals; [`check_literal_annotation`] discharges that check.
///
/// This is the Prime counterpart of `synth_literal` in
/// `typecheck_core::synth`, which carries the full surface's three-
/// tier resolution logic. Kio' has only tier 1 (the annotation), so
/// the Prime path is one-premise and side-table-free —
/// matching `specs/formal/prime.md` § 2.2's T-True / T-False /
/// T-Int / T-Float / T-Str rules.
fn synth_prime_literal<'m>(
    shape: crate::ast::RoleShape,
    annotation: &crate::ast::Type<Prime>,
    tcx: &mut TypeCtx<'m, '_, Prime>,
) -> Result<crate::ast::Type<Prime>, Error> {
    check_literal_annotation(shape, annotation, tcx.env)
}

/// The prime pipeline's typer side-channel — the `Prime`-phase
/// counterpart of `typecheck_full::Elaborations`. Surface-form-bearing
/// extension variants and `Type::Infer` are all uninhabited at Prime,
/// and Kio' literals carry their `(Type)` annotation directly on the
/// AST (see `specs/grammar.md` § Kio' grammar — `LiteralCall`'s
/// annotation slot is mandatory), so the only recording paths whose
/// targets are inhabited at this phase are `Expr::Call` type-arg and
/// implicit-Unit slots, checked `Expr::FnExpr` value-parameter types, and
/// intrinsic-value schemes materialized as ordinary typed functions.
/// [`bake_prime_elaborations`] drains the table into the AST after
/// type-checking so downstream passes (structural recovery,
/// `recover_to_low::lower`, the per-backend emitters) read the
/// canonical shape directly off the AST.
#[derive(Debug, Default)]
pub struct PrimeElaborations {
    /// Checked value occurrences only. Saturated direct calls need no wrapper.
    /// Semantic occurrence IDs are globally fresh across the checked package.
    #[allow(clippy::box_collection)] // Keep the ordinary no-inventory layout to one pointer.
    intrinsic_values: Option<
        Box<
            std::collections::HashMap<
                crate::ast::ExpressionOccurrenceId,
                crate::pass::typecheck_core::RecordedTypeResolution<Prime>,
            >,
        >,
    >,
    /// The typer's resolved type at each call's type-arg slot,
    /// keyed by `(module_path, call_id, slot_index)`. Populated
    /// by `apply_polymorphic_function` — every binder it solves at
    /// canonical slot N records a `(call_id, N) -> resolved`
    /// entry, regardless of whether the user wrote a value-shaped
    /// expression at the slot or omitted an inference-driven
    /// constructor existential witness. The bake pass consults this
    /// together with `value_at_type_slot` to rebuild the call's args
    /// list in fully-explicit canonical form.
    type_resolutions: std::collections::HashMap<
        (String, crate::ast::ExpressionOccurrenceId, usize),
        crate::pass::typecheck_core::RecordedTypeResolution<Prime>,
    >,
    /// `(module_path, call_id, slot_index)` triples where the
    /// user supplied a structurally value-shaped expression that
    /// `expr_to_type_arg` accepted at a type-arg slot. Bare paths are
    /// already classified by identifier casing before name lookup;
    /// this marker does not let a callee signature reinterpret a
    /// lowercase value path. The bake pass uses it to distinguish
    /// "user wrote a value here, reclassify it" (replace the user arg
    /// with the recorded type and advance the user index) from "constructor
    /// existential witness was omitted" (insert the recorded type
    /// as a fresh `CallArg::Type` without advancing the user index).
    /// Mirrors `typecheck_full::Elaborations::value_at_type_slot`.
    value_at_type_slot:
        std::collections::HashSet<(String, crate::ast::ExpressionOccurrenceId, usize)>,
    /// Canonical call slots supplied by the Unit value of a syntactically
    /// empty source call. A nonempty type-only packet never records one.
    implicit_unit_slots:
        std::collections::HashSet<(String, crate::ast::ExpressionOccurrenceId, usize)>,
    fn_param_types: std::collections::HashMap<
        (String, crate::ast::ExpressionOccurrenceId, usize),
        crate::pass::typecheck_core::RecordedTypeResolution<Prime>,
    >,
}

impl PrimeElaborations {
    /// Record the typer's solved type at canonical slot
    /// `slot_index` of the call occurrence `call_id`.
    pub fn record_type_resolution(
        &mut self,
        module_path: &str,
        call_id: crate::ast::ExpressionOccurrenceId,
        slot_index: usize,
        resolved: crate::pass::typecheck_core::InternedType<Prime>,
    ) {
        self.type_resolutions.insert(
            (module_path.to_owned(), call_id, slot_index),
            resolved.into(),
        );
    }

    /// The recorded type at the call's slot `slot_index`, if any.
    pub(crate) fn type_resolution_for(
        &self,
        module_path: &str,
        call_id: crate::ast::ExpressionOccurrenceId,
        slot_index: usize,
    ) -> Option<&crate::pass::typecheck_core::RecordedTypeResolution<Prime>> {
        self.type_resolutions
            .get(&(module_path.to_owned(), call_id, slot_index))
    }

    /// Mark the `(module_path, call_id, slot_index)` triple as a
    /// value-shaped expression supplied at a type-arg slot.
    pub fn mark_value_at_type_slot(
        &mut self,
        module_path: &str,
        call_id: crate::ast::ExpressionOccurrenceId,
        slot_index: usize,
    ) {
        self.value_at_type_slot
            .insert((module_path.to_owned(), call_id, slot_index));
    }

    /// True when the call at `call_id`'s slot `slot_index` inside
    /// module `module_path` carries a value-shaped expression the
    /// typer reclassified as a type.
    pub fn is_value_at_type_slot(
        &self,
        module_path: &str,
        call_id: crate::ast::ExpressionOccurrenceId,
        slot_index: usize,
    ) -> bool {
        self.value_at_type_slot
            .contains(&(module_path.to_owned(), call_id, slot_index))
    }

    pub fn record_call_implicit_unit(
        &mut self,
        module_path: &str,
        call_id: crate::ast::ExpressionOccurrenceId,
        slot_index: usize,
    ) {
        self.implicit_unit_slots
            .insert((module_path.to_owned(), call_id, slot_index));
    }

    pub fn is_call_implicit_unit_slot(
        &self,
        module_path: &str,
        call_id: crate::ast::ExpressionOccurrenceId,
        slot_index: usize,
    ) -> bool {
        self.implicit_unit_slots
            .contains(&(module_path.to_owned(), call_id, slot_index))
    }

    pub fn record_fn_param_type(
        &mut self,
        module_path: &str,
        fn_id: crate::ast::ExpressionOccurrenceId,
        value_param_index: usize,
        resolved: crate::pass::typecheck_core::InternedType<Prime>,
    ) {
        self.fn_param_types.insert(
            (module_path.to_owned(), fn_id, value_param_index),
            resolved.into(),
        );
    }

    pub(crate) fn fn_param_type_for(
        &self,
        module_path: &str,
        fn_id: crate::ast::ExpressionOccurrenceId,
        value_param_index: usize,
    ) -> Option<&crate::pass::typecheck_core::RecordedTypeResolution<Prime>> {
        self.fn_param_types
            .get(&(module_path.to_owned(), fn_id, value_param_index))
    }

    pub fn merge(&mut self, other: PrimeElaborations) {
        if let Some(values) = other.intrinsic_values {
            self.intrinsic_values
                .get_or_insert_with(Default::default)
                .extend(*values);
        }
        for (key, value) in other.type_resolutions {
            self.type_resolutions.insert(key, value);
        }
        for key in other.value_at_type_slot {
            self.value_at_type_slot.insert(key);
        }
        for key in other.implicit_unit_slots {
            self.implicit_unit_slots.insert(key);
        }
        for (key, value) in other.fn_param_types {
            self.fn_param_types.insert(key, value);
        }
    }
}

pub(crate) fn record_intrinsic_value(
    expression: &crate::ast::Expr<Prime>,
    ty: &InternedType<Prime>,
    tcx: &mut TypeCtx<'_, '_, Prime>,
) {
    let crate::ast::Expr::Path { segments, .. } = expression else {
        return;
    };
    if segments.len() == 1
        && crate::pass::resolve::PRIME_INTRINSICS.contains(&segments[0].name.as_str())
    {
        tcx.elaborations
            .intrinsic_values
            .get_or_insert_with(Default::default)
            .insert(expression.site().id, ty.clone().into());
    }
}

impl TyperPhase for Prime {
    // `Prime` admits no surface-form-bearing extension variants and no
    // `Type::Infer` — all are uninhabited at this phase, and Kio' literals
    // carry their `(Type)` annotation
    // directly on the AST (see `specs/grammar.md` § Kio' grammar —
    // `LiteralCall`'s annotation slot is mandatory). The inhabited
    // recording paths are `Expr::Call` type-arg and implicit-Unit slots plus
    // checked `Expr::FnExpr` value-parameter types and intrinsic-value schemes.
    // See the struct doc for the
    // full picture.
    type Elaborations = PrimeElaborations;
    type Typer = PrimeTyper;

    fn merge_elaborations(target: &mut Self::Elaborations, source: Self::Elaborations) {
        target.merge(source);
    }
}

/// Walk every module of a fully-resolved `Package<Prime>` and
/// validate it under Kio's type system. Returns a `Package<Prime>`
/// whose `Expr::Call` args lists are in fully-explicit canonical
/// form (every type-arg slot present as `CallArg::Type`) and whose
/// checked lambda parameters carry their resolved type annotations. Intrinsic
/// values become ordinary fully typed forwarding functions.
/// Literal annotations are read off the AST directly per the Kio'
/// grammar (see `specs/prime.md`). The typer records call and checked-lambda
/// completion data and intrinsic-value schemes in a [`PrimeElaborations`] table, and
/// [`bake_prime_elaborations`] stamps it into the AST before the separate
/// statement-spine canonicalizer runs.
///
/// Implementation: delegate to the shared scoped checker for each module
/// (declaration-level + expression-level via the [`Typer<Prime>`]
/// dispatch — `PrimeTyper::synth_expr` and
/// `PrimeTyper::check_value_against` discharge the elaboration arms
/// `match *ext {}` and route the rest to the same helpers the
/// Lowered typer uses), then walk the package file's export block
/// the same way `typecheck_full` does.
pub fn check_package(package: &Package<Prime>) -> Result<Package<Prime>, LocatedError> {
    check_normalized_package(normalize_package(package))
}

/// Validate a package whose lexical type binders have already been
/// alpha-normalized.  The full pipeline uses this entry after Lowered →
/// Prime substitution so it does not normalize the same tree a second time.
pub(crate) fn check_normalized_package(
    normalized: AlphaNormalizedPackage<Prime>,
) -> Result<Package<Prime>, LocatedError> {
    let mut elaborations = PrimeElaborations::default();
    let type_interner = std::sync::Arc::new(crate::pass::typecheck_core::TypeInterner::default());
    let typecheck_scope =
        crate::pass::typecheck_core::PackageTypecheckScope::fresh_for_normalized_package(
            &normalized,
            type_interner,
        );
    check_package_modules_with_typecheck_scope_collect_errors_with_execution(
        normalized.package(),
        &mut elaborations,
        typecheck_scope,
        crate::pass::typecheck_core::TypecheckExecution::AllowParallel,
    )
    .map_err(|errors| {
        errors
            .into_iter()
            .next()
            .expect("collected Prime package errors are non-empty")
    })?;
    let (package, _) = normalized.into_parts();
    let mut typed = package;
    bake_prime_elaborations(&mut typed, &elaborations);
    crate::prime::canonical::canonicalize_package(&mut typed);
    crate::pass::alpha_normalize::erase_package_occurrences(&mut typed);
    Ok(typed)
}

/// Stamp every recorded Prime completion into the AST: rebuild each affected
/// `Expr::Call` argument list in fully explicit canonical form, annotate
/// checked lambda value parameters, and materialize intrinsic values.
/// This is the prime pipeline's counterpart of the full pipeline's
/// `substitute` pass — narrower because Prime has no surface-form
/// elaboration sites to rewrite, but the call-arg canonicalization
/// step is identical to `substitute::visit_expr_call`. Walking only
/// `FnDef` / `ExportFn` bodies is exhaustive: those are the sole
/// expression-bearing positions in a Prime package.
/// Bake every recorded elaboration into one module's `FnDef` bodies,
/// keyed under `module_path`. Used by the workspace driver's
/// auxiliary import-body typecheck, which types a synthetic carrier
/// module on its own (outside `check_package`'s per-module loop).
#[cfg(feature = "prime")]
pub(crate) fn bake_prime_elaborations_in_module(
    module: &mut crate::ast::Module<Prime>,
    elabs: &PrimeElaborations,
    module_path: &str,
) {
    bake_module(module, elabs, module_path);
}

fn bake_prime_elaborations(package: &mut Package<Prime>, elabs: &PrimeElaborations) {
    // The package file carries only the phase-independent `bridge` glob
    // list — it has no item bodies — so there is nothing to bake there;
    // elaborations live entirely in module item bodies.
    for entry in package.modules_mut() {
        let mp = entry
            .module
            .path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("/");
        bake_module(&mut entry.module, elabs, &mp);
        // Baking may inject qualified imports and prepend generated aliases.
        // Refresh the package entry's derived resolution index once after the
        // mutation so downstream Prime consumers cannot observe stale import
        // edges or shifted TopLevelIds.
        entry.scope =
            crate::pass::resolve::TopLevelScope::build(&entry.module).unwrap_or_else(|error| {
                unreachable!(
                    "Prime elaboration baking produced an invalid top-level scope: {error:?}"
                )
            });
    }
}

fn bake_module(
    module: &mut crate::ast::Module<Prime>,
    elabs: &PrimeElaborations,
    module_path: &str,
) {
    let mut requalifier = crate::pass::typecheck_core::PrimeTypeRequalifier::new(module);
    let mut type_binders = Vec::new();
    for item in &mut module.items {
        if let crate::ast::Item::FnDef(d) = item {
            let mark = type_binders.len();
            type_binders.extend(d.sig.params.iter().filter_map(|param| {
                let crate::ast::SignatureParam::Type(param) = param else {
                    return None;
                };
                Some(param.name.clone())
            }));
            bake_expr(
                &mut d.body,
                elabs,
                module_path,
                &mut requalifier,
                &mut type_binders,
            );
            type_binders.truncate(mark);
        }
    }
    module.imports.extend(requalifier.take_imports());
    let aliases = prime_local_alias_items(requalifier.take_local_aliases());
    module.items.splice(0..0, aliases);
}

fn prime_local_alias_items(
    aliases: Vec<crate::pass::typecheck_core::PrimeLocalTypeAlias>,
) -> Vec<crate::ast::Item<Prime>> {
    aliases
        .into_iter()
        .map(|plan| {
            let args = plan
                .params
                .iter()
                .map(|param| {
                    crate::ast::Type::synth_path(vec![param.name.clone()], Vec::new(), param.span)
                })
                .collect();
            crate::ast::Item::TypeAlias(crate::ast::TypeAlias {
                vis: crate::ast::Visibility::Private,
                name: plan.alias,
                name_span: plan.span,
                type_params: plan.params,
                body: crate::ast::Type::synth_path(vec![plan.nominal], args, plan.span),
                meta: crate::ast::Meta::new(plan.span),
                editable_span: None,
                doc: None,
            })
        })
        .collect()
}

/// Recurse through a Prime expression, rebuilding each call's args
/// list into fully-explicit canonical form. Literal annotations are
/// already on the AST per the Kio' grammar (`LiteralCall`'s
/// annotation slot is mandatory — see `specs/grammar.md` § Kio'
/// grammar). Surface-only and elaboration-bearing variants are
/// uninhabited at Prime and discharged via `match *ext {}`.
fn bake_expr(
    e: &mut crate::ast::Expr<Prime>,
    elabs: &PrimeElaborations,
    module_path: &str,
    requalifier: &mut crate::pass::typecheck_core::PrimeTypeRequalifier,
    type_binders: &mut Vec<String>,
) {
    use crate::ast::{CallArg, Expr};
    let site = e.site();
    if let Some(recorded) = elabs
        .intrinsic_values
        .as_ref()
        .and_then(|values| values.get(&site.id))
    {
        let scheme = prime_recorded_type(recorded, requalifier, type_binders);
        *e = intrinsic_value_wrapper(e.clone(), scheme, type_binders);
        return;
    }
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        // Literals already carry their annotation per the Kio' grammar;
        // no rewrite needed at this phase.
        Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::Call {
            callee, args, meta, ..
        } => {
            bake_expr(callee, elabs, module_path, requalifier, type_binders);
            // Recurse into the value-side sub-expressions before
            // canonicalizing. The canonicalization step below may
            // replace some `CallArg::Value` slots with `CallArg::Type`
            // (when the user wrote a value-shaped expression at a
            // type-arg slot), but the value-side sub-expressions that
            // survive still need their walk.
            for a in args.iter_mut() {
                if let CallArg::Value(v) = a {
                    bake_expr(v, elabs, module_path, requalifier, type_binders);
                }
            }
            // Canonicalize the args list using the typer's recorded
            // type-arg resolutions. Mirrors
            // `substitute::visit_expr_call` — see that function for
            // the per-slot decision table. The walk is unconditional
            // because a call may have a recording at a non-leading
            // slot (e.g., `foo(Bool, x, y)` where `foo` has two
            // type-params and `x` is value-shaped at slot 1); the
            // function clones-and-passes-through when no slot has
            // a recording.
            let call_span = meta.span;
            let new_args =
                canonicalize_call_args(args, elabs, module_path, site, requalifier, type_binders);
            *args = new_args;
            // Kio' shares Kio's empty-call meaning. Once standalone checking
            // accepts a monomorphic source-empty call, bake its Unit value
            // into the self-contained Prime artifact. Polymorphic calls use
            // the canonical-slot marker inside `canonicalize_call_args`.
            if args.is_empty() {
                args.push(CallArg::Value(Expr::Unit {
                    occurrence: Default::default(),
                    meta: crate::ast::Meta::new(call_span),
                }));
            }
        }
        Expr::FnExpr {
            sig, body, meta: _, ..
        } => {
            let mark = type_binders.len();
            type_binders.extend(sig.params.iter().filter_map(|param| {
                let crate::ast::SignatureParam::Type(param) = param else {
                    return None;
                };
                Some(param.name.clone())
            }));
            bake_fn_signature(sig, elabs, module_path, site.id, requalifier, type_binders);
            bake_expr(body, elabs, module_path, requalifier, type_binders);
            type_binders.truncate(mark);
        }
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            bake_expr(value, elabs, module_path, requalifier, type_binders);
            bake_expr(body, elabs, module_path, requalifier, type_binders);
        }
        Expr::Path { .. } | Expr::Unit { .. } => {}
        // Surface-only variants — uninhabited at Prime.
        Expr::Tuple { ext, .. } => match *ext {},
        Expr::LabelValue { ext, .. } => match *ext {},
        Expr::RowLet { ext, .. } => match *ext {},
        Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        // Elaboration-bearing variants — uninhabited at Prime.
        Expr::Elaborator { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { ext, .. } => match *ext {},
        Expr::Ufcs { ext, .. } => match *ext {},
        // Enriched structural variants only exist post-typecheck.
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. } => match *ext {},
        crate::ast::Expr::LowHostCall { ext, .. }
        | crate::ast::Expr::LowModuleCall { ext, .. }
        | crate::ast::Expr::LowQualifiedModuleCall { ext, .. }
        | crate::ast::Expr::LowQualifiedNewtypeMember { ext, .. }
        | crate::ast::Expr::LowNewtypeCtor { ext, .. }
        | crate::ast::Expr::LowNewtypeProj { ext, .. }
        | crate::ast::Expr::LowClosureCall { ext, .. }
        | crate::ast::Expr::LowIndirectCall { ext, .. }
        | crate::ast::Expr::LowTypeApplication { ext, .. }
        | crate::ast::Expr::LowAbsurdCall { ext, .. }
        | crate::ast::Expr::LowCpsProjectorApply { ext, .. }
        | crate::ast::Expr::LowBoundRef { ext, .. }
        | crate::ast::Expr::LowHostFnValueRef { ext, .. }
        | crate::ast::Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
    }
}

/// Materialize only the already-checked scheme. In particular, the Bool role
/// remains the identity selected in the source's lexical scope.
fn intrinsic_value_wrapper(
    intrinsic: crate::ast::Expr<Prime>,
    mut scheme: crate::ast::Type<Prime>,
    ambient_binders: &[String],
) -> crate::ast::Expr<Prime> {
    use crate::ast::{CallArg, Expr, Meta, Param, Signature, SignatureGroup, Type};
    let span = intrinsic.span();
    let mut occupied = ambient_binders.iter().cloned().collect();
    crate::pass::typecheck_core::collect_free_type_vars(&scheme, &mut occupied);
    let mut groups = Vec::new();
    let mut args = Vec::new();
    while let Type::Forall {
        mut param, body, ..
    } = scheme
    {
        let name = (0usize..)
            .map(|index| format!("_Intrinsic_t{index}"))
            .find(|name| occupied.insert(name.clone()))
            .expect("unbounded fresh binder search");
        let ty = Type::synth_path(vec![name.clone()], Vec::new(), span);
        scheme = crate::pass::typecheck_core::subst_type(
            &body,
            &std::collections::HashMap::from([(param.name.clone(), ty.clone())]),
        );
        param.name = name;
        groups.push(SignatureGroup::Type(vec![param]));
        args.push(CallArg::Type(ty));
    }
    let Type::Function {
        param,
        ret,
        abi_arity,
        ..
    } = scheme
    else {
        unreachable!("checked structural intrinsic scheme ends in a value function")
    };
    let values = Type::right_spine_take(&param, abi_arity)
        .into_iter()
        .enumerate()
        .map(|(index, ty)| {
            let name = format!("_intrinsic_arg{index}");
            args.push(CallArg::Value(Expr::Path {
                occurrence: Default::default(),
                segments: vec![crate::ast::PathSegment::new(name.clone(), span)],
                ext: (),
                meta: Meta::new(span),
            }));
            Param {
                name,
                ty: Some(ty.clone()),
                pattern: (),
                meta: Meta::new(span),
            }
        })
        .collect();
    groups.push(SignatureGroup::Value(values));
    Expr::FnExpr {
        occurrence: Default::default(),
        sig: Signature::from_groups(groups),
        ret_ty: Some(*ret),
        caps: (),
        meta: Meta::new(span),
        body: Box::new(Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(intrinsic),
            args,
            ext: (),
            meta: Meta::new(span),
        }),
    }
}

fn bake_fn_signature(
    sig: &mut crate::ast::Signature<Prime>,
    elabs: &PrimeElaborations,
    module_path: &str,
    fn_id: crate::ast::ExpressionOccurrenceId,
    requalifier: &mut crate::pass::typecheck_core::PrimeTypeRequalifier,
    type_binders: &[String],
) {
    let mut value_index = 0usize;
    for param in &mut sig.params {
        if let crate::ast::SignatureParam::Value(v) = param {
            if v.ty.is_none()
                && let Some(ty) = elabs.fn_param_type_for(module_path, fn_id, value_index)
            {
                v.ty = Some(prime_recorded_type(ty, requalifier, type_binders));
            }
            value_index += 1;
        }
    }
}

fn prime_recorded_type(
    recorded: &crate::pass::typecheck_core::RecordedTypeResolution<Prime>,
    requalifier: &mut crate::pass::typecheck_core::PrimeTypeRequalifier,
    type_binders: &[String],
) -> crate::ast::Type<Prime> {
    if recorded.identity_canonical {
        let bound = type_binders.iter().cloned().collect();
        requalifier.rewrite(&recorded.resolved, &bound)
    } else {
        recorded.resolved.clone()
    }
}

/// Rebuild a Prime `Expr::Call`'s args list in fully-explicit
/// canonical form. Mirrors `substitute::visit_expr_call`'s call-arg
/// canonicalization step — the only divergence is that
/// `Type::Infer` is statically uninhabited at Prime (its `ext` is
/// `Never`), so the "Type::Infer at user_idx" arms collapse.
///
/// At each canonical slot `s`:
///   * If the typer recorded an implicit Unit at `s`, insert one
///     `CallArg::Value(Expr::Unit)` and do not consume a user arg.
///   * If `args[user_idx]` is `CallArg::Type` and there's a
///     resolution at `s`: pass the user type through. Advance
///     both.
///   * If `args[user_idx]` is `CallArg::Value` AND there's a
///     resolution at `s` AND the typer marked this user-index
///     position as `value_at_type_slot=true`: the user wrote a
///     value-shaped expression at a type-arg slot; replace with
///     the recorded type and advance both.
///   * If `args[user_idx]` is `CallArg::Value` AND there's a
///     resolution at `s` (no value-at-type-slot marker): an omitted
///     constructor existential witness; insert the resolution and
///     do **not** advance `user_idx`.
///   * If `args[user_idx]` is `CallArg::Type` or `CallArg::Value`
///     and no resolution at `s`: pass through. Advance both.
///   * If we've consumed every user arg and a resolution still
///     exists at `s`: append it (trailing inferred constructor
///     existential witness).
fn canonicalize_call_args(
    args: &[crate::ast::CallArg<Prime>],
    elabs: &PrimeElaborations,
    module_path: &str,
    call_site: crate::ast::ExpressionSite,
    requalifier: &mut crate::pass::typecheck_core::PrimeTypeRequalifier,
    type_binders: &[String],
) -> Vec<crate::ast::CallArg<Prime>> {
    use crate::ast::CallArg;
    let call_span = call_site.span;
    let mut new_args: Vec<CallArg<Prime>> = Vec::with_capacity(args.len() + 2);
    let mut user_idx = 0usize;
    let mut canonical_slot = 0usize;
    loop {
        if elabs.is_call_implicit_unit_slot(module_path, call_site.id, canonical_slot) {
            new_args.push(CallArg::Value(crate::ast::Expr::Unit {
                occurrence: Default::default(),
                meta: crate::ast::Meta::new(call_span),
            }));
            canonical_slot += 1;
            continue;
        }
        let resolution = elabs.type_resolution_for(module_path, call_site.id, canonical_slot);
        if user_idx < args.len() {
            match (&args[user_idx], resolution) {
                (CallArg::Type(t), _) => {
                    new_args.push(CallArg::Type(t.clone()));
                    user_idx += 1;
                    canonical_slot += 1;
                }
                (CallArg::Value(_), Some(t))
                    if elabs.is_value_at_type_slot(module_path, call_site.id, canonical_slot) =>
                {
                    // User wrote a value-shaped expression at a
                    // type-arg slot; the typer reclassified it
                    // via `expr_to_type_arg`. Replace the slot
                    // with the recorded type and consume the user
                    // arg.
                    new_args.push(CallArg::Type(prime_recorded_type(
                        t,
                        requalifier,
                        type_binders,
                    )));
                    user_idx += 1;
                    canonical_slot += 1;
                }
                (CallArg::Value(_), Some(t)) => {
                    // Inferring-path leading or interleaved type-
                    // arg: insert here, leave the user arg for
                    // the next iteration.
                    new_args.push(CallArg::Type(prime_recorded_type(
                        t,
                        requalifier,
                        type_binders,
                    )));
                    canonical_slot += 1;
                }
                (CallArg::Value(v), None) => {
                    new_args.push(CallArg::Value(v.clone()));
                    user_idx += 1;
                    canonical_slot += 1;
                }
            }
        } else if let Some(t) = resolution {
            new_args.push(CallArg::Type(prime_recorded_type(
                t,
                requalifier,
                type_binders,
            )));
            canonical_slot += 1;
        } else {
            break;
        }
    }
    new_args
}

#[cfg(all(test, feature = "prime"))]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::pass::parser::parse;
    use crate::span::Span;
    use std::path::{Path, PathBuf};

    fn build_package(src: &str) -> Package<Prime> {
        let parsed = parse(src).expect("parse");
        let m = crate::prime::lower::lower_module(parsed).expect("prime::lower");
        // The file path must match the `module …;` declaration so the
        // `Package::build` path-coherence check is satisfied. The path
        // relative to the (empty) package root is the segments after
        // the package name; a single-segment `module pkg;` lives at
        // `pkg.kio`.
        let segs = &m.path.segments;
        let rest = &segs[1.min(segs.len())..];
        let mut file_path = PathBuf::new();
        for seg in &rest[..rest.len().saturating_sub(1)] {
            file_path.push(&seg.name);
        }
        file_path.push(format!(
            "{}.kio",
            rest.last()
                .or_else(|| segs.last())
                .map(|s| s.name.as_str())
                .unwrap_or("module")
        ));
        let package: Package<Prime> =
            Package::build(Path::new(""), vec![(file_path, m)], None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package.check_no_value_cycles().expect("cycle check");
        package
            .check_in_body_resolution()
            .expect("in-body resolution");
        package
    }

    fn build_package_modules(sources: &[(&str, &str)]) -> Package<Prime> {
        let modules = sources
            .iter()
            .map(|(file, source)| {
                let parsed = parse(source).expect("parse");
                let module = crate::prime::lower::lower_module(parsed).expect("prime::lower");
                (PathBuf::from(file), module)
            })
            .collect();
        let package = Package::build(Path::new(""), modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package.check_no_value_cycles().expect("cycle check");
        package
            .check_in_body_resolution()
            .expect("in-body resolution");
        package
    }

    fn prime_type_error(src: &str) -> String {
        let package = build_package(src);
        check_package(&package)
            .expect_err("expected standalone Prime type error")
            .error
            .diag()
            .1
            .to_owned()
    }

    fn intrinsic_value_record_count(package: &Package<Prime>) -> usize {
        let normalized = normalize_package(package);
        let mut elaborations = PrimeElaborations::default();
        let scope =
            crate::pass::typecheck_core::PackageTypecheckScope::fresh_for_normalized_package(
                &normalized,
                std::sync::Arc::new(crate::pass::typecheck_core::TypeInterner::default()),
            );
        check_package_modules_with_typecheck_scope_collect_errors_with_execution(
            normalized.package(),
            &mut elaborations,
            scope,
            crate::pass::typecheck_core::TypecheckExecution::AllowParallel,
        )
        .expect("ordinary checking succeeds");
        elaborations.intrinsic_values.as_ref().map_or(0, |values| {
            assert!(
                !values.is_empty(),
                "zero records must leave the optional inventory absent"
            );
            values.len()
        })
    }

    #[test]
    fn intrinsic_values_recheck_without_new_wrappers() {
        for source in [
            "module x; import __intrinsics__; host type I32; fn f(x: I32, y: I32) -> I32 & I32 { let p = __pair__; p(I32, I32, x, y) }",
            "module x; import __intrinsics__; host type I32; fn f(x: I32, y: I32) -> I32 & I32 { let p = __pair__(I32, I32); p(x, y) }",
            "module x; import __intrinsics__; fn f() -> [A][B](A & B) -> A & B { __pair__ }",
            "module x; import __intrinsics__; host type I32; fn use(p: [A][B](A & B) -> A & B, x: I32, y: I32) -> I32 & I32 { p(I32, I32, x, y) } fn f(x: I32, y: I32) -> I32 & I32 { use(__pair__, x, y) }",
            "module x; import __intrinsics__; host type I32; fn f(x: I32, y: I32) -> I32 & I32 { let p = __pair__; let first = p(I32, I32, x, y); p(I32, I32, __fst__(I32, I32, first), y) }",
            "module x; import __intrinsics__; host type First role(bool); fn f(c: First) -> First { let choose = __if_then_else__; choose(First, c, .() -> First { .t(First) }, .() -> First { .f(First) }) } host type Later role(bool);",
        ] {
            let original = build_package(source);
            assert_eq!(intrinsic_value_record_count(&original), 1, "{source}");
            let checked = check_package(&original).expect("first check");
            assert_eq!(
                intrinsic_value_record_count(&checked),
                0,
                "generated calls are saturated"
            );
            let text =
                crate::backends::kio_prime::emit_module(&checked.module("x").unwrap().module);
            let reparsed = build_package(&text);
            assert_eq!(intrinsic_value_record_count(&reparsed), 0, "{text}");
            let rechecked = check_package(&reparsed).expect("reparsed wrapper is ordinary Prime");
            assert_eq!(
                text,
                crate::backends::kio_prime::emit_module(&rechecked.module("x").unwrap().module)
            );
        }
    }

    #[test]
    fn intrinsic_values_saturated_calls_do_not_record() {
        let package = build_package(
            "module x; import __intrinsics__; fn f(x: ., y: .) -> . & . { __pair__(., ., x, y) }",
        );
        assert_eq!(intrinsic_value_record_count(&package), 0);
        let callable_result = build_package(
            "module x; import __intrinsics__; host type I32; fn f(value: (I32 -> I32) & I32) -> I32 -> I32 { __fst__(I32 -> I32, I32, value) }",
        );
        assert_eq!(intrinsic_value_record_count(&callable_result), 0);
    }

    #[test]
    fn intrinsic_values_ambiguous_role_remains_rejected() {
        let message = prime_type_error(
            "module x; import __intrinsics__; host type First role(bool); host type Second role(bool); fn f() -> . { let choose = __if_then_else__; () }",
        );
        assert!(
            message.contains("more than one `role(bool)` type"),
            "{message}"
        );
    }

    #[test]
    fn intrinsic_values_keep_qualified_role_and_avoid_type_capture() {
        let package = build_package_modules(&[
            (
                "provider.kio",
                "module provider; pub host type Bool role(bool);",
            ),
            (
                "x.kio",
                "module x; import __intrinsics__; import provider as provider; type _Intrinsic_t0 = provider.Bool; fn choose(c: _Intrinsic_t0) -> _Intrinsic_t0 { let pick = __if_then_else__; pick(_Intrinsic_t0, c, .() -> _Intrinsic_t0 { c }, .() -> _Intrinsic_t0 { c }) }",
            ),
        ]);
        assert_eq!(intrinsic_value_record_count(&package), 1);
        let checked = check_package(&package).expect("qualified role is selected once");
        let source = crate::backends::kio_prime::emit_module(&checked.module("x").unwrap().module);
        assert!(source.contains(".[_Intrinsic_t1]"), "{source}");
        let provider =
            crate::backends::kio_prime::emit_module(&checked.module("provider").unwrap().module);
        let reparsed = build_package_modules(&[("provider.kio", &provider), ("x.kio", &source)]);
        assert_eq!(intrinsic_value_record_count(&reparsed), 0);
        let rechecked = check_package(&reparsed).expect("reparsed role identity and binders");
        assert_eq!(
            source,
            crate::backends::kio_prime::emit_module(&rechecked.module("x").unwrap().module)
        );
    }

    #[test]
    fn explicit_prime_arguments_keep_path_alias_literal_and_lambda_checks() {
        let package = build_package(
            "module x;
             host type I32 role(i32);
             type Alias = I32;
             fn identity[A](value: A) -> A { value }
             fn consume[A](_value: A) -> . { () }
             fn path(value: I32) -> I32 { identity(I32, value) }
             fn alias(value: Alias) -> I32 { identity(I32, value) }
             fn literal() -> I32 { identity(I32, 42(I32)) }
             fn lambda() -> . {
               consume(. -> ., .(value: .) -> . { value })
             }",
        );

        check_package(&package).expect(
            "explicit Prime arguments retain ordinary path, alias, literal, and lambda checks",
        );
    }

    #[test]
    fn explicit_prime_argument_mismatches_keep_the_argument_span() {
        for (source, marker) in [
            (
                "module x;
                 host type I32;
                 host type String;
                 fn identity[A](value: A) -> A { value }
                 fn bad(wrong_value: String) -> I32 {
                   identity(I32, wrong_value)
                 }",
                "wrong_value)",
            ),
            (
                "module x;
                 host type I32;
                 host type String;
                 fn second[A](_first: A, value: A) -> A { value }
                 fn bad(first: I32, dependent_wrong: String) -> I32 {
                   second(I32, first, dependent_wrong)
                 }",
                "dependent_wrong)",
            ),
        ] {
            let expected_start = source
                .rfind(marker)
                .expect("unique mismatched argument marker")
                as u32;
            let expected_span = Span::new(expected_start, expected_start + marker.len() as u32 - 1);
            let package = build_package(source);
            let error = check_package(&package).expect_err("mismatched explicit argument");
            let Error::Type(diagnostic) = error.error else {
                panic!("explicit argument mismatch must remain a type error")
            };
            assert_eq!(diagnostic.span, expected_span);
            assert_eq!(
                diagnostic.message,
                "type mismatch: expected `I32`, found `String`"
            );
        }
    }

    #[test]
    fn ordinary_qualified_import_does_not_expose_private_fn() {
        let package = build_package_modules(&[
            (
                "provider.kio",
                "module provider; fn hidden(x: .) -> . { x }",
            ),
            (
                "consumer.kio",
                "module consumer; import provider as p; fn call() -> . { p.hidden(()) }",
            ),
        ]);
        let error = check_package(&package).expect_err("private fn stays private");
        assert!(
            error.error.diag().1.contains("no `pub fn` named `hidden`"),
            "got: {}",
            error.error.diag().1
        );
    }

    /// A type-alias body that's well-formed kind-checks via the
    /// standalone walker.
    #[test]
    fn type_alias_body_kind_checks_standalone() {
        let pkg = build_package("module x; type Pair[A][B] = (A & B);");
        check_package(&pkg).expect("check_package");
    }

    /// Kio-prime accepts existential binders on a newtype: the
    /// Surface → Prime lowering threads `existential_params` through
    /// to the Prime AST, and the prime typer's
    /// `newtype_member_scheme` treats them as additional type-params
    /// on both the constructor and projector schemes.
    #[test]
    fn existentials_threaded_to_prime_and_typecheck() {
        let pkg = build_package(
            "module x; newtype Pack[A] <U> : A & U { \
             pub constructor mk_pack; pub projector un_pack; };",
        );
        check_package(&pkg).expect("check_package");
        let entry = pkg.module("x").expect("module x present");
        let newtype = entry
            .module
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::Newtype(d) if d.name == "Pack" => Some(d),
                _ => None,
            })
            .expect("Pack newtype emitted");
        assert_eq!(
            newtype
                .type_params
                .iter()
                .map(|tp| tp.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A"],
            "universal stays on type_params"
        );
        assert_eq!(
            newtype
                .existential_params
                .iter()
                .map(|tp| tp.name.as_str())
                .collect::<Vec<_>>(),
            vec!["U"],
            "existential stays on existential_params"
        );
    }

    /// A newtype payload with a self-reference under the left of
    /// a function arrow is rejected by the standalone
    /// strict-positivity check via
    /// [`typecheck_core::check_newtype_payload`].
    #[test]
    fn self_ref_under_arrow_lhs_rejected_standalone() {
        let pkg = build_package(
            "module x; rec newtype Bad : (Bad -> .) { \
             pub constructor mk_bad; pub projector un_bad; };",
        );
        let err = check_package(&pkg).expect_err("strict positivity rejects");
        match err.error {
            Error::Totality(_) => {}
            other => panic!("expected Totality error; got {other:?}"),
        }
    }

    /// A simple Kio' function body checks through the shared path and
    /// bidirectional-checking helpers.
    #[test]
    fn simple_fn_def_body_checks_standalone() {
        let pkg = build_package("module x; fn id[A](x: A) -> A { x }");
        check_package(&pkg).expect("check_package");
    }

    #[test]
    fn prime_check_synth_and_immediate_policies_consume_the_shared_header_plan() {
        use crate::pass::typecheck_core::annotation_plan::{
            annotation_plan_consumer_work, reset_annotation_plan_consumer_work,
        };

        reset_annotation_plan_consumer_work();
        check_package(&build_package(
            "module x; fn checked() -> . -> . { .(value: .) -> . { value } }",
        ))
        .expect("Prime checked lambda");
        let checked = annotation_plan_consumer_work();
        assert_eq!(checked.prime_check, 1);
        assert_eq!(checked.prime_synth, 0);
        assert_eq!(checked.prime_immediate, 0);

        reset_annotation_plan_consumer_work();
        check_package(&build_package(
            "module x; fn synthesized() -> . -> . { \
               let identity = .(value: .) -> . { value }; identity \
             }",
        ))
        .expect("Prime synthesized lambda");
        let synthesized = annotation_plan_consumer_work();
        assert_eq!(synthesized.prime_check, 0);
        assert_eq!(synthesized.prime_synth, 1);
        assert_eq!(synthesized.prime_immediate, 0);

        reset_annotation_plan_consumer_work();
        check_package(&build_package(
            "module x; fn immediate() -> . { .(value) { value }(()) }",
        ))
        .expect("Prime immediate lambda");
        let immediate = annotation_plan_consumer_work();
        assert_eq!(immediate.prime_check, 0);
        assert_eq!(immediate.prime_synth, 0);
        assert_eq!(immediate.prime_immediate, 1);
    }

    #[test]
    fn if_intrinsic_rejects_ambiguous_bool_role_standalone_in_both_orders() {
        for declarations in [
            "host type First role(bool); host type Second role(bool);",
            "host type Second role(bool); host type First role(bool);",
        ] {
            let pkg = build_package(&format!(
                "module x; import __intrinsics__; {declarations} \
                 fn pick(c: First) -> First {{ \
                   __if_then_else__(First, c, \
                     .() -> First {{ .t(First) }}, \
                     .() -> First {{ .f(First) }}) \
                 }}"
            ));
            let err = check_package(&pkg).expect_err("ambiguous bool role must reject intrinsic");
            let Error::Type(diagnostic) = err.error else {
                panic!("expected Type error, got {:?}", err.error);
            };
            assert!(
                diagnostic
                    .message
                    .contains("more than one `role(bool)` type"),
                "got: {}",
                diagnostic.message
            );
            assert!(
                diagnostic.message.contains("`x.First`")
                    && diagnostic.message.contains("`x.Second`"),
                "got: {}",
                diagnostic.message
            );
        }
    }

    #[test]
    fn if_intrinsic_rejects_a_mismatched_repeated_result() {
        let package = build_package(
            "module x; import __intrinsics__; \
             host type Bool role(bool); \
             host type Left; \
             host type Right; \
             host fn left() -> Left; \
             host fn right() -> Right; \
             fn pick(c: Bool) -> Left { \
               __if_then_else__(Left, c, .() { left() }, .() { right() }) \
             }",
        );
        let error = check_package(&package).expect_err("both thunks must return the explicit type");
        let Error::Type(diagnostic) = error.error else {
            panic!("expected Type error, got {:?}", error.error);
        };
        assert!(
            diagnostic.message.contains("type mismatch"),
            "unexpected diagnostic: {diagnostic:?}"
        );
    }

    #[test]
    fn if_intrinsic_ignores_bool_role_declared_later() {
        let pkg = build_package(
            "module x; import __intrinsics__; \
             host type First role(bool); \
             fn pick(c: First) -> First { \
               __if_then_else__(First, c, \
                 .() -> First { .t(First) }, \
                 .() -> First { .f(First) }) \
             } \
             host type Later role(bool);",
        );
        check_package(&pkg).expect("a later Boolean-role declaration is not in scope");
    }

    /// A return-type-mismatch in a Kio' program is rejected by the
    /// standalone walker — the typer's diagnostic surfaces normally
    /// through the `LocatedError` path. `wrong` declares
    /// `.> (. & .)` but its body returns `()`.
    #[test]
    fn return_type_mismatch_rejected_standalone() {
        let pkg = build_package("module x;\nfn wrong() -> (. & .) { () }\n");
        let err = check_package(&pkg).expect_err("expected type error");
        match err.error {
            Error::Type(_) => {}
            other => panic!("expected Type error, got {other:?}"),
        }
    }

    #[test]
    fn annotated_lambda_synthesizes_named_complete_scheme_body() {
        let package = build_package(
            "module x;
             fn identity[A](value: A) -> A { value }
             fn staged(_unit: .)[A](value: A) -> A { value }
             fn consume(_thunk: . -> [A] A -> A) -> . { () }
             fn through_local() -> . -> [A] A -> A {
               let wrapper = .(_unit: .) { identity };
               wrapper
             }
             fn through_argument() -> . {
               consume(.() { identity })
             }
             fn through_direct_callee() -> [A] A -> A {
               .(_unit: .) { identity }(())
             }
             fn through_interleaved() -> . -> . -> [A] A -> A {
               let wrapper = .(_unit: .) { staged };
               wrapper
             }",
        );
        check_package(&package)
            .expect("a complete named function scheme is a lambda body value type");
    }

    #[test]
    fn named_scheme_remains_rejected_in_monomorphic_argument_position() {
        let message = prime_type_error(
            "module x;
             fn identity[A](value: A) -> A { value }
             fn consume(_value: .) -> . { () }
             fn bad() -> . { consume(identity) }",
        );
        assert!(
            message.contains("polymorphic value used in a monomorphic position"),
            "got: {message}"
        );
    }

    #[test]
    fn elided_universal_call_type_arg_rejected_in_check_mode() {
        let message = prime_type_error(
            "module x; \
             fn id[T](value: T) -> T { value } \
             fn choose[A](poly: [B] B -> B, value: A) -> A { poly(A, value) } \
             fn bad() -> . { choose(id, ()) }",
        );
        assert!(
            message.contains("Kio' requires an explicit type argument for `[A]`"),
            "got: {message}"
        );
    }

    #[test]
    fn elided_universal_call_type_arg_rejected_in_synth_mode() {
        let message = prime_type_error(
            "module x; \
             fn id[T](value: T) -> T { value } \
             fn choose[A](poly: [B] B -> B, value: A) -> A { poly(A, value) } \
             fn bad() -> . { let x = choose(id, ()); x }",
        );
        assert!(
            message.contains("Kio' requires an explicit type argument for `[A]`"),
            "got: {message}"
        );
    }

    #[test]
    fn constructor_shaped_rank_n_alias_does_not_inherit_inference() {
        let message = prime_type_error(
            "module x; \
             newtype Hidden <U> : U { \
               pub constructor mk_hidden; pub projector un_hidden; \
             }; \
             fn bad[A](poly: [B] B -> Hidden, value: A) -> Hidden { poly(value) }",
        );
        assert!(
            message.contains("Kio' requires an explicit type argument for `[B]`"),
            "got: {message}"
        );
    }

    #[test]
    fn elided_pair_type_args_rejected_standalone() {
        let message = prime_type_error(
            "module x; import __intrinsics__; \
             fn bad() -> (. & .) { __pair__((), ()) }",
        );
        assert!(
            message.contains("Kio' requires an explicit type argument"),
            "got: {message}"
        );
    }

    #[test]
    fn existential_constructor_suffix_stays_inference_driven() {
        let package = build_package(
            "module x; import __intrinsics__; \
             newtype Hidden <U> : U { \
               pub constructor mk_hidden; pub projector un_hidden; \
             }; \
             newtype Pack[A] <U> : A & U { \
               pub constructor mk_pack; pub projector un_pack; \
             }; \
             fn hidden[U](value: U) -> Hidden { Hidden.mk_hidden(value) } \
             fn direct[A][U](a: A, u: U) -> Pack(A) { \
               Pack.mk_pack(A, __pair__(A, U, a, u)) \
             } \
             fn partial[A][U](a: A, u: U) -> Pack(A) { \
               Pack.mk_pack(A)(__pair__(A, U, a, u)) \
             }",
        );
        check_package(&package).expect("constructor existential suffix remains inferable");
    }

    #[test]
    fn existential_constructor_does_not_hide_elided_universal_prefix() {
        let message = prime_type_error(
            "module x; import __intrinsics__; \
             newtype Pack[A] <U> : A & U { \
               pub constructor mk_pack; pub projector un_pack; \
             }; \
             fn bad[A][U](a: A, u: U) -> Pack(A) { \
               Pack.mk_pack(__pair__(A, U, a, u)) \
             }",
        );
        assert!(
            message.contains("Kio' requires an explicit type argument for `[A]`"),
            "got: {message}"
        );
    }

    #[test]
    fn empty_polymorphic_function_call_requires_type_arg() {
        let message = prime_type_error(
            "module x; \
             fn make[A]() -> . { () } \
             fn bad() -> . { make() }",
        );
        assert!(
            message.contains("Kio' requires an explicit type argument for `[A]`"),
            "got: {message}"
        );
    }

    #[test]
    fn written_type_only_unit_domain_is_residual_in_fresh_prime() {
        use crate::ast::{CallArg, Expr, Item, Type};

        let typed = check_package(&build_package(
            "module x; \
             host type N; \
             host fn nil[A]() -> A; \
             fn identity[A](value: A) -> A { value } \
             fn use(value: .) -> . { value } \
             fn residual_nil() -> . -> N { nil(N) } \
             fn nested_nil() -> N { nil(N)() } \
             fn saturated_nil() -> N { nil(N, ()) } \
             fn residual_identity() -> N -> N { identity(N) } \
             fn saturated_identity(value: N) -> N { identity(N, value) } \
             fn monomorphic_empty() -> . { use() }",
        ))
        .expect("fresh Kio' keeps type-only packets residual and bakes empty Unit calls");
        let module = &typed.module("x").expect("module x present").module;

        for name in ["residual_nil", "residual_identity"] {
            let definition = module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::FnDef(definition) if definition.name == name => Some(definition),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("fn `{name}` present"));
            let Expr::Call { args, .. } = &definition.body else {
                panic!("{name} must remain a call")
            };
            assert!(
                matches!(args.as_slice(), [CallArg::Type(_)]),
                "{name} must retain its value layer: {args:?}"
            );
        }

        let monomorphic = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(definition) if definition.name == "monomorphic_empty" => {
                    Some(definition)
                }
                _ => None,
            })
            .expect("fn `monomorphic_empty` present");
        let Expr::Call { args, .. } = &monomorphic.body else {
            panic!("monomorphic_empty must remain a call")
        };
        assert!(
            matches!(args.as_slice(), [CallArg::Value(Expr::Unit { .. })]),
            "the empty call must bake one Unit value: {args:?}"
        );

        let saturated = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(definition) if definition.name == "saturated_nil" => Some(definition),
                _ => None,
            })
            .expect("fn `saturated_nil` present");
        let Expr::Call { args, .. } = &saturated.body else {
            panic!("saturated_nil must remain a call")
        };
        assert!(
            matches!(
                args.as_slice(),
                [
                    CallArg::Type(Type::Path { segments, .. }),
                    CallArg::Value(Expr::Unit { .. })
                ] if segments.last().is_some_and(|segment| segment.as_str() == "N")
            ),
            "the written Unit must saturate the selected value layer: {args:?}"
        );
    }

    #[test]
    fn empty_polymorphic_non_function_call_requires_type_arg() {
        let message = prime_type_error(
            "module x; \
             fn bad(poly: [A] A) -> . { let value = poly(); () }",
        );
        assert!(
            message.contains("Kio' requires an explicit type argument for `[A]`"),
            "got: {message}"
        );
    }

    #[test]
    fn empty_monomorphic_call_bakes_the_implicit_unit_value() {
        use crate::ast::{CallArg, Expr, Item};

        let pkg = build_package(
            "module x; \
             fn use(value: .) -> . { value } \
             fn implicit() -> . { use() } \
             fn explicit() -> . { use(()) }",
        );
        let typed = check_package(&pkg).expect("both Unit applications typecheck");
        let module = &typed.module("x").expect("module x present").module;

        for name in ["implicit", "explicit"] {
            let definition = module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::FnDef(definition) if definition.name == name => Some(definition),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("fn `{name}` present"));
            let Expr::Call { args, .. } = &definition.body else {
                panic!("{name} must remain a call")
            };
            assert!(
                matches!(args.as_slice(), [CallArg::Value(Expr::Unit { .. })]),
                "{name} must carry exactly one Unit value after baking: {args:?}"
            );
        }
    }

    #[test]
    fn selected_unit_domain_call_remains_residual_until_unit_is_written() {
        use crate::ast::{CallArg, Expr, Item, Type};

        let package = build_package(
            "module x;
             type Unit_alias = .;
             fn zero[A]() -> . { () }
             fn identity[A](value: A) -> A { value }
             fn selected() -> . -> . { zero(.) }
             fn explicit() -> . { zero(., ()) }
             fn selected_identity() -> . -> . { identity(.) }
             fn selected_alias() -> Unit_alias -> Unit_alias { identity(Unit_alias) }",
        );
        let typed = check_package(&package).expect("selected Unit-domain calls typecheck");
        let module = &typed.module("x").expect("module x present").module;
        for name in ["selected", "selected_identity", "selected_alias"] {
            let definition = module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::FnDef(definition) if definition.name == name => Some(definition),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("fn `{name}` present"));
            let Expr::Call { args, .. } = &definition.body else {
                panic!("{name} must remain a call")
            };
            assert!(
                matches!(args.as_slice(), [CallArg::Type(_)]),
                "{name} must retain its Unit value layer: {args:?}"
            );
        }

        let definition = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(definition) if definition.name == "explicit" => Some(definition),
                _ => None,
            })
            .expect("fn `explicit` present");
        let Expr::Call { args, .. } = &definition.body else {
            panic!("explicit must remain a call")
        };
        assert!(
            matches!(
                args.as_slice(),
                [
                    CallArg::Type(Type::Unit { .. }),
                    CallArg::Value(Expr::Unit { .. })
                ]
            ),
            "an explicitly written Unit must saturate the selected value layer: {args:?}"
        );
    }

    #[test]
    fn selected_non_unit_domain_call_remains_a_residual_function() {
        use crate::ast::{CallArg, Expr, Item, Type};

        let package = build_package(
            "module x;
             host type N;
             fn consume[A](value: N) -> . { () }
             fn residual() -> N -> . { consume(.) }",
        );
        let typed = check_package(&package).expect("a type-only non-Unit call typechecks");
        let definition = typed
            .module("x")
            .expect("module x present")
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(definition) if definition.name == "residual" => Some(definition),
                _ => None,
            })
            .expect("fn `residual` present");
        let Expr::Call { args, .. } = &definition.body else {
            panic!("residual must remain a call")
        };
        assert!(
            matches!(args.as_slice(), [CallArg::Type(Type::Unit { .. })]),
            "a non-Unit domain must remain unapplied after a type-only call: {args:?}"
        );
    }

    #[test]
    fn nested_elided_universal_under_existential_inference_rejected() {
        let message = prime_type_error(
            "module x; import __intrinsics__; \
             newtype Hidden <U> : U { \
               pub constructor mk_hidden; pub projector un_hidden; \
             }; \
             fn from_bottom[A](never: !) -> A { __absurd__(A, never) } \
             fn bad(never: !) -> Hidden { Hidden.mk_hidden(from_bottom(never)) }",
        );
        assert!(
            message.contains("Kio' requires an explicit type argument for `[A]`"),
            "got: {message}"
        );
    }

    /// The call-site dual of `specs/language.md` § 5's no-
    /// interleaving rule: Kio' rejects multi-layer flat calls at
    /// type-check (per § 4c). `foo[A](v1: A)[B](v2: B) -> .`
    /// has type
    ///   `Forall([A], Function(A, Forall([B], Function(B, Unit))))`;
    /// the flat spelling `foo(A, A.mk_a(()), B, B.mk_b(()))` is
    /// Kio-only sugar that Kio surface accepts under `LoweredTyper`
    /// (`ALLOW_MULTI_LAYER_FLAT_CALL = true`). `PrimeTyper` opts
    /// out (`ALLOW_MULTI_LAYER_FLAT_CALL = false`); the only
    /// spelling Kio' accepts is the explicit curried form
    /// `foo(A, A.mk_a(()))(B, B.mk_b(()))`.
    #[test]
    fn multi_layer_flat_call_rejected_standalone() {
        let pkg = build_package(
            "module x;\n\
             newtype A : . { pub constructor mk_a; pub projector un_a; };\n\
             newtype B : . { pub constructor mk_b; pub projector un_b; };\n\
             fn foo[A](v1: A)[B](v2: B) -> . { () }\n\
             fn main() -> . { foo(A, A.mk_a(()), B, B.mk_b(())) }\n",
        );
        let err = check_package(&pkg).expect_err("Kio' rejects multi-layer flat call");
        match err.error {
            Error::Type(_) => {}
            other => panic!("expected Type error, got {other:?}"),
        }
    }

    /// The Kio' dual: the explicit curried spelling is accepted by
    /// the standalone Kio' walker — one application per curry
    /// layer, no interleaving across layers.
    #[test]
    fn multi_layer_curried_call_accepted_standalone() {
        let pkg = build_package(
            "module x;\n\
             newtype A : . { pub constructor mk_a; pub projector un_a; };\n\
             newtype B : . { pub constructor mk_b; pub projector un_b; };\n\
             fn foo[A](v1: A)[B](v2: B) -> . { () }\n\
             fn main() -> . { foo(A, A.mk_a(()))(B, B.mk_b(())) }\n",
        );
        check_package(&pkg).expect("Kio' accepts curried call");
    }

    /// A call site of the shape `f(A, x)` — where `f` is a value
    /// parameter of polymorphic-fn type and `A` is a type binder in
    /// scope — comes in from the parser as
    /// `Call(f, [Value(Path(A)), Value(Path(x))])` because the
    /// grammar (`specs/grammar.md`, `CallArg ::= Expr | Type`) does
    /// not syntactically distinguish type-args from value-args and
    /// the parser classifies a bare ident as `CallArg::Value`. The typer
    /// reclassifies via `expr_to_type_arg` and records the
    /// resolution; the bake pass rewrites the slot into canonical
    /// `CallArg::Type(Path(A))` form so downstream emitters drop the
    /// type-arg at the FFI boundary.
    ///
    /// Regression for the kio-prime Church-encoding bug: without the
    /// bake step, the type binder `A` survived as a runtime
    /// reference and the emitted JS hit a `ReferenceError`.
    #[test]
    fn polymorphic_local_fn_call_canonicalizes_type_arg_slot() {
        use crate::ast::{CallArg, Expr, Item};
        // `apply` takes a polymorphic-fn value `f` and applies it
        // to a value `x` at type `A`. The body's `f(A, x)` is the
        // shape `n(A, f, x)` in the Church-numeral case — value
        // parameter as callee, type binder as first arg.
        let pkg = build_package(
            "module x;\n\
             fn apply[A](f: [B] B -> B, x: A) -> A { f(A, x) }\n",
        );
        let typed = check_package(&pkg).expect("check_package");
        let entry = typed.module("x").expect("module x present");
        let apply_fn = entry
            .module
            .items
            .iter()
            .find_map(|it| match it {
                Item::FnDef(d) if d.name == "apply" => Some(d),
                _ => None,
            })
            .expect("apply fn present");
        let Expr::Call { args, .. } = &apply_fn.body else {
            panic!("expected Expr::Call, got {:?}", apply_fn.body);
        };
        assert_eq!(args.len(), 2, "canonical args list has two slots");
        // Slot 0: the type binder `A`, canonicalized from
        // `CallArg::Value(Path("A"))` to `CallArg::Type(Path("A"))`.
        match &args[0] {
            CallArg::Type(crate::ast::Type::Path { segments, .. }) => {
                assert_eq!(
                    segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                    vec!["A"],
                    "slot 0 carries the type binder `A`"
                );
            }
            other => panic!("slot 0 should be CallArg::Type(Path(\"A\")); got {other:?}"),
        }
        // Slot 1: the value `x`, untouched.
        match &args[1] {
            CallArg::Value(Expr::Path { segments, .. }) => {
                assert_eq!(
                    segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                    vec!["x"],
                    "slot 1 carries the value `x`"
                );
            }
            other => panic!("slot 1 should be CallArg::Value(Path(\"x\")); got {other:?}"),
        }
    }

    #[test]
    fn bake_indexes_generated_qualified_imports_in_the_returned_package() {
        use crate::ast::{Expr, ImportKind, Item, PathSegment, Type};

        let package = build_package_modules(&[
            (
                "provider.kio",
                "module provider; pub host type I32 role(i32);",
            ),
            (
                "consumer.kio",
                "module consumer; \
                 fn id[A](value: A) -> A { value } \
                 fn value(x: .) -> . { id(x) }",
            ),
        ]);
        let mut package = crate::pass::alpha_normalize::normalize_package(&package)
            .into_parts()
            .0;
        let call_span = package
            .module("consumer")
            .expect("consumer module")
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "value" => match &def.body {
                    Expr::Call { .. } => Some(def.body.site()),
                    other => panic!("value body should be a call, got {other:?}"),
                },
                _ => None,
            })
            .expect("value function");

        let mut elaborations = PrimeElaborations::default();
        elaborations.record_type_resolution(
            "consumer",
            call_span.id,
            0,
            crate::pass::typecheck_core::InternedType::fresh_canonical(Type::synth_path(
                vec!["provider".to_owned(), "I32".to_owned()],
                Vec::new(),
                call_span.span,
            )),
        );
        bake_prime_elaborations(&mut package, &elaborations);

        let consumer = package.module("consumer").expect("consumer module");
        let generated_alias = consumer
            .module
            .imports
            .iter()
            .find_map(|usage| match &usage.kind {
                ImportKind::Qualified { path, alias }
                    if path
                        .segments
                        .iter()
                        .map(PathSegment::as_str)
                        .eq(["provider"]) =>
                {
                    Some(alias.as_str())
                }
                _ => None,
            })
            .expect("bake injects an exact provider import");
        let qualified = crate::pass::resolve::qualify_type_segments_in_entry(
            &[
                PathSegment::new(generated_alias.to_owned(), call_span.span),
                PathSegment::new("I32".to_owned(), call_span.span),
            ],
            consumer,
            &std::collections::HashMap::new(),
        );
        assert_eq!(
            qualified
                .iter()
                .map(PathSegment::as_str)
                .collect::<Vec<_>>(),
            vec!["provider", "I32"],
            "the returned package scope must index imports injected by Prime baking"
        );
    }

    #[test]
    fn returned_polymorphic_value_call_canonicalizes_type_arg_slot() {
        use crate::ast::{CallArg, Expr, Item};

        let pkg = build_package(
            "module x;\n\
             host type N;\n\
             host fn produce(_unit: .) -> [A] A;\n\
             fn returned() -> N { produce(())(N) }\n",
        );
        let typed = check_package(&pkg).expect("check returned polymorphic value call");
        let entry = typed.module("x").expect("module x present");
        let returned = entry
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(definition) if definition.name == "returned" => Some(definition),
                _ => None,
            })
            .expect("returned fn present");
        let Expr::Call { args, .. } = &returned.body else {
            panic!("returned body should be the outer type application")
        };
        assert!(
            matches!(args.as_slice(), [CallArg::Type(crate::ast::Type::Path { segments, .. })]
                if segments.last().is_some_and(|segment| segment.as_str() == "N")),
            "returned type application must bake its nominal argument: {args:?}"
        );
    }

    #[test]
    fn prime_elaboration_merge_canonicalizes_multiple_fn_bodies() {
        use crate::ast::{CallArg, Expr, Item};

        let pkg = build_package(
            "module x;\n\
             fn apply1[A](f: [B] B -> B, x: A) -> A { f(A, x) }\n\
             fn apply2[A](f: [B] B -> B, x: A) -> A { f(A, x) }\n",
        );
        let typed = check_package(&pkg).expect("check_package");
        let entry = typed.module("x").expect("module x present");
        for name in ["apply1", "apply2"] {
            let apply_fn = entry
                .module
                .items
                .iter()
                .find_map(|it| match it {
                    Item::FnDef(d) if d.name == name => Some(d),
                    _ => None,
                })
                .expect("apply fn present");
            let Expr::Call { args, .. } = &apply_fn.body else {
                panic!("expected Expr::Call in {name}, got {:?}", apply_fn.body);
            };
            assert!(
                matches!(args.first(), Some(CallArg::Type(_))),
                "{name} should carry a canonical type arg in slot 0"
            );
        }
    }
}
