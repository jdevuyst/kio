//! Phase-polymorphic typer machinery shared between the kio binary's
//! elaboration-aware typer ([`crate::pass::typecheck_full`]) and the
//! kio-prime binary's standalone Kio'-only typer
//! ([`crate::prime::typer`]).
//!
//! The module covers:
//!
//! - **Pure type helpers** — `subst_type`, `display_type`,
//!   `unfold_top` / `type_equiv` / `require_type_equiv` (with `AliasCtx<P>`),
//!   `check_well_formed_type`, `right_fold_sum`,
//!   `expr_to_type_arg`,
//!   `intrinsic_call`, `clone_with_span`, the tiny constructors (`ty_path` /
//!   `ty_sum` / `ty_product` / `ty_func` / `ty_nominal_segments` / `tp` /
//!   `vp`). None touch the typer's stack
//!   or the elaboration table.
//! - **Per-fn machinery** — `ModuleEnv<'m, P>` (with `build`,
//!   `populate_env_imports`, `populate_cross_module_imports`,
//!   `alias_ctx`, `payload_ctx`), `Local<'m, P>`,
//!   `TypeCtx<'m, 'e, P: TyperPhase>`. The TypeCtx threads the
//!   per-phase elaboration table via `P::Elaborations`: the full
//!   `Elaborations` map at Lowered, and at Prime the narrow
//!   `PrimeElaborations` table of resolved call type slots, implicit-Unit
//!   slots, and checked-lambda value-parameter types that the post-check bake
//!   installs in the AST.
//! - **Synth / check / apply helpers** — `Synth<P>` struct,
//!   `synth_path`, `synth_let`, `synth_fn`, `synth_call`,
//!   `synth_value_arg`, `synth_literal`, `apply_mono`,
//!   `apply_polymorphic_function`, the surface-only owner-scoped goal store
//!   and retained-application frontier, `check_fn_against`,
//!   `check_fn_def`, the scoped package/module checkers, plus the
//!   scheme builders (`synth_top_fn_def`, `synth_env_fn_def`,
//!   `newtype_member_scheme`, `intrinsic_scheme`) and the
//!   closed-type unifier (`unify_pattern`). The recursive ones dispatch back
//!   into the per-phase `synth_expr` / `check_value_against` via the
//!   [`Typer<P>`] trait — the only place the kio and kio-prime
//!   typers diverge.
//!
//! ## Phase-polymorphism
//!
//! Every helper is parametric over `<P>`, with one of three
//! constraint shapes depending on what the body needs:
//!
//! - **`<P: Phase + Clone>`** — for helpers that don't
//!   pattern-match on Type variants beyond what every phase has
//!   (`right_fold_sum`, `intrinsic_call`, `expr_to_type_arg`).
//! - **`<P: Phase<TypeLabelSugar = Never> [+ ItemLabels = Never]>`** — for
//!   helpers that pattern-match on
//!   `Type::LabelSugar` (and `Item::Labels` for the module-level
//!   walkers) and discharge each via `match *ext {}`. Lowered and
//!   Prime both satisfy these bounds; Surface and Desugared
//!   aren't typer input phases.
//! - **`<P: TyperPhase>`** — for helpers that thread `TypeCtx<P>`
//!   and recurse via `<P::Typer as Typer<P>>::synth_expr`. The
//!   trait method's per-phase wiring is what lets `synth_let`,
//!   `synth_fn`, `synth_call`, `apply_mono`, etc. all live in
//!   one place even though their recursive calls land in
//!   different `synth_expr` implementations at Lowered (which has
//!   the four elaboration arms inhabited) vs Prime (which
//!   discharges them via `match *ext {}`).
//!
//! `typecheck_full` calls these with `P = Lowered` (inferred from
//! the argument types); `prime::typer` calls them with `P = Prime`.
//! Both work without a duplicate copy.

use crate::ast::Meta;
use crate::span::Span;

#[cfg(test)]
use crate::ast::Lowered;
#[cfg(test)]
type Type = crate::ast::Type<Lowered>;
#[cfg(test)]
type Expr = crate::ast::Expr<Lowered>;

use crate::error::Error;

// The umbrella's substantive content lives in sub-modules. The umbrella
// keeps only the trait / dispatch infrastructure (`Synth`,
// `TyperPhase`, `Typer`, `default_check_value_against`)
// and exports the sub-modules' public surface so external consumers
// (`crate::pass::typecheck_full`, `crate::prime::typer`, `crate::package_collection`,
// the test module) keep their existing `use crate::pass::typecheck_core::…`
// paths. The split:
//
// - [`types`] — pure AST builders, validators, and pretty-printers.
// - [`intern`] — per-module type interning handles used by equality hot paths.
// - [`aliases`] — alias unfolding, structural equivalence,
//   `AliasCtx` / `PayloadCtx`.
// - [`variance`] — the strict-positivity check on `newtype` payloads.
// - [`env`] — `ModuleEnv`, `TypeCtx`, `Local`, and the three
//   `populate_*` import-folding helpers.
// - [`modules`] — module-level entry points (per-module check,
//   per-fn check, package-file validation).
// - [`apply`] — call-site dispatch (`synth_call`, the `apply_*`
//   family, polymorphic-inference engine).
// - [`synth`] — per-expression and per-item synthesis helpers,
//   bidirectional `check_fn_against`, and intrinsic scheme builders.
mod types;
#[cfg(any(feature = "surface", test))]
pub(crate) use types::canonicalize_type_expression_function_abi;
pub(crate) use types::type_contains_goal;
pub use types::{
    CallArgSlotKind, InferFreeType, call_arg_to_type_arg, call_arg_type_slot_candidate,
    check_kinds_in_type, check_no_infer, check_well_formed_type, clone_with_span, compute_kind,
    display_type, expr_to_type_arg, intrinsic_call, lookup_newtype_by_name,
    nominal_segments_in_module, right_fold_product, right_fold_sum, scheme_in_mono_position,
    subst_type, tp, ty_func, ty_nominal_segments, ty_path, ty_product, ty_sum, type_contains_infer,
    value_arg_looks_like_type_arg, vp, write_type,
};
pub(crate) use types::{append_type_args, collect_free_type_vars, fresh_type_var};
#[cfg(feature = "surface")]
pub(crate) use types::{assert_goal_free, check_callable_spine_kinds};
#[cfg(feature = "surface")]
pub(crate) use types::{display_reflected_type, display_reflected_type_name};

