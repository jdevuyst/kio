//! Pure type helpers shared between [`typecheck_full`] and
//! [`crate::prime::typer`]. None of these touch the typer's
//! per-fn `TypeCtx` or per-module `ModuleEnv`; they operate
//! entirely on `Type<P>` and `Expr<P>` shapes plus phase-
//! agnostic pieces (`Span`, `Meta`, `Error`).
//!
//! Each helper is phase-polymorphic over the trait bounds it
//! genuinely needs — usually `<P: Phase>` (for fold / clause /
//! intrinsic builders that don't pattern-match on
//! `Type::LabelSugar`) or `<P: Phase<TypeLabelSugar = Never>>`
//! (for walkers that do — `subst_type`, `check_no_infer`,
//! `check_well_formed_type`, `write_type`).
//!
//! Extracted from [`super::typecheck_core`] so the umbrella file
//! stays smaller as the typer grows; the umbrellan exports
//! every public item so external consumers don't change.

use std::collections::HashMap;
#[cfg(feature = "surface")]
use std::collections::HashSet;

use crate::ast::Meta;
use crate::error::Error;
use crate::pass::resolve::{NominalDeclaration, NominalProvider, NominalSelection};
use crate::span::Span;

pub fn value_arg_looks_like_type_arg<P>(e: &crate::ast::Expr<P>) -> bool
where
    P: crate::ast::Phase,
{
    fn segment_starts_like_type(segment: &crate::ast::PathSegment) -> bool {
        crate::naming::starts_like_type_name(segment.name.as_str())
    }

    match e {
        crate::ast::Expr::Path { segments, .. } => {
            segments.last().is_some_and(segment_starts_like_type)
        }
        crate::ast::Expr::Call { callee, .. } => match callee.as_ref() {
            crate::ast::Expr::Path { segments, .. } => {
                segments.last().is_some_and(segment_starts_like_type)
            }
            _ => false,
        },
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallArgSlotKind {
    Type,
    Value,
}

pub fn call_arg_type_slot_candidate<P>(arg: &crate::ast::CallArg<P>) -> bool
where
    P: crate::ast::Phase,
{
    match arg {
        crate::ast::CallArg::Type(_) => true,
        crate::ast::CallArg::Value(value) => value_arg_looks_like_type_arg(value),
    }
}

pub fn call_arg_to_type_arg<P>(arg: &crate::ast::CallArg<P>) -> Result<crate::ast::Type<P>, Error>
where
    P: crate::ast::Phase + Clone,
{
    match arg {
        crate::ast::CallArg::Type(t) => Ok(t.clone()),
        crate::ast::CallArg::Value(e) => expr_to_type_arg(e),
    }
}

fn validate_reinterpreted_type_path(
    segments: &[crate::ast::PathSegment],
    fallback_span: Span,
) -> Result<(), Error> {
    let Some(leaf) = segments.last() else {
        return Err(Error::type_(fallback_span, "expected a type name"));
    };
    let name = leaf.name.as_str();
    if crate::naming::is_type_reference_name(name) {
        return Ok(());
    }
    if crate::naming::is_value_reference_name(name) {
        return Err(Error::type_(
            leaf.span,
            "expected a type argument here, got a value argument",
        ));
    }
    crate::naming::validate_reference_name(name, crate::naming::NameRole::Type, leaf.span)
}

/// Reinterpret a value-position expression as a type argument at a
/// call site's type-arg slot. Handles single- / multi-segment paths
/// (`a`, `m.Foo`) and parametric type applications (`Foo(A, B)`).
/// Everything else surfaces a kind error pinned to the offending
/// node's span. In particular, `()` is always the Unit value; the
/// explicit Unit type is `.`.
///
/// This is the typer's bridge between the parser's backtracking-
/// friendly "try expr first" call-arg dispatch and type-arg slots:
/// forms that look like values syntactically but belong as types
/// here get rewritten without a separate parser pass.
///
/// Phase-polymorphic — the matched variants (`Expr::Path`,
/// `Expr::Call`) exist on every phase, and surface-only variants fall
/// through to the catch-all kind error.
pub fn expr_to_type_arg<P>(e: &crate::ast::Expr<P>) -> Result<crate::ast::Type<P>, Error>
where
    P: crate::ast::Phase + Clone,
{
    match e {
        crate::ast::Expr::Path {
            segments,
            meta: Meta { span, .. },
            ..
        } => {
            validate_reinterpreted_type_path(segments, *span)?;
            Ok(crate::ast::Type::synth_path_segments(
                segments.clone(),
                Vec::new(),
                *span,
            ))
        }
        crate::ast::Expr::Call {
            callee,
            args,
            meta: Meta { span, .. },
            ..
        } => {
            // `Foo(A, B)` parses as `Call(Path[Foo], [Value(A), Value(B)])`
            // when the parser's expr-first attempt succeeds.
            // Reinterpret recursively: the callee must itself
            // reinterpret as a path-shaped type, and each
            // value-arg as a type-arg.
            let head = match callee.as_ref() {
                crate::ast::Expr::Path { segments, .. } => segments.clone(),
                _ => {
                    return Err(Error::type_(
                        callee.span(),
                        "expected a type argument here, got a value argument",
                    ));
                }
            };
            validate_reinterpreted_type_path(&head, callee.span())?;
            let mut type_args: Vec<crate::ast::Type<P>> = Vec::with_capacity(args.len());
            for a in args {
                match a {
                    crate::ast::CallArg::Type(t) => type_args.push(t.clone()),
                    crate::ast::CallArg::Value(v) => type_args.push(expr_to_type_arg(v)?),
                }
            }
            // Higher-kinded type application in a value-position
            // type-arg slot is the ordinary parametric `Type::Path`
            // (`F(A)`, `Either(String)`); the kind discipline decides
            // admissibility downstream.
            Ok(crate::ast::Type::synth_path_segments(
                head, type_args, *span,
            ))
        }
        other => Err(Error::type_(
            other.span(),
            "expected a type argument here, got a value argument",
        )),
    }
}

/// Walk a type and report whether any `Type::Infer` placeholder
/// appears anywhere in it. Phase-boundary substitution uses this
/// only as a mechanical assertion that every planned occurrence was
/// consumed; source annotation routing is decided by the single
/// annotation plan built during the ordinary type walk.
pub fn type_contains_infer<P>(ty: &crate::ast::Type<P>) -> bool
where
    P: crate::ast::Phase,
{
    let mut pending = vec![ty];
    while let Some(ty) = pending.pop() {
        match ty {
            crate::ast::Type::Infer { .. } => return true,
            crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => {}
            crate::ast::Type::Function { param, ret, .. } => {
                pending.push(ret);
                pending.push(param);
            }
            crate::ast::Type::Product { left, right, .. }
            | crate::ast::Type::Sum { left, right, .. } => {
                pending.push(right);
                pending.push(left);
            }
            crate::ast::Type::Path { args, .. } | crate::ast::Type::Goal { args, .. } => {
                pending.extend(args.iter().rev());
            }
            crate::ast::Type::Forall { body, .. } => pending.push(body),
            crate::ast::Type::LabelSugar { labels, .. } => pending.extend(
                labels
                    .iter()
                    .rev()
                    .filter_map(|label| label.payload.as_ref()),
            ),
        }
    }
    false
}

/// Whether `ty` still contains a transient inference goal.
///
/// Goal-bearing types are valid only inside one live [`super::goals::GoalStore`]
/// domain. Package-global interning, memoization, and every phase boundary use
/// this predicate to keep the open type local to that domain.
pub(crate) fn type_contains_goal<P>(ty: &crate::ast::Type<P>) -> bool
where
    P: crate::ast::Phase,
{
    match ty {
        crate::ast::Type::Goal { .. } => true,
        crate::ast::Type::Unit { .. }
        | crate::ast::Type::Bottom { .. }
        | crate::ast::Type::Infer { .. } => false,
        crate::ast::Type::Function { param, ret, .. } => {
            type_contains_goal(param) || type_contains_goal(ret)
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            type_contains_goal(left) || type_contains_goal(right)
        }
        crate::ast::Type::Path { args, .. } => args.iter().any(type_contains_goal),
        crate::ast::Type::Forall { body, .. } => type_contains_goal(body),
        crate::ast::Type::LabelSugar { labels, .. } => labels
            .iter()
            .any(|label| label.payload.as_ref().is_some_and(type_contains_goal)),
    }
}

/// Assert the internal contract that an open inference type has closed before
/// `boundary` consumes it.
#[cfg(feature = "surface")]
pub(crate) fn assert_goal_free<P>(ty: &crate::ast::Type<P>, boundary: &str)
where
    P: crate::ast::Phase,
{
    assert!(
        !type_contains_goal(ty),
        "an open type-inference goal reached {boundary}; close and zonk its owning domain first"
    );
}

/// Walk a type and reject any `Type::Infer` occurrence with the caller's
/// position-specific message. Source positions with admitted annotation or
/// call goals use their dedicated planners instead of this closed-type guard.
pub fn check_no_infer<P>(ty: &crate::ast::Type<P>, message: &str) -> Result<(), Error>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    let mut pending = vec![ty];
    while let Some(ty) = pending.pop() {
        match ty {
            crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => {}
            crate::ast::Type::Infer {
                meta: Meta { span, .. },
                ..
            } => return Err(Error::type_(*span, message)),
            crate::ast::Type::Function { param, ret, .. } => {
                pending.push(ret);
                pending.push(param);
            }
            crate::ast::Type::Product { left, right, .. }
            | crate::ast::Type::Sum { left, right, .. } => {
                pending.push(right);
                pending.push(left);
            }
            crate::ast::Type::Path { args, .. } | crate::ast::Type::Goal { args, .. } => {
                pending.extend(args.iter().rev());
            }
            crate::ast::Type::Forall { body, .. } => pending.push(body),
            crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferFreeType<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    ty: crate::ast::Type<P>,
}

impl<P> InferFreeType<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    pub fn new(ty: crate::ast::Type<P>, message: &str) -> Result<Self, Error> {
        check_no_infer(&ty, message)?;
        Ok(Self { ty })
    }

    #[cfg(feature = "surface")]
    pub(crate) fn new_trusted(ty: crate::ast::Type<P>) -> Self {
        debug_assert!(
            !type_contains_infer(&ty),
            "trusted infer-free type contained a Type::Infer placeholder"
        );
        Self { ty }
    }

    pub fn as_type(&self) -> &crate::ast::Type<P> {
        &self.ty
    }

    pub fn into_type(self) -> crate::ast::Type<P> {
        self.ty
    }
}

/// Walk a type and reject any well-formedness violation that does not depend
/// on a typer environment. Errors use the type-error category so callers keep
/// the normal static-diagnostic boundary.
pub fn check_well_formed_type<P>(ty: &crate::ast::Type<P>) -> Result<(), Error>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    match ty {
        crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => Ok(()),
        crate::ast::Type::Function { param, ret, .. } => {
            check_well_formed_type(param)?;
            check_well_formed_type(ret)
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            check_well_formed_type(left)?;
            check_well_formed_type(right)
        }
        crate::ast::Type::Path { args, .. } => {
            for a in args {
                check_well_formed_type(a)?;
            }
            Ok(())
        }
        crate::ast::Type::Goal { .. } => unreachable!(
            "check_well_formed_type accepts closed types; an open goal must be checked through its owning goal store"
        ),
        crate::ast::Type::Forall { body, .. } => check_well_formed_type(body),
        // Statically uninhabited per the `TypeLabelSugar = Never`
        // bound on `P`.
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        // `_` placeholders are surface-level inference markers, legal
        // only in fn signatures (`.(x: _, y) -> _ { … }`) and call
        // type-argument lists (`f(_, "")`). Top-level binding
        // positions (fn / host fn / exported fn /
        // host type / exported type / type alias /
        // newtype / labels / equiv) carry
        // public contracts and must be fully explicit. The fn-
        // signature and call-site paths handle `Type::Infer`
        // directly (bidirectional fn-parameter checking, the
        // application solver for call arguments)
        // and never route through `check_well_formed_type`; reaching this arm
        // means the placeholder appeared in a position where the
        // rule rejects it.
        crate::ast::Type::Infer {
            meta: Meta { span, .. },
            ..
        } => Err(Error::type_(
            *span,
            "`_` is not allowed here: top-level binding annotations must be explicit. \
                      `_` placeholders are legal only in `fn` signatures and call type-argument \
                      lists",
        )),
    }
}

/// Build the canonical "polymorphic value used in a monomorphic position"
/// error. This is for an occurrence whose declared slot requires a monotype;
/// complete polymorphic function schemes remain admissible as structural
/// values in slots whose declared type is that scheme.
///
/// Phase-independent — operates entirely on `Span` and the
/// non-parametrized `Error` enum.
pub fn scheme_in_mono_position(span: Span) -> Error {
    Error::type_(
        span,
        "polymorphic value used in a monomorphic position; \
                  apply it with type arguments first",
    )
}

/// Look up a same-package or cross-module newtype from a nominal
/// `Type::Path`'s segments. Returns `None` when the head doesn't
/// resolve to a newtype — either it's an unbound name (caught upstream
/// by the resolver), a host type, an alias, or a bound type-parameter.
///
/// Same-module newtypes are keyed by bare name (the module is implicit
/// — this env's module). An imported newtype is matched by its
/// identity-exact `(defining-module, name)` key: the bare head is
/// resolved through this module's imports first, so a bare `Box` and an
/// explicitly-qualified `dep.lib.Box` resolve to the same declaration,
/// while two distinct dependencies' `Box`es stay distinct.
///
/// Phase-polymorphic. Both `newtypes` and `cross_module_newtypes`
/// store `&'m Newtype<P>` references with identical structure.
pub fn lookup_newtype_by_name<'m, P>(
    env: &super::ModuleEnv<'m, P>,
    segments: &[crate::ast::PathSegment],
) -> Option<&'m crate::ast::Newtype<P>>
where
    P: crate::pass::resolve::ResolvePhase,
{
    match env.nominal_ref_identity(segments) {
        // The head qualifies to a concrete `(module, name)`. Route by
        // whether that module is this env's: same-module newtypes are
        // bare-keyed, imported ones by `(module, name)`. Routing by the
        // resolved module (not the bare leaf) keeps a same-module `Box`
        // and an imported `m.Box` distinct even when their leaves match.
        Some((module, name)) => {
            if module == env.module_path {
                env.newtypes.get(name.as_str()).copied()
            } else {
                env.cross_module_newtype(segments)
            }
        }
        // No module identity (type parameter, intrinsic, unresolved
        // name). A bare same-module newtype still resolves above via the
        // `module_declares_type_name` arm, so falling through to the
        // bare table here only covers ad-hoc envs with no package.
        None => env.newtypes.get(segments.last()?.as_str()).copied(),
    }
}

fn check_source_qualified_type_path<P>(
    env: &super::ModuleEnv<'_, P>,
    segments: &[crate::ast::PathSegment],
    selected: Option<&NominalDeclaration<'_, P>>,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ResolvePhase,
{
    let [qualifier, member] = segments else {
        return Ok(());
    };
    let Some(owner) = env.qualified_imports.get(qualifier.as_str()) else {
        return Ok(());
    };
    let spelling = format!("{}.{}", qualifier.as_str(), member.as_str());
    let owner_path = owner.path.segments.join("/");
    let not_a_type = || {
        Error::type_(
            member.span,
            format!("`{spelling}` does not name a type in module `{owner_path}`"),
        )
    };
    let Some(selected) = selected else {
        return Err(not_a_type());
    };
    if selected.declaration.host_type().is_some() || selected.declaration.newtype().is_some() {
        return Ok(());
    }
    let Some(alias) = selected.declaration.type_alias() else {
        return Err(not_a_type());
    };
    if owner.path == env.module.path
        || crate::pass::resolve::is_visible(&alias.vis, &env.module.path)
    {
        return Ok(());
    }

    let help = match &alias.vis {
        crate::ast::Visibility::Private => format!(
            "add `pub` to the declaration of `{}` in module `{owner_path}` to make `{spelling}` visible here",
            member.as_str()
        ),
        crate::ast::Visibility::PublicIn(path) => {
            let scope = path.segments.join("/");
            format!(
                "`{spelling}` is restricted to `pub({scope})`; use it only from that module subtree"
            )
        }
        crate::ast::Visibility::Public => {
            unreachable!("a public type alias is visible from every module")
        }
    };
    Err(Error::type_(
        member.span,
        format!(
            "type alias `{spelling}` is not visible from module `{}`",
            env.module_path
        ),
    )
    .with_help(help))
}

/// Compute the kind of a type expression — its type-level arity in
/// the closed kind language `κ ::= * | κ → κ` (see
/// [`specs/grammar.md` § Kind grammar]). A structural walk:
///
/// - **`Type::Path`** — look up the head's declared kind (a bound
///   type-parameter's annotated kind, a newtype's kind built
///   `k0 → k1 → … → *` from its binders' own kinds, a transparent alias's
///   binder kinds followed by its expanded body kind, or an ordinary
///   host type's declared arity. `Monad[*F]` is `(*→*)→*`; `List[A]` and
///   `host type Box[A]` are `*→*`. A
///   transparent alias must supply exactly one argument per binder; it
///   cannot remain partially applied. Then subtract one arrow per supplied
///   argument, checking each argument's kind against the arrow's domain.
///   Over-application, a domain/argument kind mismatch, or applying a
///   kind-`*` head is a kind error.
/// - **`Function` / `Product` / `Sum` / `Unit` / `Bottom`** — kind
///   `*` (saturated types). Their components are kind-checked by the
///   surrounding [`check_kinds_in_type`] walk, not here.
/// - **`Forall`** — the kind of its body, computed with the binder in
///   lexical scope.
///
/// Phase-polymorphic over [`TyperPhase`]; consults `tcx` for the
/// in-scope binder kinds and the newtype table.
pub(crate) trait InferPolicy {
    /// Observe one written source placeholder with the kind required by its
    /// exact structural position. Implementations may collect sparse planning
    /// facts, but must not allocate goals, owners, or publication state.
    fn observe(&mut self, span: Span, required: crate::ast::Kind) -> Result<(), Error>;

    /// Whether a higher-kinded `forall` frontier ending at a written source
    /// placeholder should continue into the annotation occurrence walk.
    /// The annotation planner alone opts in; every ordinary kind consumer
    /// preserves the established incomplete-scheme diagnostic.
    fn defers_source_placeholder_scheme_error(&self) -> bool {
        false
    }

    /// Observe the exact source-order entry into an embedded `forall` binder.
    /// The authoritative kind walk calls this only after the binder's scheme
    /// shape has been validated. Implementations may retain a symbolic
    /// descriptor for later, mechanical tooling publication; they must not
    /// mutate the type context here.
    fn enter_forall(&mut self, _param: &crate::ast::TypeParam) {}

    /// Balance one successful [`Self::enter_forall`] call, including when a
    /// descendant kind check returns an error.
    fn exit_forall(&mut self) {}

    /// Observe a path occurrence that the same kind walk has already proved
    /// denotes either its innermost embedded binder or an exact ambient
    /// binder. Nominal paths never reach this hook.
    fn observe_binder_reference(&mut self, _span: Span, _name: &str) {}
}

/// Borrowed exact binder evidence supplied by a caller-owned planning view.
/// Embedded `forall` binders remain on the kind walk's local stack; this
/// interface prevents a lambda header from cloning its complete signature
/// prefix merely to validate each annotation.
pub(crate) trait KindBinderLookup {
    fn kind_of(&self, name: &str) -> Option<crate::ast::Kind>;
    fn for_each(&self, visit: &mut dyn FnMut(&str));
}

struct NoKindBinders;

impl KindBinderLookup for NoKindBinders {
    fn kind_of(&self, _name: &str) -> Option<crate::ast::Kind> {
        None
    }

    fn for_each(&self, _visit: &mut dyn FnMut(&str)) {}
}

struct IgnoreInfer;

impl InferPolicy for IgnoreInfer {
    fn observe(&mut self, _: Span, _: crate::ast::Kind) -> Result<(), Error> {
        Ok(())
    }
}

pub fn compute_kind<P>(
    ty: &crate::ast::Type<P>,
    span: Span,
    tcx: &super::TypeCtx<'_, '_, P>,
) -> Result<crate::ast::Kind, Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let provider = NominalProvider::new(Some(tcx.env.module), tcx.env.package);
    let mut infer = IgnoreInfer;
    compute_kind_with_embedded_binders(
        ty,
        span,
        tcx,
        &mut Vec::new(),
        &NoKindBinders,
        &mut CompleteSchemeProofs::default(),
        super::kind_scheme::KindDemand::Head,
        &mut infer,
        None,
        false,
        &provider,
        provider.root(),
        true,
    )
}

/// Compute the root kind while also validating every nested type
/// application, without recording binder metadata.
///
/// Call sites that must validate a type before opening a transactional
/// publication use this entry point: unlike [`check_kinds_in_type`], it
/// borrows the type context immutably and therefore cannot publish partial
/// LSP metadata on failure.
pub(crate) fn compute_checked_kind<P>(
    ty: &crate::ast::Type<P>,
    span: Span,
    tcx: &super::TypeCtx<'_, '_, P>,
) -> Result<crate::ast::Kind, Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let provider = NominalProvider::new(Some(tcx.env.module), tcx.env.package);
    let mut infer = IgnoreInfer;
    compute_kind_with_embedded_binders(
        ty,
        span,
        tcx,
        &mut Vec::new(),
        &NoKindBinders,
        &mut CompleteSchemeProofs::default(),
        super::kind_scheme::KindDemand::Tree,
        &mut infer,
        None,
        false,
        &provider,
        provider.root(),
        true,
    )
}

pub(crate) fn check_annotation_kinds_with_policy_and_binders<P>(
    ty: &crate::ast::Type<P>,
    tcx: &super::TypeCtx<'_, '_, P>,
    outer_binders: &dyn KindBinderLookup,
    infer: &mut dyn InferPolicy,
) -> Result<(), Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let provider = NominalProvider::new(Some(tcx.env.module), tcx.env.package);
    let required = crate::ast::Kind::Star;
    let mut embedded_binders = Vec::new();
    let kind = compute_kind_with_embedded_binders(
        ty,
        ty.span(),
        tcx,
        &mut embedded_binders,
        outer_binders,
        &mut CompleteSchemeProofs::default(),
        super::kind_scheme::KindDemand::Tree,
        infer,
        Some(&required),
        false,
        &provider,
        provider.root(),
        true,
    );
    assert!(
        embedded_binders.is_empty(),
        "annotation kind analysis left an embedded binder open"
    );
    require_star_kind_for_value_type(kind?, ty.span())
}

/// Resolve the kind of a nominal head through the same declaration-identity
/// machinery used by [`compute_kind`].
///
pub(crate) enum NominalHeadBinding {
    /// Ordinary checking may resolve the current `TypeCtx` locals.
    AmbientOrEmbedded(Option<crate::ast::Kind>),
    /// A raw declaration body may resolve only binders introduced by that
    /// body/declaration. Caller `TypeCtx` locals are not in its lexical scope.
    EmbeddedOnly(Option<crate::ast::Kind>),
    /// Goal solving uses its proof-bearing rigid scope as the only binder
    /// authority. An absent binding must resolve to a nominal declaration.
    #[cfg(feature = "surface")]
    RigidScope(Option<crate::ast::Kind>),
}

#[cfg(all(test, feature = "surface"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NominalHeadResolutionWork {
    raw_owner_item_visits: usize,
}

#[cfg(all(test, feature = "surface"))]
thread_local! {
    static NOMINAL_HEAD_RESOLUTION_WORK: std::cell::Cell<NominalHeadResolutionWork> =
        const { std::cell::Cell::new(NominalHeadResolutionWork {
            raw_owner_item_visits: 0,
        }) };
}

#[cfg(all(test, feature = "surface"))]
pub(super) fn reset_nominal_head_resolution_work() {
    NOMINAL_HEAD_RESOLUTION_WORK.with(|work| work.set(NominalHeadResolutionWork::default()));
    crate::pass::resolve::reset_nominal_provider_work();
}

#[cfg(all(test, feature = "surface"))]
fn nominal_head_resolution_work() -> NominalHeadResolutionWork {
    let mut work = NOMINAL_HEAD_RESOLUTION_WORK.with(std::cell::Cell::get);
    work.raw_owner_item_visits +=
        crate::pass::resolve::nominal_provider_work().declaration_items_indexed;
    work
}

#[cfg(feature = "surface")]
pub(crate) fn resolve_nominal_head_kind<P>(
    segments: &[crate::ast::PathSegment],
    supplied_args: usize,
    span: Span,
    tcx: &super::TypeCtx<'_, '_, P>,
    binding: NominalHeadBinding,
    identity_canonical: bool,
) -> Result<crate::ast::Kind, Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let provider = NominalProvider::new(Some(tcx.env.module), tcx.env.package);
    resolve_nominal_head_kind_with_provider(
        segments,
        supplied_args,
        span,
        tcx,
        binding,
        identity_canonical,
        &provider,
        provider.root(),
        true,
    )
}

#[allow(clippy::too_many_arguments)] // one exact head query plus its validated provider/scope authority
fn resolve_nominal_head_kind_with_provider<'m, P>(
    segments: &[crate::ast::PathSegment],
    supplied_args: usize,
    span: Span,
    tcx: &super::TypeCtx<'m, '_, P>,
    binding: NominalHeadBinding,
    identity_canonical: bool,
    provider: &NominalProvider<'m, P>,
    nominal_scope: crate::pass::resolve::NominalScope<'m, P>,
    validate_source_path: bool,
) -> Result<crate::ast::Kind, Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    use crate::ast::Kind;

    let exact_identity = identity_canonical && segments.len() > 1;
    let kind_from_params = |params: &[crate::ast::TypeParam], result: Kind| -> Kind {
        params.iter().rev().fold(result, |acc, tp| {
            Kind::Arrow(Box::new(tp.effective_kind()), Box::new(acc))
        })
    };
    let (bound_kind, allow_ambient, require_nominal) = match binding {
        NominalHeadBinding::AmbientOrEmbedded(kind) => (kind, true, false),
        NominalHeadBinding::EmbeddedOnly(kind) => (kind, false, true),
        #[cfg(feature = "surface")]
        NominalHeadBinding::RigidScope(kind) => (kind, false, true),
    };
    let type_param_kind = (bound_kind.is_none() && allow_ambient && segments.len() == 1)
        .then(|| tcx.type_param_kind(segments[0].as_str()))
        .flatten();
    let selected = (bound_kind.is_none() && type_param_kind.is_none())
        .then(|| provider.select(nominal_scope, segments, exact_identity));
    let selected_item = selected.and_then(|selection| match selection {
        NominalSelection::Selected(declaration) => Some(declaration),
        NominalSelection::Missing | NominalSelection::Opaque => None,
    });
    if validate_source_path && !exact_identity {
        check_source_qualified_type_path(tcx.env, segments, selected_item.as_ref())?;
    }
    let newtype = selected_item
        .as_ref()
        .and_then(|selected| selected.declaration.newtype());
    let alias = selected_item.as_ref().and_then(|selected| {
        selected.declaration.type_alias().map(|alias| {
            (
                super::aliases::AliasDef {
                    type_params: &alias.type_params,
                    body: alias.type_body(),
                    owner_module: selected.owner.module(),
                },
                selected.owner,
            )
        })
    });
    let host_type = selected_item
        .as_ref()
        .and_then(|selected| selected.declaration.host_type());
    let comptime_in_scope = if validate_source_path {
        tcx.env.comptime_in_scope
    } else {
        nominal_scope.module().is_some_and(|module| {
            module
                .imports
                .iter()
                .any(|import_| matches!(import_.kind, crate::ast::ImportKind::Comptime))
        })
    };
    let comptime_type = (bound_kind.is_none()
        && type_param_kind.is_none()
        && newtype.is_none()
        && alias.is_none()
        && host_type.is_none()
        && !exact_identity
        && segments.len() == 1
        && comptime_in_scope)
        .then(|| crate::comptime::ComptimeBuiltin::from_public_name(segments[0].as_str()))
        .flatten()
        .filter(|builtin| builtin.is_type_name());
    if let Some((alias, _)) = alias
        && supplied_args != alias.type_params.len()
    {
        let required = alias.type_params.len();
        let required_noun = if required == 1 {
            "type argument"
        } else {
            "type arguments"
        };
        let supplied_verb = if supplied_args == 1 { "was" } else { "were" };
        return Err(Error::type_(
            span,
            format!(
                "type alias `{}` requires {required} {required_noun}, but \
                 {supplied_args} {supplied_verb} supplied",
                segments.last().expect("non-empty type path").as_str(),
            ),
        )
        .with_help(
            "supply every alias argument, or use a `newtype` when this declaration \
             itself must be partially applied",
        ));
    }
    Ok(if let Some(kind) = bound_kind {
        kind
    } else if let Some(kind) = type_param_kind {
        kind
    } else if let Some(newtype) = newtype {
        kind_from_params(&newtype.type_params, Kind::Star)
    } else if let Some((alias, alias_scope)) = alias {
        let mut alias_binders = alias
            .type_params
            .iter()
            .map(|param| (param.name.as_str(), param.effective_kind()))
            .collect();
        let mut infer = IgnoreInfer;
        let result = compute_kind_with_embedded_binders(
            alias.body,
            alias.body.span(),
            tcx,
            &mut alias_binders,
            &NoKindBinders,
            &mut CompleteSchemeProofs::default(),
            super::kind_scheme::KindDemand::Head,
            &mut infer,
            None,
            false,
            provider,
            alias_scope,
            false,
        )?;
        kind_from_params(alias.type_params, result)
    } else if let Some(host_type) = host_type.filter(|host_type| {
        host_type
            .type_params
            .iter()
            .all(|param| param.effective_kind() == Kind::Star)
    }) {
        Kind::arrow_chain(host_type.type_params.len())
    } else if comptime_type.is_some() {
        Kind::Star
    } else if !require_nominal {
        // A head without an ordinary host-parameter kind available to this
        // walk accepts exactly the supplied kind-`*` arguments so their
        // nested kinds can still be validated. A bare reference has kind `*`.
        Kind::arrow_chain(supplied_args)
    } else {
        return Err(Error::type_(
            span,
            format!(
                "type `{}` is neither a rigid binder in this inference scope nor a declared nominal type",
                segments
                    .iter()
                    .map(crate::ast::PathSegment::as_str)
                    .collect::<Vec<_>>()
                    .join(".")
            ),
        ));
    })
}