#[cfg(feature = "surface")]
mod goals;
#[cfg(feature = "surface")]
pub(crate) use goals::{
    CloseReadyEquationConstraint, ClosedGoalOutput, ClosedProjectedType, ClosedPublicationOutput,
    ExpectedEquationOperand, GoalDelta, GoalDeltaAuthority, GoalEscape, GoalFreePublicationOutput,
    GoalOrigin, GoalOwnerKind, GoalRole, GoalSolutionPolicy, GoalStore, GoalTypeContext,
    IsolatedSpecialization, IsolatedSpecializationContext, IsolatedSpecializationContextBuilder,
    PreparedCloseType, PreparedGoalClose, PreparedGoalCommit, PreparedTypeShell,
    ProjectedReflectionCandidate, ProjectedTypeCloseScratch, PublicationGoalEscape,
    PublicationGoalInput, ReservedGoalRef, ReservedParentHeaderExport, RigidScope, ScopedType,
    ScopedTypeBinaryKind, ScopedTypeEdge, contains_goal_beneath_forall,
    solve_canonical_specialization_by_name, solve_scoped_specialization_by_name,
};
#[cfg(all(feature = "surface", test))]
pub(crate) use goals::{
    GoalContextCapability, publication_validation_work, reset_publication_validation_work,
};

mod intern;
#[cfg(feature = "lsp")]
pub(crate) use intern::PackageTypecheckRevisionStore;
#[cfg(all(feature = "surface", test))]
pub(crate) use intern::TransientTypeKey;
#[cfg(feature = "surface")]
pub(crate) use intern::TypeMemoKey;
pub use intern::{InternedType, PackageTypecheckScope, TypeInterner};

mod kind_scheme;

mod requalify;
pub(crate) use requalify::{
    PrimeLocalTypeAlias, PrimeTypeRequalifier, source_type_spelling_with_bound,
    source_type_spelling_with_lookup,
};
#[cfg(feature = "surface")]
pub(crate) use requalify::{compact_source_type_spelling_with_bound, source_type_spelling};

mod memo;
pub use memo::{
    MemoCtx, PolymorphicInstantiationKey, UserElaboratorTimingSnapshot, memo_verify_enabled,
};
#[cfg(feature = "surface")]
pub use memo::{
    PreparedUserElaborator, UserElaboratorMemoKey, UserElaboratorPreparedKey,
    UserElaboratorPreparedPurpose, UserElaboratorTemplate, UserElaboratorTemplateGeneratedImport,
    UserElaboratorTemplateResult,
};
#[cfg(feature = "surface")]
pub(crate) use memo::{
    UserElaboratorArtifactKey, UserElaboratorArtifactMemo, UserElaboratorEvalArtifactPair,
    UserElaboratorMemoInputKey,
};

mod aliases;
pub(crate) use aliases::canonicalize_deep_for_comparison;
#[cfg(all(test, feature = "surface"))]
pub(crate) use aliases::{
    deep_canonicalization_calls_for_test, reset_deep_canonicalization_calls_for_test,
};
pub(crate) mod annotation_plan;
mod persistent_exact;
#[cfg(feature = "surface")]
pub(crate) use aliases::canonicalize_for_comparison;
pub use aliases::{
    AliasCtx, AliasDef, PayloadCtx, require_type_equiv, type_equiv, unfold_and_qualify, unfold_top,
};
pub(crate) use aliases::{canonical_declared_alias_body, require_type_equiv_state};
#[cfg(feature = "surface")]
pub(crate) use aliases::{materialize_aliases_in_scope, measure_materialized_aliases_in_scope};

mod variance;
pub use variance::{
    NewtypeSccs, Variance, VarianceEnv, check_newtype_payload, compute_newtype_sccs,
    compute_variance_env,
};

mod env;
pub use env::{
    Local, ModuleEnv, PendingRecOrderState, RoleResolution, TypeCtx, module_path_key,
    populate_cross_module_imports,
};
#[cfg(feature = "surface")]
pub(crate) use env::{PreparedRecOrderCommit, RetainedTypeBinder};

mod modules;
pub(crate) use modules::TypecheckExecution;
pub(crate) use modules::check_host_type_parameters;
pub(crate) use modules::check_module_in_package_with_typecheck_scope_collect_errors;
#[cfg(feature = "cli")]
pub(crate) use modules::check_module_signatures;
pub(crate) use modules::check_no_alias_only_cycles;
pub(crate) use modules::check_package_modules_with_typecheck_scope_collect_errors_with_execution;
pub use modules::{check_equiv, check_fn_def, check_package_signatures};

pub(crate) mod apply;
pub use apply::{apply_mono, check_call_against, synth_call, synth_ufcs};

mod synth;
pub(crate) use synth::newtype_member_scheme_in_module;
#[cfg(feature = "surface")]
pub(crate) use synth::synth_value_arg_interned;
#[cfg(feature = "surface")]
pub(crate) use synth::unify_pattern_with_holes_state;
#[cfg(feature = "surface")]
pub(crate) use synth::unique_role_admitted_type_at;
pub use synth::{
    check_literal_annotation, comptime_scheme, intrinsic_scheme, newtype_member_scheme,
    synth_env_fn_def, synth_literal, synth_path, synth_seq, synth_top_fn_def, synth_value_arg,
    unify_pattern, unique_role_admitted_type,
};
pub(crate) use synth::{check_prime_fn_against, synth_closed_fn_with_source_plan};
#[cfg(all(feature = "surface", feature = "lsp"))]
pub(crate) use synth::{
    intrinsic_scheme_resolved, synth_env_fn_def_in_module, synth_top_fn_def_in_module,
};

// =========================================================================
// Synth — what a path or lambda expression resolves to in callee position
// =========================================================================

/// Result of synthesizing a path or lambda expression.
///
/// `ty` is the synthesized System-F type — possibly polymorphic
/// (`Forall(...Function(...))`). `is_scheme` is `true` iff the
/// `Synth` came from a *named-callee scheme reference* (a `fn`,
/// host fn, newtype member, or intrinsic that has type-parameter
/// binders). Genuinely monomorphic consumers call [`Self::into_mono`]
/// and require the user to apply the type arguments first. Value-type
/// consumers instead admit a structurally complete callable scheme and
/// reject incomplete schemes; see `synth_value_type`.
///
/// Value-position synthesis (let-results, locals, expression
/// results, `fn` value expressions) always sets `is_scheme = false`,
/// even when the type itself is polymorphic. A let-bound polymorphic
/// value is a value, not a scheme: the language admits explicit
/// type-application at the call site without forcing the user to
/// instantiate at the bind site (see `test-data/goldens/00_success/
/// typecheck_rank_n`).
///
/// Call-site dispatch uses the function domain's normalized ABI-slot
/// view for type-argument elision and flat calls across curry layers.
/// Type equality is based on the folded function domain type.
///
/// Phase-polymorphic so the typer's notion of "what does this name
/// mean" can be lifted across phases. `typecheck_full` consumes
/// `Synth<Lowered>`; the standalone Kio'-only walker in
/// [`crate::prime::typer`] consumes `Synth<Prime>`.
pub struct Synth<P>
where
    P: crate::ast::Phase,
{
    pub ty: InternedType<P>,
    pub is_scheme: bool,
    /// Complete-function-scheme classification established by the producer.
    /// Named callable producers construct a `forall* -> function` spine;
    /// value producers carry `Incomplete`. Consumers never rescan `ty` to
    /// rediscover this producer invariant.
    pub(crate) complete_scheme: kind_scheme::CompleteScheme,
    /// Trailing `Forall` binders whose call-site arguments are
    /// inference-driven even in Kio'. This is nonzero only for a
    /// newtype constructor's existential witnesses.
    pub(crate) inferred_type_arg_suffix: usize,
}