struct KindBinderView<'a, 'b, 'm, 'e, P>
where
    P: super::TyperPhase,
{
    tcx: &'a super::TypeCtx<'m, 'e, P>,
    embedded: &'a [(&'a str, crate::ast::Kind)],
    outer: &'b dyn KindBinderLookup,
    include_ambient: bool,
}

use super::kind_scheme::CompleteSchemeProofs;

impl<P> super::aliases::AliasBinderLookup for KindBinderView<'_, '_, '_, '_, P>
where
    P: super::TyperPhase,
{
    fn contains_alias_binder(&self, name: &str) -> bool {
        self.embedded.iter().rev().any(|(bound, _)| *bound == name)
            || self.outer.kind_of(name).is_some()
            || (self.include_ambient && self.tcx.contains_alias_binder(name))
    }

    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
        if self.include_ambient {
            self.tcx.for_each_alias_binder(visit);
        }
        self.outer.for_each(visit);
        for (name, _) in self.embedded {
            visit(name);
        }
    }
}

#[allow(clippy::too_many_arguments)] // the singular scheme analyzer threads exact binder and proof views
fn complete_function_scheme_summary<'a, 'm, 'e, P>(
    ty: &'a crate::ast::Type<P>,
    tcx: &'a super::TypeCtx<'m, 'e, P>,
    embedded: &'a [(&'a str, crate::ast::Kind)],
    outer: &dyn KindBinderLookup,
    identity_canonical: bool,
    provider: &NominalProvider<'m, P>,
    nominal_scope: crate::pass::resolve::NominalScope<'m, P>,
    include_ambient: bool,
    proofs: &mut CompleteSchemeProofs,
) -> super::kind_scheme::CompleteScheme
where
    'm: 'a,
    P: super::TyperPhase
        + crate::pass::resolve::ExportContractPhase
        + crate::ast::Phase<TypeLabelSugar = crate::ast::Never>
        + Clone,
{
    #[cfg(test)]
    super::kind_scheme::record_source_kind_consumer();
    if proofs.contains(ty) {
        return super::kind_scheme::CompleteScheme::Complete;
    }
    let binders = KindBinderView {
        tcx,
        embedded,
        outer,
        include_ambient,
    };
    let analysis = if include_ambient {
        let alias_ctx = tcx.env.alias_ctx();
        super::aliases::analyze_complete_function_scheme_in_scope_with_binder_lookup(
            ty,
            &alias_ctx,
            &binders,
            identity_canonical,
        )
    } else {
        super::aliases::analyze_complete_function_scheme_in_nominal_scope(
            ty,
            provider,
            nominal_scope,
            &binders,
            identity_canonical,
        )
    };
    proofs.extend_raw(analysis.proved_bare_spines);
    analysis.summary
}

#[allow(clippy::too_many_arguments)] // one binder gate shares the kind walk's explicit authority and proof state
fn require_admissible_forall_binder<'a, 'm, P>(
    scheme: &'a crate::ast::Type<P>,
    param: &'a crate::ast::TypeParam,
    span: Span,
    tcx: &'a super::TypeCtx<'m, '_, P>,
    embedded_binders: &'a [(&'a str, crate::ast::Kind)],
    outer_binders: &dyn KindBinderLookup,
    identity_canonical: bool,
    provider: &NominalProvider<'m, P>,
    nominal_scope: crate::pass::resolve::NominalScope<'m, P>,
    include_ambient: bool,
    infer: &dyn InferPolicy,
    proofs: &mut CompleteSchemeProofs,
) -> Result<(), Error>
where
    'm: 'a,
    P: super::TyperPhase
        + crate::pass::resolve::ExportContractPhase
        + crate::ast::Phase<TypeLabelSugar = crate::ast::Never>
        + Clone,
{
    let summary = complete_function_scheme_summary(
        scheme,
        tcx,
        embedded_binders,
        outer_binders,
        identity_canonical,
        provider,
        nominal_scope,
        include_ambient,
        proofs,
    );
    if param.effective_kind() == crate::ast::Kind::Star
        || summary.is_complete()
        || (matches!(
            summary,
            super::kind_scheme::CompleteScheme::SourcePlaceholder
        ) && infer.defers_source_placeholder_scheme_error())
    {
        return Ok(());
    }
    Err(Error::type_(
        span,
        format!(
            "higher-kinded `Forall` binder `{}` must bind a complete function scheme \
             (see `specs/prime.md` § Higher-kinded types)",
            param.name,
        ),
    ))
}

fn require_star_kind_for_value_type(kind: crate::ast::Kind, span: Span) -> Result<(), Error> {
    if kind == crate::ast::Kind::Star {
        return Ok(());
    }
    Err(Error::type_(
        span,
        format!("value type requires kind `*`, found kind `{kind}`"),
    ))
}

#[allow(clippy::too_many_arguments)] // recursive kind analysis carries one explicit walk state without a second analyzer
fn compute_kind_with_embedded_binders<'a, 'm, P>(
    ty: &'a crate::ast::Type<P>,
    span: Span,
    tcx: &'a super::TypeCtx<'m, '_, P>,
    embedded_binders: &mut Vec<(&'a str, crate::ast::Kind)>,
    outer_binders: &dyn KindBinderLookup,
    proofs: &mut CompleteSchemeProofs,
    demand: super::kind_scheme::KindDemand,
    infer: &mut dyn InferPolicy,
    required_kind: Option<&crate::ast::Kind>,
    identity_canonical: bool,
    provider: &NominalProvider<'m, P>,
    nominal_scope: crate::pass::resolve::NominalScope<'m, P>,
    validate_source_path: bool,
) -> Result<crate::ast::Kind, Error>
where
    'm: 'a,
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    use crate::ast::Kind;
    match ty {
        crate::ast::Type::Function { param, ret, .. } => {
            if demand.validates_tree() {
                let param_kind = compute_kind_with_embedded_binders(
                    param,
                    param.span(),
                    tcx,
                    embedded_binders,
                    outer_binders,
                    proofs,
                    demand,
                    infer,
                    Some(&Kind::Star),
                    identity_canonical,
                    provider,
                    nominal_scope,
                    validate_source_path,
                )?;
                require_star_kind_for_value_type(param_kind, param.span())?;
                let ret_kind = compute_kind_with_embedded_binders(
                    ret,
                    ret.span(),
                    tcx,
                    embedded_binders,
                    outer_binders,
                    proofs,
                    demand,
                    infer,
                    Some(&Kind::Star),
                    identity_canonical,
                    provider,
                    nominal_scope,
                    validate_source_path,
                )?;
                require_star_kind_for_value_type(ret_kind, ret.span())?;
            }
            Ok(Kind::Star)
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            if demand.validates_tree() {
                let left_kind = compute_kind_with_embedded_binders(
                    left,
                    left.span(),
                    tcx,
                    embedded_binders,
                    outer_binders,
                    proofs,
                    demand,
                    infer,
                    Some(&Kind::Star),
                    identity_canonical,
                    provider,
                    nominal_scope,
                    validate_source_path,
                )?;
                require_star_kind_for_value_type(left_kind, left.span())?;
                let right_kind = compute_kind_with_embedded_binders(
                    right,
                    right.span(),
                    tcx,
                    embedded_binders,
                    outer_binders,
                    proofs,
                    demand,
                    infer,
                    Some(&Kind::Star),
                    identity_canonical,
                    provider,
                    nominal_scope,
                    validate_source_path,
                )?;
                require_star_kind_for_value_type(right_kind, right.span())?;
            }
            Ok(Kind::Star)
        }
        crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => Ok(Kind::Star),
        crate::ast::Type::Infer { meta, .. } => {
            let required = required_kind.cloned().unwrap_or(Kind::Star);
            infer.observe(meta.span, required.clone())?;
            Ok(required)
        }
        crate::ast::Type::Goal { .. } => unreachable!(
            "compute_kind requires the goal store for an open Type::Goal; \
             close or use the goal-aware kind checker first"
        ),
        scheme @ crate::ast::Type::Forall {
            param,
            body,
            meta: Meta {
                span: forall_span, ..
            },
        } => {
            require_admissible_forall_binder(
                scheme,
                param,
                *forall_span,
                tcx,
                embedded_binders,
                outer_binders,
                identity_canonical,
                provider,
                nominal_scope,
                validate_source_path,
                infer,
                proofs,
            )?;
            infer.enter_forall(param);
            embedded_binders.push((param.name.as_str(), param.effective_kind()));
            let body_span = match demand {
                super::kind_scheme::KindDemand::Head => span,
                super::kind_scheme::KindDemand::Tree => body.span(),
            };
            let result = compute_kind_with_embedded_binders(
                body,
                body_span,
                tcx,
                embedded_binders,
                outer_binders,
                proofs,
                demand,
                infer,
                required_kind,
                identity_canonical,
                provider,
                nominal_scope,
                validate_source_path,
            );
            embedded_binders.pop();
            infer.exit_forall();
            let kind = result?;
            if demand.validates_tree() {
                require_star_kind_for_value_type(kind, body.span())?;
                Ok(Kind::Star)
            } else {
                Ok(kind)
            }
        }
        crate::ast::Type::Path { segments, args, .. } => {
            let embedded_kind = if segments.len() == 1 {
                embedded_binders
                    .iter()
                    .rev()
                    .find_map(|(name, kind)| (*name == segments[0].as_str()).then(|| kind.clone()))
                    .or_else(|| outer_binders.kind_of(segments[0].as_str()))
            } else {
                None
            };
            let head_kind = resolve_nominal_head_kind_with_provider(
                segments,
                args.len(),
                span,
                tcx,
                if validate_source_path {
                    NominalHeadBinding::AmbientOrEmbedded(embedded_kind.clone())
                } else {
                    NominalHeadBinding::EmbeddedOnly(embedded_kind.clone())
                },
                identity_canonical,
                provider,
                nominal_scope,
                validate_source_path,
            )?;
            if segments.len() == 1
                && (embedded_kind.is_some()
                    || (validate_source_path
                        && tcx.type_param_kind(segments[0].as_str()).is_some()))
            {
                infer.observe_binder_reference(segments[0].span, segments[0].as_str());
            }
            // Subtract one arrow per supplied argument; each argument
            // must match the consumed arrow's domain kind (so a
            // kind-`*→*` brand slot admits a kind-`*→*` argument, and
            // an ordinary kind-`*` slot admits only kind-`*`).
            let mut remaining = head_kind;
            for arg in args {
                let (domain, codomain) = match remaining {
                    Kind::Arrow(domain, codomain) => (*domain, *codomain),
                    Kind::Star => {
                        return Err(Error::type_(
                            span,
                            format!(
                                "`{}` has kind `*` and cannot be applied to a type argument \
                                 (see `specs/grammar.md` § Kind grammar)",
                                segments
                                    .iter()
                                    .map(|s| s.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join("."),
                            ),
                        ));
                    }
                };
                let arg_span = match demand {
                    super::kind_scheme::KindDemand::Head => span,
                    super::kind_scheme::KindDemand::Tree => arg.span(),
                };
                let arg_kind = compute_kind_with_embedded_binders(
                    arg,
                    arg_span,
                    tcx,
                    embedded_binders,
                    outer_binders,
                    proofs,
                    demand,
                    infer,
                    Some(&domain),
                    identity_canonical,
                    provider,
                    nominal_scope,
                    validate_source_path,
                )?;
                if arg_kind != domain {
                    return Err(Error::type_(
                        span,
                        format!(
                            "type application argument `{}` has kind `{arg_kind}`, but a \
                             kind-`{domain}` argument is required here \
                             (see `specs/grammar.md` § Kind grammar)",
                            display_type(arg),
                        ),
                    ));
                }
                remaining = codomain;
            }
            Ok(remaining)
        }
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
    }
}

/// Kind-check a user-written type: a structural walk that verifies
/// every type application is kind-consistent and that a higher-kinded
/// `Forall` forms a complete function scheme. The kind discipline (see
/// [`specs/grammar.md` § Kind grammar] and [`specs/prime.md`]
/// § Higher-kinded types) is the sole admissibility gate for
/// higher-kinded application and brand crossing.
///
/// Must run after fn parameters / newtype binders are pushed into
/// `tcx.locals` (with their kinds, via [`super::TypeCtx::push_type_param_kinded`])
/// so the per-application kind lookup resolves. Embedded `Forall`
/// binders retain their explicit kinds while their scheme shape is checked.
///
/// Phase-polymorphic with the usual `TypeLabelSugar = Never`
/// bound so the surface-only `Type::LabelSugar` arm is statically
/// uninhabited.
pub fn check_kinds_in_type<'m, P>(
    ty: &'m crate::ast::Type<P>,
    tcx: &mut super::TypeCtx<'m, '_, P>,
) -> Result<(), Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    check_kinds_in_type_with_binder_recorder(ty, tcx, record_type_binder::<P>)
}

pub(crate) fn check_kinds_in_type_with_provider<'m, P>(
    ty: &'m crate::ast::Type<P>,
    tcx: &mut super::TypeCtx<'m, '_, P>,
    provider: &NominalProvider<'m, P>,
) -> Result<(), Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let kind = compute_checked_kind_with_provider(ty, ty.span(), tcx, provider)?;
    require_star_kind_for_value_type(kind, ty.span())?;
    let mut recorder = record_type_binder::<P>;
    publish_checked_type_binders(ty, tcx, provider, &mut recorder);
    Ok(())
}

/// Kind-check the callable spine of a named `fn` or `elab` declaration.
///
/// A declaration may introduce type binders again after a value-parameter
/// layer. Those binders are still declaration binders, so they may carry a
/// higher kind. Function parameters and the terminal result are runtime value
/// types and therefore use the ordinary embedded-type rules.
#[cfg(feature = "surface")]
pub(crate) fn check_callable_spine_kinds<'m, P>(
    ty: &'m crate::ast::Type<P>,
    tcx: &mut super::TypeCtx<'m, '_, P>,
) -> Result<(), Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    match ty {
        crate::ast::Type::Forall { param, body, .. } => {
            let mark = tcx.save();
            tcx.push_type_param_kinded(param.name.as_str(), param.effective_kind(), param.span);
            record_type_binder::<P>(
                param.span,
                param.name.as_str(),
                TypeBinderRecordKind::Declaration,
                tcx,
            );
            let result = check_callable_spine_kinds(body, tcx);
            tcx.restore(mark);
            result
        }
        crate::ast::Type::Function { param, ret, .. } => {
            check_kinds_in_type(param, tcx)?;
            check_callable_spine_kinds(ret, tcx)
        }
        terminal => check_kinds_in_type(terminal, tcx),
    }
}

pub(crate) fn check_kind_tree_with_provider<'m, P>(
    ty: &'m crate::ast::Type<P>,
    tcx: &mut super::TypeCtx<'m, '_, P>,
    provider: &NominalProvider<'m, P>,
) -> Result<crate::ast::Kind, Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let kind = compute_checked_kind_with_provider(ty, ty.span(), tcx, provider)?;
    let mut recorder = record_type_binder::<P>;
    publish_checked_type_binders(ty, tcx, provider, &mut recorder);
    Ok(kind)
}

fn record_type_binder<'m, 'e, P>(
    span: Span,
    name: &str,
    record_kind: TypeBinderRecordKind,
    tcx: &mut super::TypeCtx<'m, 'e, P>,
) where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    match record_kind {
        TypeBinderRecordKind::Reference => {
            <P::Typer as super::Typer<P>>::record_binder_at(
                span,
                super::ResolvedBinderKind::TypeParam,
                name,
                None,
                None,
                tcx,
            );
        }
        TypeBinderRecordKind::Declaration => {
            <P::Typer as super::Typer<P>>::record_binder_decl_at(
                span,
                super::ResolvedBinderKind::TypeParam,
                name,
                tcx,
            );
        }
    }
}

/// Publish binder declarations/references from a type whose kind and exact
/// lexical interpretation were already validated.
/// This pass is deliberately mechanical: it performs no kind, alias, scheme,
/// readiness, or compatibility decision.
pub(crate) fn publish_validated_type_binders<'m, P>(
    ty: &crate::ast::Type<P>,
    tcx: &mut super::TypeCtx<'m, '_, P>,
) where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let provider = NominalProvider::new(Some(tcx.env.module), tcx.env.package);
    let mut recorder = record_type_binder::<P>;
    publish_checked_type_binders(ty, tcx, &provider, &mut recorder);
}

#[cfg(feature = "surface")]
pub(crate) fn stage_validated_nominal_type_binders(
    ty: &crate::ast::Type<crate::ast::Lowered>,
    tcx: &super::TypeCtx<'_, '_, crate::ast::Lowered>,
    publication: &mut crate::pass::typecheck_full::publication::PublicationBuilder,
) {
    let provider = NominalProvider::new(Some(tcx.env.module), tcx.env.package);
    let mut records = Vec::new();
    collect_nominal_type_binders(ty, tcx, &provider, &mut Vec::new(), &mut records);
    for record in records {
        publication.record_position_binder(
            record.span,
            crate::pass::typecheck_full::resolved_nominal_type_binder(
                record.kind,
                &record.module_path,
                &record.name,
            ),
        );
    }
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum TypeBinderRecordKind {
    Reference,
    Declaration,
}

pub(crate) fn check_kinds_in_type_with_binder_recorder<'m, 'e, P>(
    ty: &'m crate::ast::Type<P>,
    tcx: &mut super::TypeCtx<'m, 'e, P>,
    mut record_binder: impl FnMut(Span, &str, TypeBinderRecordKind, &mut super::TypeCtx<'m, 'e, P>),
) -> Result<(), Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let kind = check_kind_tree_with_binder_recorder(ty, tcx, &mut record_binder)?;
    require_star_kind_for_value_type(kind, ty.span())?;
    Ok(())
}

fn check_kind_tree_with_binder_recorder<'m, 'e, P>(
    ty: &'m crate::ast::Type<P>,
    tcx: &mut super::TypeCtx<'m, 'e, P>,
    mut record_binder: impl FnMut(Span, &str, TypeBinderRecordKind, &mut super::TypeCtx<'m, 'e, P>),
) -> Result<crate::ast::Kind, Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let provider = NominalProvider::new(Some(tcx.env.module), tcx.env.package);
    let kind = compute_checked_kind_with_provider(ty, ty.span(), tcx, &provider)?;
    publish_checked_type_binders(ty, tcx, &provider, &mut record_binder);
    Ok(kind)
}

#[derive(Debug)]
struct NominalTypeBinderRecord {
    span: Span,
    kind: super::NominalTypeBinderKind,
    module_path: String,
    name: String,
}

fn publish_checked_type_binders<'t, 'm, 'e, P>(
    ty: &'t crate::ast::Type<P>,
    tcx: &mut super::TypeCtx<'m, 'e, P>,
    provider: &NominalProvider<'m, P>,
    record_binder: &mut impl FnMut(Span, &str, TypeBinderRecordKind, &mut super::TypeCtx<'m, 'e, P>),
) where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let nominal = if <P::Typer as super::Typer<P>>::RECORD_NOMINAL_TYPE_BINDERS {
        let mut records = Vec::new();
        collect_nominal_type_binders(ty, tcx, provider, &mut Vec::new(), &mut records);
        records
    } else {
        Vec::new()
    };
    publish_type_binders_inner(ty, tcx, record_binder);
    for record in nominal {
        <P::Typer as super::Typer<P>>::record_nominal_type_binder_at(
            record.span,
            record.kind,
            &record.module_path,
            &record.name,
            tcx,
        );
    }
}

fn collect_nominal_type_binders<'a, P>(
    ty: &'a crate::ast::Type<P>,
    tcx: &super::TypeCtx<'_, '_, P>,
    provider: &NominalProvider<'_, P>,
    bound: &mut Vec<&'a str>,
    out: &mut Vec<NominalTypeBinderRecord>,
) where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    match ty {
        crate::ast::Type::Unit { .. }
        | crate::ast::Type::Bottom { .. }
        | crate::ast::Type::Infer { .. } => {}
        crate::ast::Type::Goal { .. } => {
            unreachable!("checked source types cannot retain an internal inference goal")
        }
        crate::ast::Type::Function { param, ret, .. } => {
            collect_nominal_type_binders(param, tcx, provider, bound, out);
            collect_nominal_type_binders(ret, tcx, provider, bound, out);
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            collect_nominal_type_binders(left, tcx, provider, bound, out);
            collect_nominal_type_binders(right, tcx, provider, bound, out);
        }
        crate::ast::Type::Path { segments, args, .. } => {
            if let Some(head) = segments.first()
                && !bound.iter().any(|name| *name == head.as_str())
                && tcx.type_param_kind(head.as_str()).is_none()
                && let NominalSelection::Selected(selected) =
                    provider.select(provider.root(), segments, false)
                && let Some(owner) = selected.owner.module()
            {
                let selected = selected.declaration;
                let identity = selected
                    .type_alias()
                    .map(|declaration| {
                        (
                            super::NominalTypeBinderKind::TypeAlias,
                            declaration.name.as_str(),
                        )
                    })
                    .or_else(|| {
                        selected.newtype().map(|declaration| {
                            (
                                super::NominalTypeBinderKind::Newtype,
                                declaration.name.as_str(),
                            )
                        })
                    })
                    .or_else(|| {
                        selected.host_type().map(|declaration| {
                            (
                                super::NominalTypeBinderKind::HostType,
                                declaration.name.as_str(),
                            )
                        })
                    });
                if let (Some(segment), Some((kind, name))) = (segments.last(), identity) {
                    out.push(NominalTypeBinderRecord {
                        span: segment.span,
                        kind,
                        module_path: owner
                            .path
                            .segments
                            .iter()
                            .map(crate::ast::PathSegment::as_str)
                            .collect::<Vec<_>>()
                            .join("/"),
                        name: name.to_owned(),
                    });
                }
            }
            for argument in args {
                collect_nominal_type_binders(argument, tcx, provider, bound, out);
            }
        }
        crate::ast::Type::Forall { param, body, .. } => {
            bound.push(param.name.as_str());
            collect_nominal_type_binders(body, tcx, provider, bound, out);
            bound.pop();
        }
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
    }
}

fn compute_checked_kind_with_provider<'m, P>(
    ty: &crate::ast::Type<P>,
    span: Span,
    tcx: &super::TypeCtx<'m, '_, P>,
    provider: &NominalProvider<'m, P>,
) -> Result<crate::ast::Kind, Error>
where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let mut infer = IgnoreInfer;
    compute_kind_with_embedded_binders(
        ty,
        span,
        tcx,
        &mut Vec::new(),
        &NoKindBinders,
        &mut CompleteSchemeProofs::default(),
        super::kind_scheme::KindDemand::Tree,
        &mut infer,
        None,
        false,
        provider,
        provider.root(),
        true,
    )
}

fn publish_type_binders_inner<'t, 'm, 'e, P>(
    ty: &'t crate::ast::Type<P>,
    tcx: &mut super::TypeCtx<'m, 'e, P>,
    record_binder: &mut impl FnMut(Span, &str, TypeBinderRecordKind, &mut super::TypeCtx<'m, 'e, P>),
) where
    P: super::TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    match ty {
        crate::ast::Type::Unit { .. }
        | crate::ast::Type::Bottom { .. }
        | crate::ast::Type::Infer { .. } => {}
        crate::ast::Type::Goal { .. } => unreachable!(
            "check_kinds_in_type checks user-written types; internal Type::Goal values \
             require the goal-aware kind checker"
        ),
        crate::ast::Type::Function { param, ret, .. } => {
            publish_type_binders_inner(param, tcx, record_binder);
            publish_type_binders_inner(ret, tcx, record_binder);
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            publish_type_binders_inner(left, tcx, record_binder);
            publish_type_binders_inner(right, tcx, record_binder);
        }
        crate::ast::Type::Path { segments, args, .. } => {
            // A single-segment head that resolves to an in-scope
            // type parameter is a *use* of that binder. Record it so
            // the LSP can resolve go-to-definition / references on a
            // type-param reference in a type position (`x: A`); value
            // paths record their binders in `synth_path`, but type
            // references resolve here.
            if segments.len() == 1 && tcx.type_param_kind(segments[0].as_str()).is_some() {
                record_binder(
                    segments[0].span,
                    segments[0].as_str(),
                    TypeBinderRecordKind::Reference,
                    tcx,
                );
            }
            // The outer checked-kind walk has already validated this
            // application in its expected kind position. This second walk
            // records binder metadata only; requiring every argument to have
            // kind `*` here would incorrectly reject a constructor argument
            // such as the `F` in `Monad(F)`.
            for a in args {
                publish_type_binders_inner(a, tcx, record_binder);
            }
        }
        crate::ast::Type::Forall { param, body, .. } => {
            let mark = tcx.save();
            tcx.push_owned_type_param_kinded(param);
            record_binder(
                param.span,
                param.name.as_str(),
                TypeBinderRecordKind::Declaration,
                tcx,
            );
            publish_type_binders_inner(body, tcx, record_binder);
            tcx.restore(mark);
        }
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
    }
}

/// Build a call to a compiler intrinsic (`__left__`,
/// `__pair__`, `__if_then_else__`, etc.). The callee position
/// is a single-segment `Expr::Path` carrying the intrinsic name;
/// the args are a positional list of `CallArg`s where each entry
/// is either a type-arg (`CallArg::Type`) or a value-arg
/// (`CallArg::Value`). The typer pairs them positionally against
/// the intrinsic's scheme.
///
/// The returned span is shared by the call, the callee path, and
/// the surrounding span of the intrinsic invocation. Callers that
/// want distinct sub-spans can build the `Expr::Call` directly.
///
/// Phase-polymorphic — `Expr::Call` and `Expr::Path` exist on
/// every phase's `Expr<P>` enum without phase-specific extension
/// fields. `prime::typer` builds Prime intrinsic calls through
/// this helper.
pub fn intrinsic_call<P>(
    name: &str,
    args: Vec<crate::ast::CallArg<P>>,
    span: Span,
) -> crate::ast::Expr<P>
where
    P: crate::ast::Phase<ExprResolved = ()>,
{
    let mut call = crate::ast::Expr::synth_call(
        crate::ast::Expr::Path {
            occurrence: crate::ast::ExpressionOccurrenceCarrier::fresh(),
            segments: vec![crate::ast::PathSegment::new(name.to_owned(), span)],
            meta: Meta::new(span),
            ext: (),
        },
        args,
        span,
    );
    *call.occurrence_mut() = crate::ast::ExpressionOccurrenceCarrier::fresh();
    call
}