impl<P> Synth<P>
where
    P: crate::ast::Phase + Clone,
{
    /// Build a value-position [`Synth`] — locals, let-results,
    /// expression results, `fn` value expressions, and named
    /// monomorphic callees (the latter behave like values once they
    /// have no type-binders to apply). The `ty` may itself be
    /// polymorphic, but the polymorphism is a property of the value,
    /// not a "callee-only" restriction.
    pub fn value(ty: crate::ast::Type<P>) -> Self {
        Self {
            ty: InternedType::fresh(ty),
            is_scheme: false,
            complete_scheme: kind_scheme::CompleteScheme::Incomplete,
            inferred_type_arg_suffix: 0,
        }
    }

    pub fn value_interned(ty: InternedType<P>) -> Self {
        Self {
            ty,
            is_scheme: false,
            complete_scheme: kind_scheme::CompleteScheme::Incomplete,
            inferred_type_arg_suffix: 0,
        }
    }

    pub(crate) fn canonical_value(ty: crate::ast::Type<P>) -> Self {
        Self {
            ty: InternedType::fresh_canonical(ty),
            is_scheme: false,
            complete_scheme: kind_scheme::CompleteScheme::Incomplete,
            inferred_type_arg_suffix: 0,
        }
    }

    /// Build a conservatively classified polymorphic scheme. Not bindable as
    /// a value — [`Self::into_mono`] returns an error pointed at the type's
    /// span. Known callable producers use the crate-private complete-scheme
    /// constructors below after establishing their exact outer spine.
    pub fn scheme(ty: crate::ast::Type<P>) -> Self {
        Self {
            ty: InternedType::fresh(ty),
            is_scheme: true,
            complete_scheme: kind_scheme::CompleteScheme::Incomplete,
            inferred_type_arg_suffix: 0,
        }
    }

    pub fn scheme_interned(ty: InternedType<P>) -> Self {
        Self {
            ty,
            is_scheme: true,
            complete_scheme: kind_scheme::CompleteScheme::Incomplete,
            inferred_type_arg_suffix: 0,
        }
    }

    pub(crate) fn canonical_scheme(ty: crate::ast::Type<P>) -> Self {
        Self {
            ty: InternedType::fresh_canonical(ty),
            is_scheme: true,
            complete_scheme: kind_scheme::CompleteScheme::Incomplete,
            inferred_type_arg_suffix: 0,
        }
    }

    pub(crate) fn complete_scheme(ty: crate::ast::Type<P>) -> Self {
        Self::scheme(ty).with_complete_scheme()
    }

    #[cfg(feature = "surface")]
    pub(crate) fn complete_scheme_interned(ty: InternedType<P>) -> Self {
        Self::scheme_interned(ty).with_complete_scheme()
    }

    pub(crate) fn canonical_complete_scheme(ty: crate::ast::Type<P>) -> Self {
        Self::canonical_scheme(ty).with_complete_scheme()
    }

    fn with_complete_scheme(mut self) -> Self {
        self.complete_scheme = kind_scheme::CompleteScheme::Complete;
        self
    }

    pub(crate) fn with_inferred_type_arg_suffix(mut self, count: usize) -> Self {
        self.inferred_type_arg_suffix = count;
        self
    }

    /// Convert to a monomorphic type, erroring if this is a polymorphic
    /// scheme. Callers use this only after their particular semantic slot has
    /// been classified as monomorphic; complete schemes remain admissible in
    /// structural value slots whose declared type is that scheme.
    pub fn into_mono(self) -> Result<crate::ast::Type<P>, Error> {
        Ok(self.into_mono_interned()?.clone_type())
    }

    pub(crate) fn into_mono_interned(self) -> Result<InternedType<P>, Error> {
        if self.is_scheme {
            let span = self.ty.span();
            return Err(scheme_in_mono_position(span));
        }
        Ok(self.ty)
    }

    pub(crate) fn with_canonical_identity(mut self) -> Self {
        self.ty = self.ty.with_canonical_identity();
        self
    }
}

// =========================================================================
// TyperPhase + Typer traits
// =========================================================================

/// Phases the typer can run against. Lowered has the elaboration-
/// bearing variants (`Expr::UserElaborator`, `Expr::Elaborator`)
/// inhabited; Prime has them all uninhabited.
///
/// `Elaborations` carries the per-phase completion side channel. At Lowered it
/// is the full `typecheck_full::Elaborations` table; at Prime it is the narrower
/// call-argument and checked-lambda completion table. Prime's later
/// statement-spine canonicalizer is a separate pass.
///
/// `Typer` is the per-phase dispatch type for the recursive
/// expression-synthesis driver. The non-elaboration arms of
/// `synth_expr` (Path, Call, FnExpr, Let, literals) live in
/// [`typecheck_core`] and are shared across phases via helpers
/// like [`synth_let`] / [`synth_fn`] / [`synth_call`] (which call
/// back into `T::synth_expr` for sub-expressions). The elaboration
/// arms — `Elaborator` / `UserElaborator` — are inhabited
/// only at Lowered; the Lowered `Typer` impl routes user elaborators through
/// the ordinary value cursor and field syntax to
/// `synth_field_access` / `synth_field_update`,
/// while the Prime impl discharges them via
/// `match *ext {}`.
pub trait TyperPhase:
    crate::ast::Phase<ExprResolved = (), ExpressionOccurrence = crate::ast::ExpressionOccurrence>
    + crate::pass::resolve::ResolvePhase
where
    Self: Sized,
{
    type Elaborations: Default;
    type Typer: Typer<Self>;

    fn merge_elaborations(target: &mut Self::Elaborations, source: Self::Elaborations);

    fn check_fn_capability_signature<'m>(
        _definition: &'m crate::ast::FnDef<Self>,
        _tcx: &TypeCtx<'m, '_, Self>,
    ) -> Result<(), Error>
    where
        Self: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    {
        Ok(())
    }
}