/// Capture-avoiding substitution under a `Forall` binder.
/// Drops any subst entry whose key is shadowed by a
/// binder; if a binder name collides with a free type variable in
/// the surviving subst's values, alpha-rename the binder to a fresh
/// spelling so the value's free var doesn't get captured. Names free
/// in the body stay reserved because they may refer to enclosing binders.
fn subst_under_binder<P>(
    param: &crate::ast::TypeParam,
    body: &crate::ast::Type<P>,
    subst: &HashMap<String, crate::ast::Type<P>>,
) -> (crate::ast::TypeParam, crate::ast::Type<P>)
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let mut combined: HashMap<String, crate::ast::Type<P>> = HashMap::new();
    for (k, v) in subst {
        if k != &param.name {
            combined.insert(k.clone(), v.clone());
        }
    }
    let mut free: std::collections::HashSet<String> = std::collections::HashSet::new();
    for v in combined.values() {
        collect_free_type_vars(v, &mut free);
    }
    let new_param = if free.contains(&param.name) {
        let mut taken = free.clone();
        collect_free_type_vars(body, &mut taken);
        taken.insert(param.name.clone());
        let fresh = fresh_type_var(&param.name, &taken);
        combined.insert(
            param.name.clone(),
            crate::ast::Type::synth_path(vec![fresh.clone()], Vec::new(), param.span),
        );
        crate::ast::TypeParam {
            name: fresh,
            span: param.span,
            kind: param.kind.clone(),
        }
    } else {
        param.clone()
    };
    let body = if combined.is_empty() {
        body.clone()
    } else {
        subst_type(body, &combined)
    };
    (new_param, body)
}

/// Apply `args` to a path-like type head without losing an already-written
/// partial application.
///
/// The goal solver and capture-avoiding substitution share this operation so
/// a higher-kinded goal's use-site arguments are appended exactly once.
pub(crate) fn append_type_args<P>(
    head: crate::ast::Type<P>,
    args: Vec<crate::ast::Type<P>>,
    span: Span,
) -> Option<crate::ast::Type<P>>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    if args.is_empty() {
        return Some(head);
    }
    match head {
        crate::ast::Type::Path {
            segments,
            args: mut head_args,
            ..
        } => {
            head_args.extend(args);
            Some(crate::ast::Type::synth_path_segments(
                segments, head_args, span,
            ))
        }
        crate::ast::Type::Goal {
            goal,
            args: mut head_args,
            ext,
            ..
        } => {
            head_args.extend(args);
            Some(crate::ast::Type::Goal {
                goal,
                args: head_args,
                meta: Meta::new(span),
                ext,
            })
        }
        _ => None,
    }
}

/// Apply a type-parameter substitution throughout a type. A keyed bare
/// single-segment path is replaced directly. A keyed applied head such as
/// `F(A)` is replaced by a path-like substitute with the recursively
/// substituted use-site arguments appended; qualified heads are unchanged.
/// Substitution under `Forall` alpha-renames a binder when necessary to keep
/// free names in replacement types free.
///
/// The phase bound excludes surface label sugar, which has no substitution
/// semantics at the typer and backend phases that call this function.
pub fn subst_type<P>(
    ty: &crate::ast::Type<P>,
    subst: &HashMap<String, crate::ast::Type<P>>,
) -> crate::ast::Type<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    if subst.is_empty() {
        return ty.clone();
    }
    match ty {
        crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => ty.clone(),
        crate::ast::Type::Function {
            param,
            ret,
            meta: Meta { span, .. },
            abi_arity,
            caps,
        } => {
            let param = subst_type(param, subst);
            let ret = subst_type(ret, subst);
            crate::ast::Type::Function {
                param: Box::new(param),
                ret: Box::new(ret),
                meta: Meta::new(*span),
                abi_arity: *abi_arity,
                caps: caps.clone(),
            }
        }
        crate::ast::Type::Product {
            left,
            right,
            meta: Meta { span, .. },
        } => crate::ast::Type::Product {
            left: Box::new(subst_type(left, subst)),
            right: Box::new(subst_type(right, subst)),
            meta: Meta::new(*span),
        },
        crate::ast::Type::Sum {
            left,
            right,
            meta: Meta { span, .. },
        } => crate::ast::Type::Sum {
            left: Box::new(subst_type(left, subst)),
            right: Box::new(subst_type(right, subst)),
            meta: Meta::new(*span),
        },
        crate::ast::Type::Path {
            segments,
            args,
            meta: Meta { span, .. },
            ..
        } => {
            // Single-segment Paths can be type-parameter references;
            // if the head is in the substitution, replace it. Multi-
            // segment paths (qualified imports) are never type-params,
            // so we never substitute their head.
            let subst_args: Vec<crate::ast::Type<P>> =
                args.iter().map(|a| subst_type(a, subst)).collect();
            if segments.len() == 1
                && let Some(t) = subst.get(segments[0].as_str())
            {
                if args.is_empty() {
                    return t.clone();
                }
                return append_type_args(t.clone(), subst_args, *span).unwrap_or_else(|| {
                    unreachable!(
                        "a higher-kinded substitution must resolve to a path or inference-goal head"
                    )
                });
            }
            crate::ast::Type::synth_path_segments(segments.clone(), subst_args, *span)
        }
        crate::ast::Type::Forall {
            param,
            body,
            meta: Meta { span, .. },
        } => {
            let (new_param, body) = subst_under_binder(param, body, subst);
            crate::ast::Type::Forall {
                param: new_param,
                body: Box::new(body),
                meta: Meta::new(*span),
            }
        }
        crate::ast::Type::Goal {
            goal,
            args,
            meta: Meta { span, .. },
            ext,
        } => crate::ast::Type::Goal {
            goal: *goal,
            args: args.iter().map(|arg| subst_type(arg, subst)).collect(),
            meta: Meta::new(*span),
            ext: ext.clone(),
        },
        // Statically uninhabited per the `TypeLabelSugar = Never`
        // bound on `P`; discharge by case analysis on the empty type.
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        // A source `_` is not a named variable and therefore cannot be
        // captured. Preserve it while alpha-renaming the surrounding checked
        // annotation; synthesis-only paths reject it before a phase boundary.
        crate::ast::Type::Infer { .. } => ty.clone(),
    }
}

/// Collect the set of free single-segment type-variable names appearing
/// in `ty` into `out`. A "free type variable" is a single-segment
/// `Type::Path` with no args whose head is *not* shadowed by an
/// enclosing `Type::Forall`. Used by capture-avoiding substitution.
pub(crate) fn collect_free_type_vars<P>(
    ty: &crate::ast::Type<P>,
    out: &mut std::collections::HashSet<String>,
) where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    match ty {
        crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => {}
        crate::ast::Type::Path { segments, args, .. } => {
            // A single-segment head is a free type variable — whether
            // bare (`A`) or applied as a higher-kinded brand (`F(A)`,
            // where the kind-`*→*` binder `f` is itself free). Collect
            // it, then descend into any arguments.
            if segments.len() == 1 {
                out.insert(segments[0].name.clone());
            }
            for a in args {
                collect_free_type_vars(a, out);
            }
        }
        crate::ast::Type::Function { param, ret, .. } => {
            collect_free_type_vars(param, out);
            collect_free_type_vars(ret, out);
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            collect_free_type_vars(left, out);
            collect_free_type_vars(right, out);
        }
        crate::ast::Type::Forall { param, body, .. } => {
            // Collect from body, then remove this binder's name.
            let mut inner: std::collections::HashSet<String> = std::collections::HashSet::new();
            collect_free_type_vars(body, &mut inner);
            inner.remove(&param.name);
            out.extend(inner);
        }
        crate::ast::Type::Goal { args, .. } => {
            for arg in args {
                collect_free_type_vars(arg, out);
            }
        }
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        // A checked annotation may still contain `_` while its surrounding
        // `Forall` is alpha-renamed. The placeholder contributes no free name.
        crate::ast::Type::Infer { .. } => {}
    }
}

/// Pick a name not in `taken` by adding `_n2`, `_n3`, … before the trailing affix of `base`.
pub(crate) fn fresh_type_var(base: &str, taken: &std::collections::HashSet<String>) -> String {
    fresh_type_var_with(base, |candidate| taken.contains(candidate))
}

/// Pick a fresh type-variable name using a borrowed exact-membership view.
///
/// This is the non-copying form used when part of the protected namespace is
/// already stored in a persistent lexical scope. Callers can combine that
/// scope with a small operation-local set without first materializing the
/// complete scope as a `HashSet`.
pub(crate) fn fresh_type_var_with(base: &str, is_taken: impl Fn(&str) -> bool) -> String {
    if !is_taken(base) {
        return base.to_string();
    }
    let mut i = 2usize;
    loop {
        let candidate = crate::naming::indexed_name(base, i);
        if !is_taken(&candidate) {
            return candidate;
        }
        i += 1;
    }
}

/// Right-fold a list of types into a sum chain. An empty list
/// returns the bottom type `!` (the sum identity); a single-element
/// list returns the element directly. Used by the elaborator to
/// build up an `A | B | C | …` shape from a vector of branches.
///
/// ```text
/// right_fold_sum[A][B][C] = A | (B | C)
/// ```
///
/// Phase-polymorphic. `Type::Bottom` and `Type::Sum` exist on
/// every phase's `Type<P>` enum with no extension fields, so the
/// fold works uniformly for any `P: Phase`. The `P: Clone` bound
/// matches the derived `Type<P>: Clone` impl — every concrete
/// exported phase (`Surface`, `Desugared`, `Lowered`, `UncheckedPrime`,
/// `Prime`, `Enriched`, and `Routed`) implements `Clone`, as does
/// substitution's private `PrePrime` staging phase, so callers don't need to
/// think about it. The bound, rather than this phase catalogue, governs other
/// implementations. `prime::typer` calls
/// this on `Type<Prime>` values without a separate copy.
pub fn right_fold_sum<P>(branches: &[crate::ast::Type<P>], span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase + Clone,
{
    right_fold_types(
        branches,
        span,
        crate::ast::Type::Bottom {
            meta: Meta::new(span),
        },
        |left, right, span| crate::ast::Type::Sum {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(span),
        },
    )
}

/// Right-fold a list of factor types into a `(T_n0 & (T_n1 & ...))`
/// product. Empty list → `()`; singleton → the element verbatim.
/// The `&`-mirror of [`right_fold_sum`], with `()` as the neutral
/// and `Type::Product` as the combiner. Phase-polymorphic for the
/// same reason: `Type::Unit` and `Type::Product` exist on every
/// phase's `Type<P>` enum.
pub fn right_fold_product<P>(factors: &[crate::ast::Type<P>], span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase + Clone,
{
    right_fold_types(
        factors,
        span,
        crate::ast::Type::Unit {
            meta: Meta::new(span),
        },
        |left, right, span| crate::ast::Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(span),
        },
    )
}

/// Shared right-fold over a `Type<P>` slice, used by
/// [`right_fold_sum`] and [`right_fold_product`]. Empty → `empty`;
/// singleton → the element verbatim; otherwise apply `combine`
/// from right to left, threading `span` into each combiner.
fn right_fold_types<P, F>(
    items: &[crate::ast::Type<P>],
    span: Span,
    empty: crate::ast::Type<P>,
    combine: F,
) -> crate::ast::Type<P>
where
    P: crate::ast::Phase + Clone,
    F: Fn(crate::ast::Type<P>, crate::ast::Type<P>, Span) -> crate::ast::Type<P>,
{
    if items.is_empty() {
        return empty;
    }
    if items.len() == 1 {
        return items[0].clone();
    }
    let mut iter = items.iter().rev();
    let mut acc = iter.next().unwrap().clone();
    for ty in iter {
        acc = combine(ty.clone(), acc, span);
    }
    acc
}

/// Render a [`Type`] as a human-readable string for diagnostics.
/// The format is the surface syntax: `()` / `!` / `(A & B)` /
/// `(A | B)` / `(A & B) -> C` / `Foo[A][B]`. Used by the typer's
/// `expected … found …` error messages and any other diagnostic
/// that wants a recognisable type name.
///
/// Phase-polymorphic with `P::TypeLabelSugar = Never` — see
/// [`subst_type`] for the rationale.
pub fn display_type<P>(ty: &crate::ast::Type<P>) -> String
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    let mut s = String::new();
    write_type_with_path_style(ty, &mut s, PathStyle::Source);
    s
}

/// Render a reflected type with canonical module separators. A lexical alias
/// has one qualifier segment, so its source-shaped `m.Item` spelling is
/// unchanged while a multi-component canonical path becomes `foo/bar.Item`.
#[cfg(feature = "surface")]
pub(crate) fn display_reflected_type<P>(ty: &crate::ast::Type<P>) -> String
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    let mut s = String::new();
    write_type_with_path_style(ty, &mut s, PathStyle::CanonicalModule);
    s
}

#[cfg(feature = "surface")]
pub(crate) fn display_reflected_type_name<S: AsRef<str>>(segments: &[S]) -> String {
    let Some((name, module)) = segments.split_last() else {
        return String::new();
    };
    if module.is_empty() {
        return name.as_ref().to_owned();
    }
    format!(
        "{}.{}",
        module
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join("/"),
        name.as_ref()
    )
}

#[derive(Clone, Copy)]
enum PathStyle {
    Source,
    #[cfg(feature = "surface")]
    CanonicalModule,
}

/// Append the surface-syntax rendering of `ty` to `out`. Driven by
/// [`display_type`]; exposed separately so callers building larger
/// diagnostic strings can emit a type without an intermediate allocation.
pub fn write_type<P>(ty: &crate::ast::Type<P>, out: &mut String)
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    write_type_with_path_style(ty, out, PathStyle::Source);
}

fn write_type_with_path_style<P>(ty: &crate::ast::Type<P>, out: &mut String, path_style: PathStyle)
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    match ty {
        crate::ast::Type::Unit { .. } => out.push('.'),
        crate::ast::Type::Bottom { .. } => out.push('!'),
        crate::ast::Type::Function { param, ret, .. } => {
            write_function_domain_type(param, out, path_style);
            out.push_str(" -> ");
            write_type_with_path_style(ret, out, path_style);
        }
        crate::ast::Type::Product { left, right, .. } => {
            out.push('(');
            write_chain_child_type(left, out, path_style);
            out.push_str(" & ");
            write_chain_child_type(right, out, path_style);
            out.push(')');
        }
        crate::ast::Type::Sum { left, right, .. } => {
            out.push('(');
            write_chain_child_type(left, out, path_style);
            out.push_str(" | ");
            write_chain_child_type(right, out, path_style);
            out.push(')');
        }
        crate::ast::Type::Path { segments, args, .. } => {
            // NUL-prefixed names are compiler-only inference holes. They
            // cannot occur in parsed Kio identifiers, and diagnostics must
            // render the source-level unknown rather than leaking planner
            // implementation details.
            if segments.iter().any(|segment| segment.starts_with('\0')) {
                out.push('_');
            } else {
                match path_style {
                    PathStyle::Source => out.push_str(&segments.join(".")),
                    #[cfg(feature = "surface")]
                    PathStyle::CanonicalModule => {
                        out.push_str(&display_reflected_type_name(segments));
                    }
                }
            }
            if !args.is_empty() {
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_type_with_path_style(a, out, path_style);
                }
                out.push(')');
            }
        }
        crate::ast::Type::Forall { param, body, .. } => {
            let mut binders = vec![param];
            let mut cur = body.as_ref();
            while let crate::ast::Type::Forall { param, body, .. } = cur {
                binders.push(param);
                cur = body.as_ref();
            }
            for tp in binders {
                out.push('[');
                for _ in 0..tp.effective_kind().arity() {
                    out.push('*');
                }
                out.push_str(&tp.name);
                out.push(']');
            }
            out.push(' ');
            write_type_with_path_style(cur, out, path_style);
        }
        // Statically uninhabited per the `TypeLabelSugar = Never`
        // bound on `P`.
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        // Partial source annotations legitimately reach diagnostics before
        // their `_` slots are checked against a synthesized type.
        crate::ast::Type::Infer { .. } => out.push('_'),
        crate::ast::Type::Goal { args, .. } => {
            out.push('_');
            if !args.is_empty() {
                out.push('(');
                for (index, arg) in args.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    write_type_with_path_style(arg, out, path_style);
                }
                out.push(')');
            }
        }
    }
}