/// A solved call type argument plus its handle-local identity state.
/// The boundary rewrite consumes the state before producing structural Prime.
#[derive(Clone, Debug)]
pub(crate) struct RecordedTypeResolution<P>
where
    P: crate::ast::Phase,
{
    pub(crate) resolved: crate::ast::Type<P>,
    pub(crate) identity_canonical: bool,
}

impl<P> From<InternedType<P>> for RecordedTypeResolution<P>
where
    P: crate::ast::Phase + Clone,
{
    fn from(resolved: InternedType<P>) -> Self {
        Self {
            identity_canonical: resolved.identity_is_canonical(),
            resolved: resolved.clone_type(),
        }
    }
}

/// Per-phase synth-expr / check-against dispatcher. Implementations are
/// unit structs whose static methods route shared rules through the phase's
/// elaboration sink.
pub trait Typer<P>
where
    P: TyperPhase,
{
    /// Whether this phase publishes declaration identities for nominal type
    /// paths into an editor-facing position index.
    const RECORD_NOMINAL_TYPE_BINDERS: bool;

    /// Whether ordinary universal type arguments may be omitted and
    /// inferred at a call site. Surface Kio permits this local
    /// inference; Kio' requires the universal prefix to be explicit.
    /// The inference-driven existential suffix on a newtype
    /// constructor is a separate Kio' rule and remains admissible.
    const ALLOW_ELIDED_UNIVERSAL_CALL_TYPE_ARGS: bool;

    /// True when this phase accepts the multi-layer flat-call form
    /// per `specs/language.md` § 4c — one application that spans
    /// multiple consecutive curry layers (e.g. `foo(T1, v1, T2,
    /// v2)` against `Forall([A], Function(A, Forall([B],
    /// Function(b, Unit))))`). Kio surface accepts it; Kio' rejects
    /// it at type-check (the call-site dual of § 5's no-interleaving
    /// rule). Consulted by [`apply_mono`].
    const ALLOW_MULTI_LAYER_FLAT_CALL: bool;

    /// Walk an expression, synthesize its type. The four
    /// elaboration-bearing arms are dispatched per-phase; everything
    /// else (Path / Call / FnExpr / Let / literals / Unit) routes
    /// through the shared helpers in this module.
    fn synth_expr<'m>(
        e: &'m crate::ast::Expr<P>,
        tcx: &mut TypeCtx<'m, '_, P>,
    ) -> Result<Synth<P>, Error>;

    /// Synthesize one ordinary application through the goal-free,
    /// phase-polymorphic planner. The Lowered expression dispatcher handles
    /// surface calls before reaching this hook; retaining the generic hook
    /// keeps shared checking code phase-polymorphic without placing
    /// Lowered-only goal state in [`TyperPhase`] or `TypeCtx<Prime>`.
    fn synth_call<'m>(
        callee: &'m crate::ast::Expr<P>,
        args: &'m [crate::ast::CallArg<P>],
        site: crate::ast::ExpressionSite,
        tcx: &mut TypeCtx<'m, '_, P>,
    ) -> Result<Synth<P>, Error>
    where
        P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    {
        apply::synth_call(callee, args, site, tcx)
    }

    /// Check one ordinary application against its expected result through
    /// the goal-free phase-polymorphic planner. Lowered surface calls are
    /// intercepted by the expression dispatcher, where the expected result
    /// joins the same finite goal domain as retained value arguments.
    fn check_call_against_interned<'m>(
        callee: &'m crate::ast::Expr<P>,
        args: &'m [crate::ast::CallArg<P>],
        expected: &InternedType<P>,
        site: crate::ast::ExpressionSite,
        tcx: &mut TypeCtx<'m, '_, P>,
    ) -> Result<InternedType<P>, Error>
    where
        P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    {
        apply::check_call_against_interned(callee, args, expected, site, tcx)
    }

    /// Bidirectional check: verify `e` has type `expected`.
    /// Per-phase because the `Expr::FnExpr` bidirectional check,
    /// `Expr::UserElaborator` (`iso!` / `into!` / `onto!` / `align!` /
    /// `ease!`) and `Expr::Elaborator` (field access / update) paths all
    /// need elaboration-aware machinery at Lowered.
    fn check_value_against_interned<'m>(
        e: &'m crate::ast::Expr<P>,
        expected: &InternedType<P>,
        tcx: &mut TypeCtx<'m, '_, P>,
    ) -> Result<(), Error>;

    fn check_value_against<'m>(
        e: &'m crate::ast::Expr<P>,
        expected: &crate::ast::Type<P>,
        tcx: &mut TypeCtx<'m, '_, P>,
    ) -> Result<(), Error>
    where
        P: Clone,
    {
        Self::check_value_against_interned(e, &InternedType::fresh(expected.clone()), tcx)
    }

    fn check_user_elaborator_item<'m>(
        elaborator: &'m crate::ast::UserElaboratorDef<P>,
        item_index: usize,
        env: &ModuleEnv<'m, P>,
        elaborations: &mut P::Elaborations,
    ) -> Result<(), Error>;

    /// Record the typer's solved type at canonical slot
    /// `slot_index` of the call whose source span is `call_site`.
    /// Per-phase because the recording sink differs — at Lowered
    /// it's the `typecheck_full::Elaborations` table the
    /// `substitute` pass consults; at Prime it's the
    /// `PrimeElaborations` table the prime pipeline's bake pass
    /// consults. (`Type::Infer` is statically uninhabited at Prime,
    /// so the Prime recording covers only the `expr_to_type_arg`
    /// reclassification path and inference-driven existential
    /// constructor witnesses; the `_` placeholder path is
    /// Lowered-only.)
    fn record_type_resolution<'m>(
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
        resolved: InternedType<P>,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Mark the `(call_site, slot_index)` pair as a value-shaped
    /// expression supplied at a type-arg slot — the typer
    /// reclassified it via `expr_to_type_arg`, and the post-typer
    /// rewrite pass needs the marker to rewrite the slot as
    /// `CallArg::Type` rather than leaving the value-arg in place
    /// at the next slot. Per-phase because the recording sink
    /// differs — Lowered records into `typecheck_full::Elaborations`
    /// (consumed by `substitute`); Prime records into
    /// `PrimeElaborations` (consumed by the prime pipeline's bake
    /// pass).
    fn mark_value_at_type_slot<'m>(
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record that the call at `call_site` should be emitted as a
    /// nested call at the given canonical argument boundary. Full Kio
    /// uses this when it accepts a multi-layer flat call; Kio' input
    /// has no such surface allowance, so the Prime implementation is a
    /// no-op.
    fn record_call_split<'m>(
        call_site: crate::ast::ExpressionSite,
        split_after_slot: usize,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record the Unit value supplied by a syntactically empty source call at
    /// `slot_index`. A nonempty type-only packet never records one. Lowered
    /// records this for substitution; freshly parsed Kio' records the same
    /// canonical slot for its bake step.
    fn record_call_implicit_unit<'m>(
        call_site: crate::ast::ExpressionSite,
        slot_index: usize,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record the Lowered-only rewrite from one regular UFCS expression
    /// to the already-built ordinary prefix call that replaces it at the
    /// Lowered → Prime boundary. The shared application planner invokes
    /// this only after the application has completed, which lets a UFCS
    /// value argument wait for its expected type without re-synthesizing
    /// the callee or replaying the application.
    ///
    /// Prime has no inhabited `Expr::Ufcs`, so its implementation is
    /// unreachable.
    #[doc(hidden)]
    fn record_regular_ufcs_elaboration<'m>(
        original: &'m crate::ast::Expr<P>,
        normalized_call: crate::ast::Expr<P>,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Publish the typer's three-tier-resolved host type for the literal at
    /// `literal_site`. Lowered records the resolution in
    /// `typecheck_full::Elaborations` for substitution. Kio' literals already
    /// carry their mandatory annotation: Prime synthesis bypasses this hook,
    /// and Prime checking may invoke it but deliberately records nothing. The
    /// validated annotation remains on Prime and later backend IR; emission
    /// does not rerun front-end literal resolution.
    fn record_literal_resolution<'m>(
        literal_site: crate::ast::ExpressionSite,
        resolved: InternedType<P>,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record a checked lambda value-parameter type. The post-typer
    /// rewrite bakes this into `Expr::FnExpr` so Kio' output has enough
    /// annotations to rebuild without reusing typing context that only
    /// existed at the original call site.
    fn record_fn_param_type<'m>(
        fn_site: crate::ast::ExpressionSite,
        value_param_index: usize,
        resolved: InternedType<P>,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record an opaque per-path-segment binder resolution at
    /// `segment_span`. `kind` and `name` together identify the
    /// resolution target (e.g. `"local"`/`"x"`, `"fn"`/`"foo"`,
    /// `"newtype_member"`/`"Foo.mk_foo"`). Per-phase because the
    /// recording sink only exists at Lowered (the position-keyed
    /// index is `typecheck_full::Elaborations`'s field — the
    /// shared `synth_path` invokes this method on every binder
    /// resolution but the call is a no-op at Prime since the
    /// `kio-prime` binary processes input with no surface for an
    /// editor to query against).
    ///
    /// The implementation maps `(kind, name, parent, qualifier)`
    /// into the phase-specific `ResolvedBinder` variant
    /// (`Local`, `TypeParam`, `Fn`, `HostEnvFn`, …); `parent`
    /// carries the head segment's name for two-segment member
    /// paths (e.g. `Some("Foo")` on the `mk_foo` segment of
    /// `Foo.mk_foo`), `None` for the head segment. `qualifier` carries
    /// the leading module alias for the leaf of a three-segment path such as
    /// `provider.Foo.mk_foo`, whose immediate `parent` alone is not enough to
    /// recover the resolved module identity.
    fn record_binder_at<'m>(
        segment_span: Span,
        kind: ResolvedBinderKind,
        name: &str,
        parent: Option<&str>,
        qualifier: Option<&str>,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record the exact declaration identity selected for a nominal type-path
    /// head. Type paths are checked by the shared kind checker rather than
    /// `synth_path`, so they have a distinct publication hook from value-path
    /// binders. `declaring_module_path` is the resolved owner, never the
    /// consumer module or the source-written import alias.
    fn record_nominal_type_binder_at<'m>(
        segment_span: Span,
        kind: NominalTypeBinderKind,
        declaring_module_path: &str,
        name: &str,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record a constructor/projector occurrence at its exact terminal
    /// newtype identity. This differs from [`Self::record_binder_at`]'s
    /// spelling-shaped member hook when a written type head is an
    /// identity-preserving transparent alias: navigation for the head stays
    /// on that alias, while the member resolves to the terminal nominal's
    /// declaration.
    fn record_newtype_member_binder_at<'m>(
        segment_span: Span,
        declaring_module_path: &str,
        newtype: &str,
        member: &str,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record the declaration span of a local or type-parameter
    /// binder at its introduction site (the `let`, the `fn` value
    /// parameter, the `[A]` type-parameter binder). Keyed by the
    /// module-scoped `(module_path, name)` identity, so the LSP's
    /// go-to-definition resolves a use to the declaration the use
    /// refers to. Per-phase for the same reason as
    /// [`Self::record_binder_at`]: the position index lives at Lowered
    /// only, so the Prime impl no-ops.
    fn record_binder_decl_at<'m>(
        decl_span: Span,
        kind: ResolvedBinderKind,
        name: &str,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record the typer's resolved type for the expression at
    /// `expr_span` in the position-keyed index. The `synth_expr`
    /// dispatch site at Lowered's [`Typer`] impl records every
    /// synthesized expression's type here; the parallel check-mode
    /// arms in [`default_check_value_against`] (covering
    /// `Expr::Let` / `Expr::Seq`) mirror that recording so the
    /// position index covers check-mode-only paths too. Per-phase
    /// because the recording sink only exists at Lowered (Prime
    /// processes input with no surface for an editor to query
    /// against; the impl no-ops).
    fn record_position_type<'m>(
        expr_span: Span,
        resolved: InternedType<P>,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record the inferred type for an unannotated `let` binder.
    /// Lowered stores this in the LSP position index; Prime no-ops.
    fn record_inlay_let_type<'m>(
        name_span: Span,
        name: &str,
        resolved: crate::ast::Type<P>,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Record inferred type arguments for display at the end of the
    /// callee expression. Semantic substitution records use the call's
    /// occurrence identity; this hook retains source placement for editors.
    fn record_inlay_type_args<'m>(
        call_site: crate::ast::ExpressionSite,
        callee_span: Span,
        slot_offset: usize,
        resolved: Vec<crate::ast::Type<P>>,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    /// Discharge the module's deferred elaboration obligations.
    /// Called once at the end of each module's type-check, after every
    /// `fn` body has been typed but while the module's alias context
    /// (`env`) is still live. At Lowered this drains the
    /// `typecheck_full::Elaborations` deferred queue — producing each
    /// structural elaborator's Kio' glue off the type-checking critical
    /// path and gating it through the coherence check (a mismatch fails
    /// the build, returning an `Error`). Prime has no surface elaborator
    /// obligations, so its implementation no-ops even though its narrower
    /// publication table may contain call/lambda completions.
    ///
    /// Separating the type-deciding pass (eager, on the critical path)
    /// from glue production (deferred, here) is the load-bearing
    /// structure of the elaborator-type-rules feature: type-checking — and
    /// therefore diagnostics and IDE feedback — completes at type-rule
    /// speed, with the slower glue production batched at the module
    /// boundary.
    fn discharge_module_deferrals(
        env: &ModuleEnv<'_, P>,
        elaborations: &mut P::Elaborations,
    ) -> Result<(), Error>;
}

/// Phase-independent classification of what a path segment resolves
/// to. Implementations of [`Typer::record_binder_at`] map this into
/// their phase-specific `ResolvedBinder` representation (the
/// Lowered impl populates `typecheck_full::ResolvedBinder`; the
/// Prime impl no-ops, since position-indexing is Full-only).
///
/// One variant per resolution path in
/// [`crate::pass::typecheck_core::synth_path`]. New resolution paths in
/// `synth_path` land a matching variant here.
#[derive(Copy, Clone, Debug)]
pub enum ResolvedBinderKind {
    /// `let x = e;` or a `fn` value parameter. `name` is the
    /// binding's name.
    Local,
    /// A type parameter `[A]` introduced by a `fn`, `fn`, or
    /// `alias`. `name` is the parameter's name.
    TypeParam,
    /// A top-level `fn` declared in this module or imported from
    /// a sibling module via `import m(foo);`. `name` is the fn's
    /// identifier; the Lowered impl looks the source module up to
    /// fill in `module_path`.
    Fn,
    /// A top-level `fn` reached through a cross-module import
    /// (`import m(foo);`). `name` is the imported identifier;
    /// the Lowered impl resolves the source module via the
    /// per-module env's cross-module imports.
    CrossModuleFn,
    /// A host fn. `name` is the fn's identifier.
    HostEnvFn,
    /// One of the value intrinsics (`__left__`, `__right__`,
    /// `__either__`, `__pair__`, `__fst__`, `__snd__`,
    /// `__if_then_else__`, `__absurd__`).
    /// `name` is the intrinsic's identifier.
    Intrinsic,
    /// The head segment of a two-segment newtype path
    /// (`Foo.mk_foo`). `name` is the newtype's identifier.
    Newtype,
    /// A cross-module newtype, imported via `import m(Foo);`.
    CrossModuleNewtype,
    /// The `.<member>` tail of a two-segment newtype path.
    /// `name` is the member's identifier; `parent` carries the
    /// newtype's identifier.
    NewtypeMember,
    /// A qualified-import alias — the head segment of a
    /// `h.foo` reference where `import pkg/helper as h;` registered
    /// `h`. `name` is the alias.
    QualifiedImport,
    /// The `.<member>` tail of a qualified-import path. `name`
    /// is the member's identifier; `parent` carries the alias.
    QualifiedImportMember,
}