fn write_function_domain_type<P>(ty: &crate::ast::Type<P>, out: &mut String, path_style: PathStyle)
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    match ty {
        crate::ast::Type::Function { .. } | crate::ast::Type::Forall { .. } => {
            out.push('(');
            write_type_with_path_style(ty, out, path_style);
            out.push(')');
        }
        _ => write_type_with_path_style(ty, out, path_style),
    }
}

fn write_chain_child_type<P>(ty: &crate::ast::Type<P>, out: &mut String, path_style: PathStyle)
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    if matches!(
        ty,
        crate::ast::Type::Function { .. } | crate::ast::Type::Forall { .. }
    ) {
        out.push('(');
        write_type_with_path_style(ty, out, path_style);
        out.push(')');
    } else {
        write_type_with_path_style(ty, out, path_style);
    }
}
// =========================================================================
// Tiny type / scheme constructors
// =========================================================================
//
// Pure builder helpers. They construct AST nodes — no pattern-matching
// against phase-specific variants beyond the `TypeLabelSugar = Never`
// discharge in [`clone_with_span`]. Both `typecheck_full` (P = Lowered)
// and the standalone Kio'-only checker in [`crate::prime::typer`] (P = Prime)
// call these with the same source.

/// Build a `Type::Path` referencing the named identifier with no
/// type arguments. Used for type-parameter references (`A`, `B`) and
/// nominal type references (`Bool`).
pub fn ty_path<P>(name: &str, span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    crate::ast::Type::synth_path(vec![name.to_owned()], Vec::new(), span)
}

/// Build a `Type::Sum` (`A | B`).
pub fn ty_sum<P>(
    left: crate::ast::Type<P>,
    right: crate::ast::Type<P>,
    span: Span,
) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    crate::ast::Type::Sum {
        left: Box::new(left),
        right: Box::new(right),
        meta: Meta::new(span),
    }
}

/// Build a `Type::Product` (`A & B`).
pub fn ty_product<P>(
    left: crate::ast::Type<P>,
    right: crate::ast::Type<P>,
    span: Span,
) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    crate::ast::Type::Product {
        left: Box::new(left),
        right: Box::new(right),
        meta: Meta::new(span),
    }
}

/// Build a `Type::Function` with the given param list and return type.
pub fn ty_func<P>(
    params: Vec<crate::ast::Type<P>>,
    ret: crate::ast::Type<P>,
    span: Span,
) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    crate::ast::Type::synth_function(params, ret, span)
}

/// Build the identity-exact segment vector `[module …, name]` for a
/// nominal type declared in `module_path` (a slash-joined module path).
/// Used at the qualified-import and fully-qualified-name resolution
/// sites, where the declaring module is already explicit, to mint the
/// `(module, name)` head a newtype's own type carries.
pub fn nominal_segments_in_module(
    module_path: &str,
    name: &str,
    span: Span,
) -> Vec<crate::ast::PathSegment> {
    module_path
        .split('/')
        .filter(|s| !s.is_empty())
        .chain(std::iter::once(name))
        .map(|s| crate::ast::PathSegment::new(s.to_owned(), span))
        .collect()
}

/// Build a newtype's own (self-)type from its identity-qualified
/// segments — the `(module …, name)` path the typer matches a nominal
/// type by. Like [`ty_nominal`] but the head carries the
/// defining-module prefix so the synthesized constructor result /
/// projector receiver type is identity-exact, distinguishing two
/// dependencies that each re-root a same-named `pub newtype`.
pub fn ty_nominal_segments<P>(
    segments: &[crate::ast::PathSegment],
    type_params: &[crate::ast::TypeParam],
    span: Span,
) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    crate::ast::Type::synth_path_segments(
        segments.to_vec(),
        type_params.iter().map(|p| ty_path(&p.name, span)).collect(),
        span,
    )
}

/// Build a `SignatureParam::Type` for a type-parameter `<name>`.
pub fn tp<P>(name: &str, span: Span) -> crate::ast::SignatureParam<P>
where
    P: crate::ast::Phase,
{
    crate::ast::SignatureParam::Type(crate::ast::TypeParam {
        name: name.to_owned(),
        span,
        kind: None,
    })
}

/// Build a `SignatureParam::Value` for an annotated value parameter
/// `name: ty`.
pub fn vp<P>(name: &str, ty: crate::ast::Type<P>, span: Span) -> crate::ast::SignatureParam<P>
where
    P: crate::ast::Phase,
{
    crate::ast::SignatureParam::Value(crate::ast::Param {
        name: name.to_owned(),
        ty: Some(ty),
        pattern: Default::default(),
        meta: Meta::new(span),
    })
}

/// Clone a type expression but rewrite every span to the given one.
/// Used so synthetic schemes report errors at the user's call-site
/// rather than at the original declaration's source coordinates.
///
/// Phase-polymorphic with `P::TypeLabelSugar = Never` — see
/// [`subst_type`] for the rationale.
pub fn clone_with_span<P>(ty: &crate::ast::Type<P>, span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    match ty {
        crate::ast::Type::Unit { .. } => crate::ast::Type::Unit {
            meta: Meta::new(span),
        },
        crate::ast::Type::Bottom { .. } => crate::ast::Type::Bottom {
            meta: Meta::new(span),
        },
        crate::ast::Type::Function {
            param,
            ret,
            abi_arity,
            caps,
            ..
        } => {
            let param = clone_with_span(param, span);
            let ret = clone_with_span(ret, span);
            crate::ast::Type::Function {
                param: Box::new(param),
                ret: Box::new(ret),
                meta: Meta::new(span),
                abi_arity: *abi_arity,
                caps: caps.clone(),
            }
        }
        crate::ast::Type::Product { left, right, .. } => crate::ast::Type::Product {
            left: Box::new(clone_with_span(left, span)),
            right: Box::new(clone_with_span(right, span)),
            meta: Meta::new(span),
        },
        crate::ast::Type::Sum { left, right, .. } => crate::ast::Type::Sum {
            left: Box::new(clone_with_span(left, span)),
            right: Box::new(clone_with_span(right, span)),
            meta: Meta::new(span),
        },
        crate::ast::Type::Path { segments, args, .. } => crate::ast::Type::synth_path_segments(
            segments
                .iter()
                .map(|segment| crate::ast::PathSegment::new(segment.name.clone(), span))
                .collect(),
            args.iter().map(|a| clone_with_span(a, span)).collect(),
            span,
        ),
        crate::ast::Type::Forall { param, body, .. } => {
            let mut param = param.clone();
            param.span = span;
            crate::ast::Type::Forall {
                param,
                body: Box::new(clone_with_span(body, span)),
                meta: Meta::new(span),
            }
        }
        crate::ast::Type::Goal {
            goal, args, ext, ..
        } => crate::ast::Type::Goal {
            goal: *goal,
            args: args.iter().map(|arg| clone_with_span(arg, span)).collect(),
            meta: Meta::new(span),
            ext: ext.clone(),
        },
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        crate::ast::Type::Infer { .. } => unreachable!(
            "Type::Infer should be resolved by the typer's substitution pass; check_well_formed_type catches it at the boundary"
        ),
    }
}

/// Give every bound type variable a deterministic presentation and stamp the
/// rebuilt type at one source position.
///
/// Free single-segment names are reserved before assigning the canonical
/// `A`, `A_n2`, ... sequence, so a bound name can never capture a free one.
/// Both passes are linear in the type tree; the binder maps retain lexical
/// shadowing without rescanning enclosing binders. The returned handle is
/// deliberately fresh while preserving the input handle's nominal-identity
/// provenance.
#[cfg(feature = "surface")]
pub(crate) fn canonicalize_bound_type_names_at<P>(
    ty: &super::InternedType<P>,
    span: Span,
) -> super::InternedType<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    canonicalize_bound_type_presentation_at(ty, span)
}

/// Canonicalize a type for a recorded source-level type annotation.
///
/// In addition to deterministic binder names and spans, function ABI metadata
/// follows what reparsing the printed type expression produces: a unit domain
/// has zero ABI slots and every other arrow has one structural domain value.
/// Named-signature and lambda value-group arity belongs to the value expression
/// and is bridged by the ordinary higher-order adapter machinery; it is not
/// representable in type-expression syntax.
#[cfg(feature = "surface")]
pub(crate) fn canonicalize_type_annotation_presentation_at<P>(
    ty: &super::InternedType<P>,
    span: Span,
) -> super::InternedType<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let canonical = canonicalize_bound_type_names_at(ty, span);
    let mut annotation = canonical.clone_type();
    canonicalize_type_expression_function_abi(&mut annotation);
    super::InternedType::fresh_with_identity(annotation, canonical.identity_is_canonical())
}

/// Normalize function ABI metadata to the representation carried by type
/// expression syntax. Runtime callable and declaration grouping use their own
/// selected ABI and must cross this boundary through a higher-order adapter.
#[cfg(any(feature = "surface", test))]
pub(crate) fn canonicalize_type_expression_function_abi<P>(ty: &mut crate::ast::Type<P>)
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    match ty {
        crate::ast::Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } => {
            canonicalize_type_expression_function_abi(param);
            canonicalize_type_expression_function_abi(ret);
            *abi_arity = usize::from(!matches!(param.as_ref(), crate::ast::Type::Unit { .. }));
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            canonicalize_type_expression_function_abi(left);
            canonicalize_type_expression_function_abi(right);
        }
        crate::ast::Type::Path { args, .. } | crate::ast::Type::Goal { args, .. } => {
            for arg in args {
                canonicalize_type_expression_function_abi(arg);
            }
        }
        crate::ast::Type::Forall { body, .. } => canonicalize_type_expression_function_abi(body),
        crate::ast::Type::Unit { .. }
        | crate::ast::Type::Bottom { .. }
        | crate::ast::Type::Infer { .. } => {}
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
    }
}