/// Declaration class selected for a type-position path head.
#[derive(Copy, Clone, Debug)]
pub enum NominalTypeBinderKind {
    TypeAlias,
    Newtype,
    HostType,
}

// `impl TyperPhase for Lowered` + `LoweredTyper` (Lowered's
// `Typer<Lowered>` impl) live in `crate::pass::typecheck_full`;
// `impl TyperPhase for Prime` + `PrimeTyper` (Prime's
// `Typer<Prime>` impl) live in `crate::prime::typer`. Each impl
// sits next to the phase-local publication table type the trait associates
// (the full surface table for Lowered, the narrow call/lambda table for Prime)
// and the per-phase
// elaboration arms each typer dispatches to (or discharges).

/// Shared phase-independent checking for both Lowered and Prime. After each
/// phase handles its own forms, this retains expected types for pending
/// recursive-order locals, checks literals and calls with the expected type,
/// checks sequences, compares complete schemes directly in expected `Forall`
/// slots, and otherwise synthesizes a monotype for equivalence checking.
/// Surface-bearing variants are statically uninhabited at Prime.
pub fn default_check_value_against<'m, P>(
    e: &'m crate::ast::Expr<P>,
    expected: &InternedType<P>,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<(), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    if let crate::ast::Expr::Path { segments, .. } = e
        && segments.len() == 1
        && tcx.lookup_pending_rec_order(segments[0].as_str()).is_some()
    {
        tcx.fill_pending_rec_order_interned(segments[0].as_str(), expected.clone());
        return Ok(());
    }
    // Literal in check mode — three-tier resolution with `expected`
    // supplying tier 2 (an expected type from the surrounding
    // position). `resolve_literal_against` invokes the phase publication hook:
    // Lowered records the selected type for substitution, while Prime's hook
    // is a no-op because Kio' already requires the annotation. The equiv-check
    // then catches a tier-1 annotation that disagrees with `expected`.
    if let Some((shape, annotation, span)) = literal_check_parts(e) {
        let resolved = synth::resolve_literal_against(shape, annotation, expected, e.site(), tcx)?;
        // Checking positions never reach the synthesis dispatch that records
        // position types, so a literal checked against an expected type — an
        // argument to a monomorphic callee, an annotated `let` initializer, a
        // function body — would have no entry of its own, and a hover over it
        // would report the nearest enclosing recorded expression instead.
        <P::Typer as Typer<P>>::record_position_type(span, resolved.clone(), tcx);
        let binders = tcx.in_scope_type_param_binders();
        return aliases::require_interned_type_equiv(
            &resolved,
            expected,
            span,
            &tcx.binder_alias_ctx(&binders),
        );
    }
    // Call in check mode — thread the expected type into
    // `apply_polymorphic_function`'s type-argument inference so a
    // binder reachable only through the callee's return type can
    // be solved per `specs/language.md` § Type system. The result
    // is still equality-checked against `expected` afterwards;
    // threading only feeds inference.
    if let crate::ast::Expr::Call {
        callee,
        args,
        meta: Meta { span, .. },
        ..
    } = e
    {
        let call_ty = <P::Typer as Typer<P>>::check_call_against_interned(
            callee,
            args,
            expected,
            e.site(),
            tcx,
        )?;
        // Same reason as the literal arm above: without this, a call nested in
        // a checking position has no entry, and hovering it answers with the
        // enclosing expression's type.
        <P::Typer as Typer<P>>::record_position_type(*span, call_ty.clone(), tcx);
        let binders = tcx.in_scope_type_param_binders();
        return aliases::require_interned_type_equiv(
            &call_ty,
            expected,
            *span,
            &tcx.binder_alias_ctx(&binders),
        );
    }
    // `value; body` (sequence) in check mode — same threading rule
    // as let, minus the binder: synth and discard the value (it must
    // be `.`), then check the body against `expected`.
    if let crate::ast::Expr::Seq { value, body, .. } = e {
        let value_ty = <P::Typer as Typer<P>>::synth_expr(value, tcx)?.into_mono()?;
        if !matches!(value_ty, crate::ast::Type::Unit { .. }) {
            return Err(Error::type_(
                value.span(),
                format!(
                    "expression-statement `e;` must have type `.`, got `{}`. Use `let _ = e;` \
                     to discard a non-unit value deliberately",
                    display_type(&value_ty)
                ),
            ));
        }
        <P::Typer as Typer<P>>::check_value_against_interned(body, expected, tcx)?;
        <P::Typer as Typer<P>>::record_position_type(e.span(), expected.clone(), tcx);
        return Ok(());
    }
    // Polytype-against-polytype slot per `specs/formal/elaboration.md`
    // § 5.2 (E-App value-arg checking): when the receiving parameter
    // slot is itself a polytype (rank-2-or-higher position) and the
    // value is a non-lambda expression whose synthesized type is also a
    // polytype — typically a path expression referring to a top-level
    // polymorphic `fn` — the typer checks structural equivalence
    // between the two polytypes directly. No instantiation, no
    // skolemization: both sides already carry the polytype shape, so
    // `type_equiv`'s alpha-equivalence rule on `Forall` decides
    // admissibility. This is the PJ-V-W-S "subsumption against an
    // already-polytype value" case (their `Eq` / `DSK` rule's
    // identity slice). The `synth_value_arg` /
    // `require_type_equiv` path below would reject such a value with
    // the canonical "polymorphic value used in a monomorphic
    // position" diagnostic — that diagnostic is for monotype
    // expected slots, not polytype slots, so we route here first.
    let binders = tcx.in_scope_type_param_binders();
    if matches!(
        aliases::unfold_and_qualify_state(
            expected.as_type(),
            &tcx.binder_alias_ctx(&binders),
            expected.identity_is_canonical(),
        )
        .0,
        crate::ast::Type::Forall { .. }
    ) {
        let synth = <P::Typer as Typer<P>>::synth_expr(e, tcx)?;
        return aliases::require_interned_type_equiv(
            &synth.ty,
            expected,
            e.span(),
            &tcx.binder_alias_ctx(&binders),
        );
    }
    let arg_ty = synth::synth_value_arg_interned(e, tcx)?;
    aliases::require_interned_type_equiv(
        &arg_ty,
        expected,
        e.span(),
        &tcx.binder_alias_ctx(&binders),
    )
}