#[cfg(feature = "surface")]
fn canonicalize_bound_type_presentation_at<P>(
    ty: &super::InternedType<P>,
    span: Span,
) -> super::InternedType<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    fn collect_free_names<P>(
        ty: &crate::ast::Type<P>,
        bound: &mut HashMap<String, usize>,
        free: &mut HashSet<String>,
    ) where
        P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
    {
        match ty {
            crate::ast::Type::Unit { .. }
            | crate::ast::Type::Bottom { .. }
            | crate::ast::Type::Infer { .. } => {}
            crate::ast::Type::Function { param, ret, .. } => {
                collect_free_names(param, bound, free);
                collect_free_names(ret, bound, free);
            }
            crate::ast::Type::Product { left, right, .. }
            | crate::ast::Type::Sum { left, right, .. } => {
                collect_free_names(left, bound, free);
                collect_free_names(right, bound, free);
            }
            crate::ast::Type::Path { segments, args, .. } => {
                if segments.len() == 1 && !bound.contains_key(segments[0].as_str()) {
                    free.insert(segments[0].name.clone());
                }
                for arg in args {
                    collect_free_names(arg, bound, free);
                }
            }
            crate::ast::Type::Goal { args, .. } => {
                for arg in args {
                    collect_free_names(arg, bound, free);
                }
            }
            crate::ast::Type::Forall { param, body, .. } => {
                *bound.entry(param.name.clone()).or_insert(0) += 1;
                collect_free_names(body, bound, free);
                let depth = bound
                    .get_mut(&param.name)
                    .expect("canonical type binder disappeared during free-name collection");
                *depth -= 1;
                if *depth == 0 {
                    bound.remove(&param.name);
                }
            }
            crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    struct Canonicalizer {
        taken: HashSet<String>,
        next_name: usize,
        bound: HashMap<String, Vec<String>>,
        span: Span,
    }

    impl Canonicalizer {
        fn fresh_name(&mut self) -> String {
            loop {
                let index = self.next_name;
                self.next_name += 1;
                let candidate = if index == 0 {
                    "A".to_owned()
                } else {
                    format!("A_n{}", index + 1)
                };
                if self.taken.insert(candidate.clone()) {
                    return candidate;
                }
            }
        }

        fn rebuild<P>(&mut self, ty: &crate::ast::Type<P>) -> crate::ast::Type<P>
        where
            P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
        {
            match ty {
                crate::ast::Type::Unit { .. } => crate::ast::Type::Unit {
                    meta: Meta::new(self.span),
                },
                crate::ast::Type::Bottom { .. } => crate::ast::Type::Bottom {
                    meta: Meta::new(self.span),
                },
                crate::ast::Type::Function {
                    param,
                    ret,
                    abi_arity,
                    caps,
                    ..
                } => {
                    let param = self.rebuild(param);
                    let ret = self.rebuild(ret);
                    crate::ast::Type::Function {
                        param: Box::new(param),
                        ret: Box::new(ret),
                        meta: Meta::new(self.span),
                        abi_arity: *abi_arity,
                        caps: caps.clone(),
                    }
                }
                crate::ast::Type::Product { left, right, .. } => crate::ast::Type::Product {
                    left: Box::new(self.rebuild(left)),
                    right: Box::new(self.rebuild(right)),
                    meta: Meta::new(self.span),
                },
                crate::ast::Type::Sum { left, right, .. } => crate::ast::Type::Sum {
                    left: Box::new(self.rebuild(left)),
                    right: Box::new(self.rebuild(right)),
                    meta: Meta::new(self.span),
                },
                crate::ast::Type::Path { segments, args, .. } => {
                    let mut segments = segments
                        .iter()
                        .map(|segment| {
                            crate::ast::PathSegment::new(segment.name.clone(), self.span)
                        })
                        .collect::<Vec<_>>();
                    if segments.len() == 1
                        && let Some(name) = self
                            .bound
                            .get(segments[0].as_str())
                            .and_then(|names| names.last())
                    {
                        segments[0].name.clone_from(name);
                    }
                    crate::ast::Type::Path {
                        segments,
                        args: args.iter().map(|arg| self.rebuild(arg)).collect(),
                        meta: Meta::new(self.span),
                    }
                }
                crate::ast::Type::Goal {
                    goal, args, ext, ..
                } => crate::ast::Type::Goal {
                    goal: *goal,
                    args: args.iter().map(|arg| self.rebuild(arg)).collect(),
                    meta: Meta::new(self.span),
                    ext: ext.clone(),
                },
                crate::ast::Type::Forall { param, body, .. } => {
                    let name = self.fresh_name();
                    self.bound
                        .entry(param.name.clone())
                        .or_default()
                        .push(name.clone());
                    let body = self.rebuild(body);
                    let names = self
                        .bound
                        .get_mut(&param.name)
                        .expect("canonical type binder disappeared during rebuilding");
                    names.pop();
                    if names.is_empty() {
                        self.bound.remove(&param.name);
                    }
                    crate::ast::Type::Forall {
                        param: crate::ast::TypeParam {
                            name,
                            span: self.span,
                            kind: param.kind.clone(),
                        },
                        body: Box::new(body),
                        meta: Meta::new(self.span),
                    }
                }
                crate::ast::Type::Infer { ext, .. } => crate::ast::Type::Infer {
                    meta: Meta::new(self.span),
                    ext: ext.clone(),
                },
                crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
            }
        }
    }

    let mut free = HashSet::new();
    collect_free_names(ty.as_type(), &mut HashMap::new(), &mut free);
    let rebuilt = Canonicalizer {
        taken: free,
        next_name: 0,
        bound: HashMap::new(),
        span,
    }
    .rebuild(ty.as_type());
    super::InternedType::fresh_with_identity(rebuilt, ty.identity_is_canonical())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Lowered, Meta};
    use std::collections::HashSet;
    #[cfg(feature = "surface")]
    use std::path::PathBuf;

    type Ty = crate::ast::Type<Lowered>;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn mixed_call_argument_heads_use_case_without_builtin_name_lookup() {
        for (name, is_type) in [
            ("__Unlisted_type", true),
            ("___Unlisted_type__", true),
            ("__unlisted_value", false),
            ("_Item_type", true),
            ("item_value", false),
        ] {
            let path: crate::ast::Expr<Lowered> = crate::ast::Expr::Path {
                occurrence: Default::default(),
                segments: vec![crate::ast::PathSegment::new(name.to_owned(), sp())],
                meta: Meta::new(sp()),
                ext: (),
            };
            assert_eq!(value_arg_looks_like_type_arg(&path), is_type, "{name}");
            let call = crate::ast::Expr::synth_call(path, Vec::new(), sp());
            assert_eq!(value_arg_looks_like_type_arg(&call), is_type, "{name}");
        }
    }

    #[cfg(feature = "surface")]
    fn parse_module(src: &str) -> crate::ast::Module<Lowered> {
        let surface = crate::pass::parser::parse(src).expect("parse");
        let desugared = crate::pass::desugar::desugar_module(surface).expect("desugar");
        let (mut lowered, _) = crate::pass::label_elab::elaborate_package(
            vec![(PathBuf::from("test.kio"), desugared)],
            None,
        )
        .expect("label_elab");
        lowered.pop().expect("at least one module").1
    }

    fn path(name: &str) -> Ty {
        Ty::synth_path(vec![name.to_owned()], Vec::new(), sp())
    }

    fn applied(name: &str, args: Vec<Ty>) -> Ty {
        Ty::synth_path(vec![name.to_owned()], args, sp())
    }

    fn product(left: Ty, right: Ty) -> Ty {
        Ty::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(sp()),
        }
    }

    fn sum(left: Ty, right: Ty) -> Ty {
        Ty::Sum {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(sp()),
        }
    }

    fn func(param: Ty, ret: Ty) -> Ty {
        Ty::synth_function(vec![param], ret, sp())
    }

    fn goal(args: Vec<Ty>) -> Ty {
        Ty::Goal {
            goal: crate::ast::TypeGoalRef::for_test(1, 2, 3),
            args,
            meta: Meta::new(Span::new(4, 5)),
            ext: (),
        }
    }

    #[cfg(feature = "surface")]
    #[test]
    fn nominal_kind_fanout_uses_one_declaration_provider() {
        const UNRELATED_DECLARATIONS: usize = 64;
        const QUERIES: usize = 32;

        let mut source = String::from(
            "module provider; pub host type Host; \
             pub newtype Nominal : . { pub constructor nominal; pub projector un_nominal; }; \
             pub type Alias = .;",
        );
        for index in 0..UNRELATED_DECLARATIONS {
            source.push_str(&format!(" type Unrelated{index} = .;"));
        }
        let module = parse_module(&source);
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).expect("declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([(
                "provider".to_owned(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from("provider.kio"),
                    module,
                    scope,
                },
            )]),
            None,
        );
        let provider = &package.module("provider").expect("provider module").module;
        let env = super::super::ModuleEnv::build(provider, None, None, Some(&package))
            .expect("provider environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let exact_segments = |name: &str| {
            vec![
                crate::ast::PathSegment::new("provider".to_owned(), sp()),
                crate::ast::PathSegment::new(name.to_owned(), sp()),
            ]
        };

        reset_nominal_head_resolution_work();
        for _ in 0..QUERIES {
            for name in ["Host", "Nominal", "Alias"] {
                assert_eq!(
                    resolve_nominal_head_kind(
                        &exact_segments(name),
                        0,
                        sp(),
                        &tcx,
                        NominalHeadBinding::AmbientOrEmbedded(None),
                        true,
                    )
                    .expect("exact nominal head kind"),
                    crate::ast::Kind::Star,
                );
            }
        }
        let work = nominal_head_resolution_work();
        assert!(
            work.raw_owner_item_visits <= provider.items.len(),
            "one exact declaration provider must serve host, newtype, and alias kind heads in O(declarations + queries), not rescan the owner for every head: {work:?}"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn local_nominal_kind_walk_reuses_one_checking_provider() {
        const UNRELATED_DECLARATIONS: usize = 64;
        const OCCURRENCES: usize = 32;

        let mut source = String::from("module source;");
        for index in 0..UNRELATED_DECLARATIONS {
            source.push_str(&format!(" type Unrelated{index} = .;"));
        }
        source.push_str(
            " newtype Nominal : . { constructor nominal; projector un_nominal; }; \
             type Alias = .; host type Host;",
        );
        let module = parse_module(&source);
        let env = super::super::ModuleEnv::build(&module, None, None, None)
            .expect("checking-root environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let mut ty = product(path("Host"), product(path("Nominal"), path("Alias")));
        for _ in 1..OCCURRENCES {
            ty = product(
                ty,
                product(path("Host"), product(path("Nominal"), path("Alias"))),
            );
        }

        reset_nominal_head_resolution_work();
        assert_eq!(
            compute_checked_kind(&ty, sp(), &tcx).expect("kind"),
            crate::ast::Kind::Star,
        );
        let work = nominal_head_resolution_work();
        assert!(
            work.raw_owner_item_visits <= module.items.len(),
            "one kind walk must share its checking-root declaration provider across every local nominal leaf: {work:?}"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn source_kind_walk_consumes_the_singular_complete_scheme_summary() {
        let module = parse_module("module source; type Scheme = [*F] F(.) -> F(.);");
        let scheme = module
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::TypeAlias(alias) if alias.name == "Scheme" => Some(&alias.body),
                _ => None,
            })
            .expect("Scheme alias body");
        let env = super::super::ModuleEnv::build(&module, None, None, None)
            .expect("checking-root environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = super::super::TypeCtx::new(&env, &mut elaborations);

        super::super::kind_scheme::reset_complete_scheme_consumer_work();
        assert_eq!(
            compute_checked_kind(scheme, scheme.span(), &tcx).expect("complete scheme kind"),
            crate::ast::Kind::Star
        );
        assert_eq!(
            super::super::kind_scheme::complete_scheme_consumer_work().source_kind,
            1,
            "the source kind walk must request one shared complete-scheme summary"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn nominal_kind_written_edge_fanout_uses_the_same_declaration_provider() {
        const UNRELATED_DECLARATIONS: usize = 64;
        const QUERIES: usize = 32;

        let mut provider = String::from(
            "module provider; pub host type Host; \
             pub newtype Nominal : . { pub constructor nominal; pub projector un_nominal; }; \
             pub type Alias = .;",
        );
        for index in 0..UNRELATED_DECLARATIONS {
            provider.push_str(&format!(" type Unrelated{index} = .;"));
        }
        let provider = parse_module(&provider);
        let consumer = parse_module("module consumer; import provider as p;");
        let provider_scope = crate::pass::resolve::TopLevelScope::build(&provider)
            .expect("provider declaration-only scope");
        let consumer_scope = crate::pass::resolve::TopLevelScope::build(&consumer)
            .expect("consumer declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([
                (
                    "consumer".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("consumer.kio"),
                        module: consumer,
                        scope: consumer_scope,
                    },
                ),
                (
                    "provider".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("provider.kio"),
                        module: provider,
                        scope: provider_scope,
                    },
                ),
            ]),
            None,
        );
        let consumer = &package.module("consumer").expect("consumer module").module;
        let provider = &package.module("provider").expect("provider module").module;
        let env = super::super::ModuleEnv::build(consumer, None, None, Some(&package))
            .expect("consumer environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let written_segments = |name: &str| {
            vec![
                crate::ast::PathSegment::new("p".to_owned(), sp()),
                crate::ast::PathSegment::new(name.to_owned(), sp()),
            ]
        };

        reset_nominal_head_resolution_work();
        for _ in 0..QUERIES {
            for name in ["Host", "Nominal", "Alias"] {
                assert_eq!(
                    resolve_nominal_head_kind(
                        &written_segments(name),
                        0,
                        sp(),
                        &tcx,
                        NominalHeadBinding::AmbientOrEmbedded(None),
                        false,
                    )
                    .expect("written qualified nominal head kind"),
                    crate::ast::Kind::Star,
                );
            }
        }
        let work = nominal_head_resolution_work();
        assert!(
            work.raw_owner_item_visits <= provider.items.len(),
            "one written-edge declaration provider must serve host, newtype, and alias kind heads in O(declarations + queries), not rescan its target for qualification and each head class: {work:?}"
        );
    }

    #[test]
    fn substitution_preserves_goal_identity_and_rewrites_arguments() {
        let ty = goal(vec![path("A")]);
        let subst = HashMap::from([("A".to_owned(), path("B"))]);
        let rewritten = subst_type(&ty, &subst);

        let Ty::Goal {
            goal, args, meta, ..
        } = rewritten
        else {
            panic!("expected goal");
        };
        assert_eq!(goal, crate::ast::TypeGoalRef::for_test(1, 2, 3));
        assert_eq!(meta.span, Span::new(4, 5));
        assert!(matches!(&args[..], [Ty::Path { segments, .. }] if segments[0].as_str() == "B"));
    }

    #[test]
    fn higher_kinded_substitution_appends_goal_arguments_exactly_once() {
        let head = goal(vec![path("Existing")]);
        let ty = applied("F", vec![path("UseSite")]);
        let rewritten = subst_type(&ty, &HashMap::from([("F".to_owned(), head)]));

        let Ty::Goal { args, .. } = rewritten else {
            panic!("expected substituted goal head");
        };
        assert_eq!(args.len(), 2);
        assert!(
            matches!(&args[0], Ty::Path { segments, .. } if segments[0].as_str() == "Existing")
        );
        assert!(matches!(&args[1], Ty::Path { segments, .. } if segments[0].as_str() == "UseSite"));
    }

    #[test]
    fn goal_detection_reaches_nested_type_positions() {
        let nested = func(path("A"), product(path("B"), goal(vec![path("C")])));
        assert!(type_contains_goal(&nested));
        assert!(!type_contains_goal(&func(path("A"), path("B"))));
    }

    #[test]
    fn span_clone_preserves_goal_identity_and_rewrites_nested_spans() {
        let rewritten_span = Span::new(20, 30);
        let rewritten = clone_with_span(&goal(vec![path("A")]), rewritten_span);

        let Ty::Goal {
            goal, args, meta, ..
        } = rewritten
        else {
            panic!("expected goal");
        };
        assert_eq!(goal, crate::ast::TypeGoalRef::for_test(1, 2, 3));
        assert_eq!(meta.span, rewritten_span);
        assert_eq!(args[0].span(), rewritten_span);
    }

    #[test]
    fn span_clone_rewrites_forall_binder_and_every_applied_path_segment() {
        let target = Span::new(80, 90);
        let kind = crate::ast::Kind::arrow_chain(2);
        let original = Ty::Forall {
            param: crate::ast::TypeParam {
                name: "F".to_owned(),
                span: Span::new(1, 2),
                kind: Some(kind.clone()),
            },
            body: Box::new(Ty::Path {
                segments: vec![
                    crate::ast::PathSegment::new("pkg".to_owned(), Span::new(3, 4)),
                    crate::ast::PathSegment::new("Apply".to_owned(), Span::new(5, 6)),
                ],
                args: vec![Ty::Path {
                    segments: vec![crate::ast::PathSegment::new(
                        "F".to_owned(),
                        Span::new(9, 10),
                    )],
                    args: Vec::new(),
                    meta: Meta::new(Span::new(7, 11)),
                }],
                meta: Meta::new(Span::new(3, 11)),
            }),
            meta: Meta::new(Span::new(1, 11)),
        };

        let rewritten = clone_with_span(&original, target);
        let Ty::Forall { param, body, meta } = rewritten else {
            panic!("expected forall");
        };
        assert_eq!(param.name, "F");
        assert_eq!(param.kind, Some(kind));
        assert_eq!(meta.span, target);
        let Ty::Path {
            segments,
            args,
            meta,
        } = body.as_ref()
        else {
            panic!("expected applied path");
        };
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.as_str())
                .collect::<Vec<_>>(),
            ["pkg", "Apply"]
        );
        let outer_segment_spans = segments
            .iter()
            .map(|segment| segment.span)
            .collect::<Vec<_>>();
        assert_eq!(meta.span, target);
        let Ty::Path {
            segments,
            args: nested_args,
            meta,
        } = &args[0]
        else {
            panic!("expected path argument");
        };
        assert_eq!(segments[0].as_str(), "F");
        assert!(nested_args.is_empty());
        assert_eq!(meta.span, target);
        assert_eq!(
            (param.span, segments[0].span, outer_segment_spans,),
            (target, target, vec![target, target]),
            "the binder, nested argument head, and every outer path segment relocate together"
        );
    }

    fn forall(name: &str, body: Ty, span: Span) -> Ty {
        Ty::Forall {
            param: crate::ast::TypeParam {
                name: name.to_owned(),
                span,
                kind: None,
            },
            body: Box::new(body),
            meta: Meta::new(span),
        }
    }

    #[test]
    fn substitution_reserves_outer_binder_references_when_renaming_nested_binder() {
        let ty = forall(
            "A_n2",
            forall(
                "A",
                product(path("X"), product(path("A_n2"), path("A"))),
                sp(),
            ),
            sp(),
        );

        let rewritten = subst_type(&ty, &HashMap::from([("X".to_owned(), path("A"))]));

        let Ty::Forall {
            param: outer, body, ..
        } = rewritten
        else {
            panic!("expected outer forall")
        };
        let Ty::Forall {
            param: inner, body, ..
        } = body.as_ref()
        else {
            panic!("expected inner forall")
        };
        assert_eq!(outer.name, "A_n2");
        assert_eq!(inner.name, "A_n3");
        assert_ne!(outer.name, inner.name);

        let Ty::Product {
            left: replacement,
            right,
            ..
        } = body.as_ref()
        else {
            panic!("expected outer product")
        };
        let Ty::Product {
            left: outer_reference,
            right: inner_reference,
            ..
        } = right.as_ref()
        else {
            panic!("expected nested product")
        };
        assert_path_name(replacement, "A");
        assert_path_name(outer_reference, &outer.name);
        assert_path_name(inner_reference, &inner.name);
    }

    #[test]
    fn substitution_preserves_nested_binders_when_no_rename_is_required() {
        let ty = forall(
            "A_n2",
            forall(
                "A",
                product(path("X"), product(path("A_n2"), path("A"))),
                sp(),
            ),
            sp(),
        );

        let rewritten = subst_type(&ty, &HashMap::from([("X".to_owned(), path("Z"))]));

        let Ty::Forall {
            param: outer, body, ..
        } = rewritten
        else {
            panic!("expected outer forall")
        };
        let Ty::Forall {
            param: inner, body, ..
        } = body.as_ref()
        else {
            panic!("expected inner forall")
        };
        assert_eq!(outer.name, "A_n2");
        assert_eq!(inner.name, "A");
        assert_ne!(outer.name, inner.name);

        let Ty::Product {
            left: replacement,
            right,
            ..
        } = body.as_ref()
        else {
            panic!("expected outer product")
        };
        let Ty::Product {
            left: outer_reference,
            right: inner_reference,
            ..
        } = right.as_ref()
        else {
            panic!("expected nested product")
        };
        assert_path_name(replacement, "Z");
        assert_path_name(outer_reference, &outer.name);
        assert_path_name(inner_reference, &inner.name);
    }

    fn assert_path_name(ty: &Ty, expected: &str) {
        assert!(
            matches!(ty, Ty::Path { segments, .. } if segments.len() == 1 && segments[0].as_str() == expected),
            "expected path `{expected}`, found `{}`",
            display_type(ty)
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn canonical_bound_names_ignore_source_spelling_and_preserve_handle_provenance() {
        let source_x = super::super::InternedType::fresh(forall(
            "X",
            func(path("X"), path("X")),
            Span::new(1, 2),
        ));
        let source_z = super::super::InternedType::fresh_canonical(forall(
            "Z",
            func(path("Z"), path("Z")),
            Span::new(3, 4),
        ));
        let target = Span::new(50, 60);

        let normalized_x = canonicalize_bound_type_names_at(&source_x, target);
        let normalized_z = canonicalize_bound_type_names_at(&source_z, target);
        let normalized_x_again = canonicalize_bound_type_names_at(&source_x, target);

        assert_eq!(display_type(normalized_x.as_type()), "[A] A -> A");
        assert_eq!(normalized_x.as_type(), normalized_z.as_type());
        assert!(!normalized_x.identity_is_canonical());
        assert!(normalized_z.identity_is_canonical());
        assert!(!normalized_x.ptr_eq(&source_x));
        assert!(!normalized_x.ptr_eq(&normalized_x_again));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn canonical_bound_names_reserve_free_names_and_follow_shadowing() {
        let nested = forall(
            "X",
            product(
                product(path("A"), path("A_n2")),
                product(forall("X", path("X"), Span::new(5, 6)), path("X")),
            ),
            Span::new(1, 2),
        );
        let normalized = canonicalize_bound_type_names_at(
            &super::super::InternedType::fresh(nested),
            Span::new(70, 80),
        );

        let Ty::Forall {
            param: outer, body, ..
        } = normalized.as_type()
        else {
            panic!("expected outer forall")
        };
        assert_eq!(outer.name, "A_n3");
        let Ty::Product {
            left: free_names,
            right: nested_and_outer,
            ..
        } = body.as_ref()
        else {
            panic!("expected outer product")
        };
        let Ty::Product {
            left: free_a,
            right: free_a_2,
            ..
        } = free_names.as_ref()
        else {
            panic!("expected free-name product")
        };
        assert_path_name(free_a, "A");
        assert_path_name(free_a_2, "A_n2");
        let Ty::Product {
            left: nested,
            right: outer_use,
            ..
        } = nested_and_outer.as_ref()
        else {
            panic!("expected nested-binder product")
        };
        let Ty::Forall {
            param: inner,
            body: inner_use,
            ..
        } = nested.as_ref()
        else {
            panic!("expected inner forall")
        };
        assert_eq!(inner.name, "A_n4");
        assert_path_name(inner_use, "A_n4");
        assert_path_name(outer_use, "A_n3");
    }

    #[cfg(feature = "surface")]
    fn assert_all_lowered_type_spans(ty: &Ty, expected: Span) {
        assert_eq!(ty.span(), expected);
        match ty {
            Ty::Path { segments, args, .. } => {
                assert!(segments.iter().all(|segment| segment.span == expected));
                for arg in args {
                    assert_all_lowered_type_spans(arg, expected);
                }
            }
            Ty::Function { param, ret, .. } => {
                assert_all_lowered_type_spans(param, expected);
                assert_all_lowered_type_spans(ret, expected);
            }
            Ty::Product { left, right, .. } | Ty::Sum { left, right, .. } => {
                assert_all_lowered_type_spans(left, expected);
                assert_all_lowered_type_spans(right, expected);
            }
            Ty::Goal { args, .. } => {
                for arg in args {
                    assert_all_lowered_type_spans(arg, expected);
                }
            }
            Ty::Forall { param, body, .. } => {
                assert_eq!(param.span, expected);
                assert_all_lowered_type_spans(body, expected);
            }
            Ty::Unit { .. } | Ty::Bottom { .. } | Ty::Infer { .. } => {}
            Ty::LabelSugar { ext, .. } => match *ext {},
        }
    }

    #[cfg(feature = "surface")]
    #[test]
    fn canonical_bound_names_stamp_every_type_position_and_preserve_metadata() {
        let original_goal = crate::ast::TypeGoalRef::for_test(7, 8, 9);
        let original = Ty::Forall {
            param: crate::ast::TypeParam {
                name: "F".to_owned(),
                span: Span::new(1, 2),
                kind: Some(crate::ast::Kind::arrow_chain(2)),
            },
            body: Box::new(Ty::Function {
                param: Box::new(product(
                    Ty::Unit {
                        meta: Meta::new(Span::new(3, 4)),
                    },
                    Ty::Bottom {
                        meta: Meta::new(Span::new(5, 6)),
                    },
                )),
                ret: Box::new(Ty::Sum {
                    left: Box::new(Ty::Path {
                        segments: vec![
                            crate::ast::PathSegment::new("pkg".to_owned(), Span::new(7, 8)),
                            crate::ast::PathSegment::new("T".to_owned(), Span::new(9, 10)),
                        ],
                        args: vec![Ty::Goal {
                            goal: original_goal,
                            args: vec![applied("F", vec![path("Free")])],
                            meta: Meta::new(Span::new(11, 12)),
                            ext: (),
                        }],
                        meta: Meta::new(Span::new(13, 14)),
                    }),
                    right: Box::new(Ty::Infer {
                        meta: Meta::new(Span::new(15, 16)),
                        ext: (),
                    }),
                    meta: Meta::new(Span::new(17, 18)),
                }),
                meta: Meta::new(Span::new(19, 20)),
                abi_arity: 7,
                caps: (),
            }),
            meta: Meta::new(Span::new(21, 22)),
        };
        let target = Span::new(90, 100);
        let normalized =
            canonicalize_bound_type_names_at(&super::super::InternedType::fresh(original), target);

        assert_all_lowered_type_spans(normalized.as_type(), target);
        let Ty::Forall { param, body, .. } = normalized.as_type() else {
            panic!("expected forall")
        };
        assert_eq!(param.name, "A");
        assert_eq!(param.kind, Some(crate::ast::Kind::arrow_chain(2)));
        let Ty::Function { ret, abi_arity, .. } = body.as_ref() else {
            panic!("expected function")
        };
        assert_eq!(*abi_arity, 7);
        let Ty::Sum { left, .. } = ret.as_ref() else {
            panic!("expected sum")
        };
        let Ty::Path { segments, args, .. } = left.as_ref() else {
            panic!("expected qualified path")
        };
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.as_str())
                .collect::<Vec<_>>(),
            ["pkg", "T"]
        );
        let Ty::Goal { goal, args, .. } = &args[0] else {
            panic!("expected goal argument")
        };
        assert_eq!(*goal, original_goal);
        let Ty::Path {
            segments,
            args: applied_args,
            ..
        } = &args[0]
        else {
            panic!("expected applied bound HKT head")
        };
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].as_str(), "A");
        assert_eq!(applied_args.len(), 1);
        assert_path_name(&applied_args[0], "Free");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn canonical_bound_names_preserve_routed_function_capabilities() {
        use crate::ast::{FnTypeCapabilities, Lifetime, Routed, Type};

        let source = super::super::InternedType::fresh(Type::<Routed>::Function {
            param: Box::new(Type::Unit {
                meta: Meta::new(Span::new(1, 2)),
            }),
            ret: Box::new(Type::Unit {
                meta: Meta::new(Span::new(3, 4)),
            }),
            meta: Meta::new(Span::new(5, 6)),
            abi_arity: 9,
            caps: FnTypeCapabilities {
                lifetime: Lifetime::Stack,
            },
        });
        let target = Span::new(110, 120);
        let normalized = canonicalize_bound_type_names_at(&source, target);

        let Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            caps,
        } = normalized.as_type()
        else {
            panic!("expected routed function")
        };
        assert_eq!(*abi_arity, 9);
        assert_eq!(caps.lifetime, Lifetime::Stack);
        assert_eq!(meta.span, target);
        assert_eq!(param.span(), target);
        assert_eq!(ret.span(), target);
    }

    #[test]
    fn type_expression_function_abi_is_recursive_and_unit_sensitive() {
        let unit = Ty::Unit {
            meta: Meta::new(sp()),
        };
        let nested_unit_function = Ty::Function {
            param: Box::new(unit.clone()),
            ret: Box::new(path("R")),
            meta: Meta::new(sp()),
            abi_arity: 9,
            caps: (),
        };
        let mut ty = forall(
            "X",
            Ty::Function {
                param: Box::new(product(path("X"), path("X"))),
                ret: Box::new(applied("Box", vec![nested_unit_function])),
                meta: Meta::new(sp()),
                abi_arity: 7,
                caps: (),
            },
            sp(),
        );

        super::super::canonicalize_type_expression_function_abi(&mut ty);

        let Ty::Forall { body, .. } = ty else {
            panic!("expected forall")
        };
        let Ty::Function { ret, abi_arity, .. } = body.as_ref() else {
            panic!("expected outer function")
        };
        assert_eq!(*abi_arity, 1);
        let Ty::Path { args, .. } = ret.as_ref() else {
            panic!("expected nested path")
        };
        assert!(matches!(
            args.as_slice(),
            [Ty::Function { abi_arity: 0, .. }]
        ));
    }

    #[test]
    fn fresh_type_var_returns_base_when_uncontested() {
        let taken: HashSet<String> = HashSet::new();
        assert_eq!(fresh_type_var("A", &taken), "A");
    }

    #[test]
    fn fresh_type_var_skips_single_collision() {
        let taken: HashSet<String> = ["A".to_owned()].into_iter().collect();
        assert_eq!(fresh_type_var("A", &taken), "A_n2");
    }

    #[test]
    fn fresh_type_var_advances_past_two_collisions() {
        // Base `A` and its first suffixed candidate `A_n2` are both
        // taken, so the counter must climb to `A_n3`. Pins both the
        // suffix-increment direction and the base-return fast path.
        let taken: HashSet<String> = ["A".to_owned(), "A_n2".to_owned()].into_iter().collect();
        assert_eq!(fresh_type_var("A", &taken), "A_n3");
    }

    #[test]
    fn fresh_type_var_accepts_a_borrowed_exact_membership_view() {
        let taken: HashSet<String> = ["A".to_owned(), "A_n2".to_owned()].into_iter().collect();
        assert_eq!(
            fresh_type_var_with("A", |candidate| taken.contains(candidate)),
            "A_n3"
        );
    }

    #[test]
    fn write_type_separates_multiple_path_args_with_commas() {
        // `Foo(A, B, C)` renders each argument after the first behind a
        // `", "` separator; a mis-indexed guard would front-load or drop
        // the separators.
        assert_eq!(
            display_type(&applied("Foo", vec![path("A"), path("B"), path("C")])),
            "Foo(A, B, C)"
        );
    }

    #[test]
    fn write_function_domain_product_needs_no_parens() {
        // A product / sum function domain renders bare (no wrapping
        // parens), unlike a function- or forall-domain.
        assert_eq!(
            display_type(&func(product(path("A"), path("B")), path("C"))),
            "(A & B) -> C"
        );
        assert_eq!(
            display_type(&func(sum(path("A"), path("B")), path("C"))),
            "(A | B) -> C"
        );
    }
}