/// Destructure a literal `Expr` into the `(shape, annotation, span)`
/// triple `resolve_literal_against` consumes. Returns `None` for any
/// non-literal expression.
fn literal_check_parts<P: crate::ast::Phase>(
    e: &crate::ast::Expr<P>,
) -> Option<(crate::ast::RoleShape, Option<&crate::ast::Type<P>>, Span)> {
    use crate::ast::{LitAnnotationExt, RoleShape};
    match e {
        crate::ast::Expr::StrLit {
            annotation, meta, ..
        } => Some((RoleShape::Str, annotation.as_type(), meta.span)),
        crate::ast::Expr::IntLit {
            annotation, meta, ..
        } => Some((RoleShape::Int, annotation.as_type(), meta.span)),
        crate::ast::Expr::FloatLit {
            annotation, meta, ..
        } => Some((RoleShape::Float, annotation.as_type(), meta.span)),
        crate::ast::Expr::BoolLit {
            annotation, meta, ..
        } => Some((RoleShape::Bool, annotation.as_type(), meta.span)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::error::Diagnostic;

    fn unit() -> Type {
        crate::ast::Type::Unit {
            meta: Meta::new(Span::new(0, 0)),
        }
    }

    fn bottom() -> Type {
        crate::ast::Type::Bottom {
            meta: Meta::new(Span::new(0, 0)),
        }
    }

    #[test]
    fn right_fold_sum_empty_is_bottom() {
        let span = Span::new(0, 0);
        let result: Type = right_fold_sum(&[], span);
        assert!(matches!(result, crate::ast::Type::Bottom { .. }));
    }

    #[test]
    fn right_fold_sum_single_passes_through() {
        let span = Span::new(0, 0);
        let u = unit();
        let result = right_fold_sum(std::slice::from_ref(&u), span);
        assert!(matches!(result, crate::ast::Type::Unit { .. }));
    }

    #[test]
    fn right_fold_sum_works_on_prime_too() {
        // Exercise the phase-polymorphic shape with `Type<Prime>` values. The
        // standalone Prime checker shares this constructor with the Lowered
        // checker; this test pins the generic route directly.
        use crate::ast::Prime;
        let span = Span::new(0, 0);
        let a: crate::ast::Type<Prime> = crate::ast::Type::Unit {
            meta: Meta::new(span),
        };
        let b: crate::ast::Type<Prime> = crate::ast::Type::Bottom {
            meta: Meta::new(span),
        };
        let result = right_fold_sum(&[a, b], span);
        match result {
            crate::ast::Type::Sum { left, right, .. } => {
                assert!(matches!(*left, crate::ast::Type::Unit { .. }));
                assert!(matches!(*right, crate::ast::Type::Bottom { .. }));
            }
            other => panic!("expected Sum, got {other:?}"),
        }
    }

    #[test]
    fn right_fold_sum_three_is_right_associative() {
        // [A, B, C] should fold to A | (B | C), not (A | B) | C.
        let span = Span::new(0, 0);
        let a = unit();
        let b = bottom();
        let c = unit();
        let result = right_fold_sum(&[a, b, c], span);
        match result {
            crate::ast::Type::Sum { left, right, .. } => {
                assert!(matches!(*left, crate::ast::Type::Unit { .. }));
                // The right side must itself be a Sum (B | C),
                // not just B.
                assert!(matches!(*right, crate::ast::Type::Sum { .. }));
            }
            other => panic!("expected Sum, got {other:?}"),
        }
    }

    #[test]
    fn display_type_renders_unit_and_bottom() {
        assert_eq!(display_type(&unit()), ".");
        assert_eq!(display_type(&bottom()), "!");
    }

    #[test]
    fn display_type_hides_internal_inference_holes() {
        let internal: Type =
            crate::ast::Type::synth_path(vec!["\0expected0".into()], Vec::new(), Span::new(0, 0));
        assert_eq!(display_type(&internal), "_");
    }

    #[test]
    fn display_type_renders_function_with_two_params() {
        let span = Span::new(0, 0);
        let f = crate::ast::Type::synth_function(vec![unit(), bottom()], unit(), span);
        assert_eq!(display_type(&f), "(. & !) -> .");
    }

    #[test]
    fn display_type_renders_product_param_independent_of_abi_arity() {
        let span = Span::new(0, 0);
        let product = crate::ast::Type::Product {
            left: Box::new(unit()),
            right: Box::new(bottom()),
            meta: Meta::new(span),
        };
        let one_param = crate::ast::Type::synth_function(vec![product], unit(), span);
        let two_params = crate::ast::Type::synth_function(vec![unit(), bottom()], unit(), span);

        assert_eq!(display_type(&one_param), "(. & !) -> .");
        assert_eq!(display_type(&two_params), "(. & !) -> .");
    }

    #[test]
    fn display_type_renders_product_and_sum_paren_wrapped() {
        let span = Span::new(0, 0);
        let prod = crate::ast::Type::Product {
            left: Box::new(unit()),
            right: Box::new(bottom()),
            meta: Meta::new(span),
        };
        assert_eq!(display_type(&prod), "(. & !)");
        let sum = crate::ast::Type::Sum {
            left: Box::new(unit()),
            right: Box::new(bottom()),
            meta: Meta::new(span),
        };
        assert_eq!(display_type(&sum), "(. | !)");
    }

    #[test]
    fn subst_type_replaces_single_segment_path() {
        let span = Span::new(0, 0);
        let mut subst = HashMap::new();
        subst.insert("A".into(), unit());
        // Substitute `A` with `.` inside `A -> A`.
        let ty = crate::ast::Type::synth_function(
            vec![crate::ast::Type::synth_path(
                vec!["A".into()],
                Vec::new(),
                span,
            )],
            crate::ast::Type::synth_path(vec!["A".into()], Vec::new(), span),
            span,
        );
        assert_eq!(display_type(&subst_type(&ty, &subst)), ". -> .");
    }

    #[test]
    fn subst_type_preserves_function_abi_arity() {
        let span = Span::new(0, 0);
        let mut subst = HashMap::new();
        subst.insert(
            "A".into(),
            crate::ast::Type::Product {
                left: Box::new(unit()),
                right: Box::new(unit()),
                meta: Meta::new(span),
            },
        );
        let ty = crate::ast::Type::synth_function(
            vec![crate::ast::Type::synth_path(
                vec!["A".into()],
                Vec::new(),
                span,
            )],
            unit(),
            span,
        );
        let substituted = subst_type(&ty, &subst);
        let crate::ast::Type::Function {
            param, abi_arity, ..
        } = substituted
        else {
            panic!("expected function type");
        };
        assert!(matches!(param.as_ref(), crate::ast::Type::Product { .. }));
        assert_eq!(abi_arity, 1);
        assert_eq!(crate::ast::Type::function_param_abi_slot_count(&param), 2);
    }

    #[test]
    fn subst_type_skips_qualified_paths() {
        // `m.Foo` is a qualified import — never a type-parameter
        // reference, so substitutions on its head never apply.
        let span = Span::new(0, 0);
        let mut subst = HashMap::new();
        subst.insert("m".into(), unit());
        let ty = crate::ast::Type::synth_path(vec!["m".into(), "Foo".into()], Vec::new(), span);
        assert_eq!(display_type(&subst_type(&ty, &subst)), "m.Foo");
    }

    #[test]
    fn subst_type_empty_substitution_is_identity() {
        let span = Span::new(0, 0);
        let ty: Type = crate::ast::Type::synth_path(vec!["a".into()], Vec::new(), span);
        let subst = HashMap::new();
        assert_eq!(display_type(&subst_type(&ty, &subst)), "a");
    }

    #[test]
    fn intrinsic_call_emits_path_callee_with_name() {
        let span = Span::new(0, 0);
        let call: Expr = intrinsic_call("__pair__", Vec::new(), span);
        match call {
            crate::ast::Expr::Call { callee, args, .. } => {
                assert!(args.is_empty());
                match *callee {
                    crate::ast::Expr::Path { segments, .. } => {
                        assert_eq!(segments, vec!["__pair__".to_owned()]);
                    }
                    other => panic!("expected Path callee, got {other:?}"),
                }
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn check_well_formed_type_accepts_well_formed_path() {
        let span = Span::new(0, 0);
        let ty = crate::ast::Type::synth_path(vec!["Foo".into()], vec![unit()], span);
        assert!(check_well_formed_type(&ty).is_ok());
    }

    #[test]
    fn scheme_in_mono_position_is_a_type_error() {
        let span = Span::new(0, 5);
        let err = scheme_in_mono_position(span);
        match err {
            Error::Type(Diagnostic {
                span: e_span,
                message,
                ..
            }) => {
                assert_eq!(e_span, span);
                assert!(message.contains("polymorphic"));
            }
            other => panic!("expected Type error, got {other:?}"),
        }
    }

    #[test]
    fn expr_to_type_arg_rejects_values_and_malformed_type_paths() {
        let span = Span::new(0, 0);
        // The Unit value never doubles as a Unit type argument.
        let unit_expr: Expr = crate::ast::Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(span),
        };
        let error = expr_to_type_arg(&unit_expr).expect_err("`()` is a value, not a type");
        assert!(matches!(error, Error::Type(_)));
        let path_expr = |name: &str| -> Expr {
            crate::ast::Expr::Path {
                occurrence: Default::default(),
                segments: vec![crate::ast::PathSegment::new(name, span)],
                meta: Meta::new(span),
                ext: (),
            }
        };
        for name in ["A", "_A", "__Type__", "__Foo"] {
            let ty = expr_to_type_arg(&path_expr(name)).expect("exact type-shaped path");
            assert_eq!(display_type(&ty), name);
        }
        for name in ["a", "_value"] {
            let error = expr_to_type_arg(&path_expr(name)).expect_err("value path in type slot");
            assert!(matches!(error, Error::Type(_)), "{name}: {error:?}");
        }
        for name in ["_FooBar", "_1Foo", "FooBar"] {
            let error = expr_to_type_arg(&path_expr(name)).expect_err("invalid type path");
            assert!(matches!(error, Error::Parse(_)), "{name}: {error:?}");
        }
    }

    #[test]
    fn literal_check_parts_recognizes_bool_literal() {
        // A bool literal is a role-`bool` literal; dropping its arm would
        // silently skip the annotation/role check for every `true` /
        // `false`.
        let e: crate::ast::Expr<crate::ast::Lowered> = crate::ast::Expr::BoolLit {
            occurrence: Default::default(),
            value: true,
            annotation: None,
            meta: Meta::new(Span::new(0, 0)),
        };
        assert!(matches!(
            literal_check_parts(&e),
            Some((crate::ast::RoleShape::Bool, _, _))
        ));
    }
}
