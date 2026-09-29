//! Type synthesis: per-expression and per-item walks that compute
//! [`Synth`] results (the type of an expression, its value/scheme
//! classification, and any inference-driven constructor suffix).
//!
//! The shared synth entries — `synth_top_fn_def`, `synth_env_fn_def`,
//! `newtype_member_scheme`, `synth_qualified_env_member` — produce `Synth` values for
//! top-level references. Per-expression helpers (`synth_path`,
//! `synth_let`, `synth_seq`, `synth_literal`, `synth_value_arg`,
//! `synth_fn`) walk the body and synthesize types.
//! [`check_fn_against`] is the bidirectional companion: it
//! binds each `fn` parameter at the corresponding expected type and routes
//! the body through `<P::Typer as Typer<P>>::check_value_against`.
//!
//! The closed structural helper [`unify_pattern`] lives here too. Direct
//! application and several other type relations use it; the surface retained
//! application path keeps its finite unknowns in the owner-scoped goal store.
//!
//! One intrinsic scheme builder closes out the file:
//! [`intrinsic_scheme`], which produces the canonical scheme
//! [`Synth`] for the `__intrinsic__` family. Module typing retains the
//! complete exact-role resolution so `__if_then_else__` can distinguish a
//! missing Boolean role from an ambiguous one; callers that already selected
//! singleton roles use the public map-based wrapper.
//!
//! Extracted from [`super::typecheck_core`] for navigability;
//! depends on the rest of the umbrella for the apply / alias /
//! type-builder machinery.

use std::collections::HashMap;

use crate::ast::Meta;
use crate::comptime::ComptimeBuiltin;
use crate::error::Error;
use crate::span::Span;

use super::types::{collect_free_type_vars, fresh_type_var};
use super::{
    AliasCtx, ModuleEnv, Synth, TypeCtx, Typer, TyperPhase, clone_with_span, display_type,
    subst_type, tp, ty_func, ty_nominal_segments, ty_path, ty_product, ty_sum, vp,
};

// =========================================================================
// Pattern unification (for inferred type-args at a polymorphic call)
// =========================================================================

/// Structural unification of a value-param type pattern against a
/// synthesized arg type. When the pattern is a single-segment
/// `Type::Path` whose head names a type-param, bind it (or check
/// consistency with a prior binding). On other shapes, walk both
/// sides in lockstep and recurse; type-checks ([`require_type_equiv`]) are
/// the fallback for non-pattern positions.
///
/// Both sides are unfolded at the alias boundary first so an
/// alias hiding a type-param structure is matched correctly.
/// `newtype` boundaries and `host type` opacity stop the walk —
/// those are treated as nominal atoms.
///
/// Phase-polymorphic with `P::TypeLabelSugar = Never`.
pub fn unify_pattern<P>(
    pat: &crate::ast::Type<P>,
    target: &crate::ast::Type<P>,
    type_params: &std::collections::HashSet<String>,
    subst: &mut HashMap<String, crate::ast::Type<P>>,
    span: Span,
    ctx: &AliasCtx<'_, '_, P>,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    unify_pattern_with_holes_state(
        pat,
        target,
        type_params,
        &std::collections::HashSet::new(),
        subst,
        span,
        ctx,
        false,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn unify_pattern_with_holes_state<P>(
    pat: &crate::ast::Type<P>,
    target: &crate::ast::Type<P>,
    type_params: &std::collections::HashSet<String>,
    target_holes: &std::collections::HashSet<String>,
    subst: &mut HashMap<String, crate::ast::Type<P>>,
    span: Span,
    ctx: &AliasCtx<'_, '_, P>,
    pat_is_canonical: bool,
    target_is_canonical: bool,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    unify_pattern_with_holes_source(
        pat,
        target,
        type_params,
        target_holes,
        subst,
        span,
        ctx,
        pat_is_canonical,
        target_is_canonical,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn unify_pattern_with_holes_source<P>(
    pat: &crate::ast::Type<P>,
    target: &crate::ast::Type<P>,
    type_params: &std::collections::HashSet<String>,
    target_holes: &std::collections::HashSet<String>,
    subst: &mut HashMap<String, crate::ast::Type<P>>,
    span: Span,
    ctx: &AliasCtx<'_, '_, P>,
    pat_is_canonical: bool,
    target_is_canonical: bool,
    requirement_source: Option<&super::intern::RequirementSource>,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    // The binders being solved are in scope for every head
    // canonicalization below: a bare scheme binder is a type variable, so
    // head qualification must leave it bare (else a binder shadowing a
    // same-named module type — e.g. a label-generated alias — would
    // resolve to that type and this unifier's binder-bind arm would never
    // fire). Re-scope the alias context with the binder set; the
    // structural recursion and the `require_type_equiv` fallbacks below
    // inherit it.
    let mut all_binders = ctx.binder_local_names();
    all_binders.extend(type_params.iter().cloned());
    let binder_ctx = AliasCtx {
        binder_locals: Some(&all_binders),
        ..*ctx
    };
    let ctx = &binder_ctx;
    // Canonicalize nominal heads to `(module, name)` so the
    // same-nominal-head arms below fire when one side is bare (a scheme
    // signature, a user type-arg) and the other qualified (a synthesized
    // self-type) — otherwise the segment compare misses and a brand
    // binder never gets solved, surfacing as a spurious mismatch.
    let (pat, pat_is_canonical) =
        super::aliases::canonicalize_for_comparison(pat, ctx, pat_is_canonical);
    let (target, target_is_canonical) =
        super::aliases::canonicalize_for_comparison(target, ctx, target_is_canonical);
    // Defensive: `Type::Infer` is the surface `_` placeholder.
    // It should never reach `unify_pattern` because (a) scheme
    // patterns come from validated `fn` / intrinsic / `fn`-
    // expression signatures (validated by `check_well_formed_type`
    // at top-level positions and by the lambda-signature validators),
    // and (b) target types come from `synth_value_arg`, which never
    // returns a placeholder. A reachable arm here means an upstream
    // contract was broken.
    if matches!(pat, crate::ast::Type::Infer { .. }) {
        unreachable!(
            "unify_pattern: `Type::Infer` in pattern position — \
             scheme parameters never carry placeholders. \
             Check `check_well_formed_type` / the lambda-signature validators."
        );
    }
    if matches!(target, crate::ast::Type::Infer { .. }) {
        unreachable!(
            "unify_pattern: `Type::Infer` in target position — \
             synthesized value-arg types must be concrete."
        );
    }
    // A fresh outer-application hole contributes no constraint to an
    // inner binder at this leaf. Composite targets still recurse so
    // their known leaves remain available.
    if !target_holes.is_empty()
        && let crate::ast::Type::Path { segments, args, .. } = &target
        && segments.len() == 1
        && args.is_empty()
        && target_holes.contains(segments[0].as_str())
    {
        return Ok(());
    }
    // Pattern is a single-segment type-param reference → bind.
    if let crate::ast::Type::Path {
        segments,
        args: pat_args,
        ..
    } = &pat
        && segments.len() == 1
        && pat_args.is_empty()
        && type_params.contains(segments[0].as_str())
    {
        let name = segments[0].name.clone();
        if let Some(existing) = subst.get(&name) {
            super::aliases::require_type_equiv_state(
                &target,
                existing,
                span,
                ctx,
                target_is_canonical,
                true,
            )?;
        } else {
            subst.insert(name, target.clone());
        }
        return Ok(());
    }
    // Otherwise recurse structurally where possible; fall back to
    // require_type_equiv for everything else (catches parameterized
    // Path mismatches, Rec boundaries, and so on).
    match (&pat, &target) {
        (
            crate::ast::Type::Function {
                param: p_param,
                ret: p_ret,
                ..
            },
            crate::ast::Type::Function {
                param: t_param,
                ret: t_ret,
                ..
            },
        ) => {
            unify_pattern_with_holes_source(
                p_param,
                t_param,
                type_params,
                target_holes,
                subst,
                span,
                ctx,
                pat_is_canonical,
                target_is_canonical,
                requirement_source,
            )?;
            unify_pattern_with_holes_source(
                p_ret,
                t_ret,
                type_params,
                target_holes,
                subst,
                span,
                ctx,
                pat_is_canonical,
                target_is_canonical,
                requirement_source,
            )
        }
        (
            crate::ast::Type::Product {
                left: pl,
                right: pr,
                ..
            },
            crate::ast::Type::Product {
                left: tl,
                right: tr,
                ..
            },
        ) => {
            unify_pattern_with_holes_source(
                pl,
                tl,
                type_params,
                target_holes,
                subst,
                span,
                ctx,
                pat_is_canonical,
                target_is_canonical,
                requirement_source,
            )?;
            unify_pattern_with_holes_source(
                pr,
                tr,
                type_params,
                target_holes,
                subst,
                span,
                ctx,
                pat_is_canonical,
                target_is_canonical,
                requirement_source,
            )
        }
        (
            crate::ast::Type::Sum {
                left: pl,
                right: pr,
                ..
            },
            crate::ast::Type::Sum {
                left: tl,
                right: tr,
                ..
            },
        ) => {
            unify_pattern_with_holes_source(
                pl,
                tl,
                type_params,
                target_holes,
                subst,
                span,
                ctx,
                pat_is_canonical,
                target_is_canonical,
                requirement_source,
            )?;
            unify_pattern_with_holes_source(
                pr,
                tr,
                type_params,
                target_holes,
                subst,
                span,
                ctx,
                pat_is_canonical,
                target_is_canonical,
                requirement_source,
            )
        }
        (
            crate::ast::Type::Path {
                segments: p_segs,
                args: p_args,
                ..
            },
            crate::ast::Type::Path {
                segments: t_segs,
                args: t_args,
                ..
            },
        ) if t_segs.len() == 1
            && target_holes.contains(t_segs[0].as_str())
            && p_args.len() == t_args.len() =>
        {
            // The target's applied head is an unresolved outer hole.
            // Ignore only that head and retain every argument constraint.
            for (pa, ta) in p_args.iter().zip(t_args) {
                unify_pattern_with_holes_source(
                    pa,
                    ta,
                    type_params,
                    target_holes,
                    subst,
                    span,
                    ctx,
                    pat_is_canonical,
                    target_is_canonical,
                    requirement_source,
                )?;
            }
            Ok(())
        }
        (
            crate::ast::Type::Path {
                segments: p_segs,
                args: p_args,
                ..
            },
            crate::ast::Type::Path {
                segments: t_segs,
                args: t_args,
                ..
            },
        ) if p_segs.len() == 1
            && !p_args.is_empty()
            && type_params.contains(p_segs[0].as_str())
            && p_args.len() == t_args.len() =>
        {
            // Abstract higher-kinded brand application: the pattern
            // `F(A)` has a kind-`*→*` binder `F` in head position,
            // matched against a concrete brand application `Box(T)`.
            // Bind the head binder `f := Box` (the target's nominal
            // head — its full identity-exact `(module, name)` path now
            // that brand heads are qualified), then unify each
            // argument slot. This is the inference rule that lets a
            // generic-over-`[*F]` fn resolve `f` from a concrete call
            // site.
            let head = p_segs[0].name.clone();
            let target_head = crate::ast::Type::Path {
                segments: t_segs.clone(),
                args: Vec::new(),
                meta: crate::ast::Meta::new(span),
            };
            if let Some(existing) = subst.get(&head) {
                super::aliases::require_type_equiv_state(
                    &target_head,
                    existing,
                    span,
                    ctx,
                    true,
                    true,
                )?;
            } else {
                subst.insert(head, target_head);
            }
            for (pa, ta) in p_args.iter().zip(t_args) {
                unify_pattern_with_holes_source(
                    pa,
                    ta,
                    type_params,
                    target_holes,
                    subst,
                    span,
                    ctx,
                    pat_is_canonical,
                    target_is_canonical,
                    requirement_source,
                )?;
            }
            Ok(())
        }
        (crate::ast::Type::Unit { .. }, crate::ast::Type::Unit { .. })
        | (crate::ast::Type::Bottom { .. }, crate::ast::Type::Bottom { .. }) => Ok(()),
        (
            crate::ast::Type::Path {
                segments: p_segs,
                args: p_args,
                ..
            },
            crate::ast::Type::Path {
                segments: t_segs,
                args: t_args,
                ..
            },
        ) if p_segs == t_segs && p_args.len() == t_args.len() => {
            // Same nominal head — recurse into args (each may contain
            // type-param positions).
            for (pa, ta) in p_args.iter().zip(t_args) {
                unify_pattern_with_holes_source(
                    pa,
                    ta,
                    type_params,
                    target_holes,
                    subst,
                    span,
                    ctx,
                    pat_is_canonical,
                    target_is_canonical,
                    requirement_source,
                )?;
            }
            Ok(())
        }
        (
            crate::ast::Type::Forall {
                param: p_param,
                body: p_body,
                ..
            },
            crate::ast::Type::Forall {
                param: t_param,
                body: t_body,
                ..
            },
        ) => {
            if p_param.effective_kind() != t_param.effective_kind() {
                return Err(super::aliases::type_mismatch_error_state(
                    &target,
                    &pat,
                    span,
                    ctx,
                    target_is_canonical,
                    pat_is_canonical,
                ));
            }
            // Compare under one fresh rigid binder. Reusing either source
            // spelling can capture an outer inference variable with the same
            // name and turn a bound occurrence into a constraint on it.
            let mut taken = std::collections::HashSet::new();
            collect_free_type_vars(p_body, &mut taken);
            collect_free_type_vars(t_body, &mut taken);
            taken.extend(type_params.iter().cloned());
            taken.extend(target_holes.iter().cloned());
            taken.extend(ctx.binder_local_names());
            taken.extend(subst.keys().cloned());
            for ty in subst.values() {
                collect_free_type_vars(ty, &mut taken);
            }
            taken.insert(p_param.name.clone());
            taken.insert(t_param.name.clone());
            let fresh = fresh_type_var("__alpha__", &taken);

            let p_rename = HashMap::from([(
                p_param.name.clone(),
                crate::ast::Type::synth_path(vec![fresh.clone()], Vec::new(), p_param.span),
            )]);
            let t_rename = HashMap::from([(
                t_param.name.clone(),
                crate::ast::Type::synth_path(vec![fresh.clone()], Vec::new(), t_param.span),
            )]);
            let p_body_renamed = subst_type(p_body, &p_rename);
            let t_body_renamed = subst_type(t_body, &t_rename);

            let mut nested_type_params = type_params.clone();
            nested_type_params.remove(&p_param.name);
            let mut nested_target_holes = target_holes.clone();
            nested_target_holes.remove(&t_param.name);
            let mut nested_binders = ctx.binder_local_names();
            nested_binders.insert(fresh.clone());
            let nested_ctx = AliasCtx {
                binder_locals: Some(&nested_binders),
                ..*ctx
            };
            let subst_before = subst.clone();
            let result = unify_pattern_with_holes_source(
                &p_body_renamed,
                &t_body_renamed,
                &nested_type_params,
                &nested_target_holes,
                subst,
                span,
                &nested_ctx,
                pat_is_canonical,
                target_is_canonical,
                requirement_source,
            );
            let rigid_escaped = result.is_ok()
                && subst.values().any(|ty| {
                    let mut free = std::collections::HashSet::new();
                    collect_free_type_vars(ty, &mut free);
                    free.contains(&fresh)
                });
            match result {
                Ok(()) if !rigid_escaped => Ok(()),
                Ok(()) | Err(Error::Type(_)) => {
                    // A quantified binder is rigid: it cannot solve an outer
                    // inference variable. Report either kind of failure using
                    // the source Forall types, never the internal alpha name.
                    *subst = subst_before;
                    Err(
                        super::aliases::type_mismatch_error_state_with_binder_lookup(
                            &target,
                            &pat,
                            span,
                            ctx,
                            target_is_canonical,
                            pat_is_canonical,
                            &all_binders,
                            requirement_source,
                        ),
                    )
                }
                Err(error) => {
                    *subst = subst_before;
                    Err(error)
                }
            }
        }
        _ => {
            // Anything else: structurally must match.
            super::aliases::require_type_equiv_state_with_source(
                &target,
                &pat,
                span,
                ctx,
                target_is_canonical,
                pat_is_canonical,
                requirement_source,
            )
        }
    }
}

// =========================================================================
// Top-level item schemes (fn / host fn / newtype member)
// =========================================================================

/// Build a synth result for a top-level `fn` reference, choosing
/// between [`Synth::value`] and [`Synth::scheme`] based on whether
/// the fn carries any type parameters.
///
/// A fn with type-params synthesizes as a polymorphic scheme; a
/// monomorphic fn synthesizes as a function type so the callee
/// can flow into both call positions and `fn`-shaped contexts.
///
/// Phase-polymorphic via the same `Clone` bound as the rest of the
/// builders.
pub fn synth_top_fn_def<P>(d: &crate::ast::FnDef<P>, span: Span) -> Synth<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let has_type_params = d.sig.has_type_params();
    let ty = d.sig.signature_ty(d.ret.clone(), span);
    if has_type_params {
        Synth::complete_scheme(ty)
    } else {
        Synth::value(ty)
    }
}

/// [`synth_top_fn_def`] for a fn whose declaration lives in *another*
/// module: qualify the signature's nominal heads against the **declaring**
/// module before building the scheme.
///
/// [`synth_top_fn_def`] leaves the raw AST signature types, which works for
/// a same-module fn because the identity-exact canonicalizer in
/// [`crate::pass::typecheck_core::aliases`] later re-qualifies bare nominal
/// heads in the *checking* module — and for a same-module fn the checking
/// module *is* the declaring one. A cross-module fn breaks that assumption:
/// its bare `Meters` would canonicalize in the **caller**, conflating two
/// distinct same-named newtypes from different packages. Qualifying here in
/// the declaring module gives every nominal head its `(module, name)` form
/// up front; the later canonicalization is then idempotent (qualifying an
/// already-qualified multi-segment head is a no-op, see
/// [`crate::pass::resolve::qualify_type_segments_in_module`]).
///
/// Open-world: the qualification resolves names through the **declaring**
/// module's own imports and local declarations — never the caller's. Adding
/// a declaration to any *other* module therefore cannot change what this
/// signature qualifies to, exactly as for [`exported_contract_type`]
/// applied to the same module's exports.
pub fn synth_top_fn_def_in_module<P>(
    d: &crate::ast::FnDef<P>,
    declaring_module: &crate::ast::Module<P>,
    package: Option<&crate::pass::resolve::Package<P>>,
    span: Span,
) -> Synth<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let (sig, ret) = crate::pass::resolve::qualify_contract_fn_signature_and_ret_in_module(
        &d.sig,
        &d.ret,
        declaring_module,
    );
    let has_type_params = sig.has_type_params();
    let ty = super::aliases::follow_type_reexports_deep(&sig.signature_ty(ret, span), package);
    let result = if has_type_params {
        Synth::canonical_complete_scheme(ty)
    } else {
        Synth::canonical_value(ty)
    };
    let source = package
        .and_then(|package| package.module(&declaring_module.path.segments.join("/")))
        .map(|entry| {
            std::sync::Arc::new(super::intern::RequirementSource {
                file: entry.file_path.clone(),
                span: Span::new(d.meta.span.start, d.ret.span().end),
            })
        });
    Synth {
        ty: result.ty.with_requirement_source(source),
        ..result
    }
}

fn host_env_fn_in_module<'m, P>(
    module: &'m crate::ast::Module<P>,
    name: &str,
) -> Option<&'m crate::ast::HostFn<P>>
where
    P: crate::ast::Phase,
{
    module.items.iter().find_map(|item| match item {
        crate::ast::Item::HostFn(f) if f.name == name => Some(f),
        _ => None,
    })
}

/// Build a synth result for a `host fn` reference. Anonymous value
/// parameters are renamed to `_p0`, `_p1`, … so the typer's value-arg
/// matching at the call site has stable names; spans on every
/// signature element are aliased to the call site so diagnostics
/// surface there rather than at the package-file declaration.
///
/// Phase-polymorphic with the same `TypeLabelSugar = Never` bound
/// as [`clone_with_span`].
pub fn synth_env_fn_def<P>(d: &crate::ast::HostFn<P>, span: Span) -> Synth<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let signature_params: Vec<crate::ast::SignatureParam<P>> = d
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| match p {
            crate::ast::HostFnParam::Type(tp_) => {
                crate::ast::SignatureParam::Type(crate::ast::TypeParam {
                    name: tp_.name.clone(),
                    span,
                    kind: tp_.kind.clone(),
                })
            }
            crate::ast::HostFnParam::Value(vp_) => {
                crate::ast::SignatureParam::Value(crate::ast::Param {
                    name: vp_.name.clone().unwrap_or_else(|| format!("_p{i}")),
                    ty: Some(clone_with_span(&vp_.ty, span)),
                    pattern: Default::default(),
                    meta: Meta::new(span),
                })
            }
        })
        .collect();
    let ret = clone_with_span(&d.ret, span);
    let has_type_params = signature_params
        .iter()
        .any(|p| matches!(p, crate::ast::SignatureParam::Type(_)));
    let sig = crate::ast::Signature::from_parts(signature_params, d.param_groups.clone());
    let ty = crate::ast::Type::synth_scheme_from_signature(&sig, ret, span);
    if has_type_params {
        Synth::complete_scheme(ty)
    } else {
        Synth::value(ty)
    }
}

/// [`synth_env_fn_def`] for a `host fn` whose declaration lives in
/// *another* module: qualify the signature's nominal heads against the
/// **declaring** module before building the scheme.
///
/// A cross-module host fn's signature types are written in *its* module
/// (`fmt`'s `host fn i32_to_string(I32) -> String` spells the `I32` /
/// `String` it imports from `testapi`). [`synth_env_fn_def`] leaves them
/// raw, so the identity-exact canonicalizer would re-qualify a bare
/// `I32` in the **consumer**, conflating it with the consumer's own
/// same-named type. Qualifying here in the declaring module gives every
/// nominal head its `(module, name)` form up front — matching what the
/// Rust emitter and the `sig` recorder already do for a host fn's
/// signature (`host_fn_signature` qualifies via the defining module).
///
/// Open-world: the qualification resolves names through the **declaring**
/// module's own imports — never the caller's.
pub fn synth_env_fn_def_in_module<P>(
    d: &crate::ast::HostFn<P>,
    declaring_module: &crate::ast::Module<P>,
    package: Option<&crate::pass::resolve::Package<P>>,
    span: Span,
) -> Synth<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let mut binder_kinds = std::collections::HashMap::new();
    let signature_params: Vec<crate::ast::SignatureParam<P>> = d
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| match p {
            crate::ast::HostFnParam::Type(tp_) => {
                binder_kinds.insert(tp_.name.clone(), tp_.effective_kind());
                crate::ast::SignatureParam::Type(crate::ast::TypeParam {
                    name: tp_.name.clone(),
                    span,
                    kind: tp_.kind.clone(),
                })
            }
            crate::ast::HostFnParam::Value(vp_) => {
                crate::ast::SignatureParam::Value(crate::ast::Param {
                    name: vp_.name.clone().unwrap_or_else(|| format!("_p{i}")),
                    ty: Some(crate::pass::resolve::qualify_contract_type_in_module(
                        &clone_with_span(&vp_.ty, span),
                        declaring_module,
                        &binder_kinds,
                    )),
                    pattern: Default::default(),
                    meta: Meta::new(span),
                })
            }
        })
        .collect();
    let ret = crate::pass::resolve::qualify_contract_type_in_module(
        &clone_with_span(&d.ret, span),
        declaring_module,
        &binder_kinds,
    );
    let has_type_params = signature_params
        .iter()
        .any(|p| matches!(p, crate::ast::SignatureParam::Type(_)));
    let sig = crate::ast::Signature::from_parts(signature_params, d.param_groups.clone());
    let ty = super::aliases::follow_type_reexports_deep(
        &crate::ast::Type::synth_scheme_from_signature(&sig, ret, span),
        package,
    );
    let synth = if has_type_params {
        Synth::complete_scheme(ty)
    } else {
        Synth::value(ty)
    };
    synth.with_canonical_identity()
}

/// Project the function param-type onto N fn-value-param bindings,
/// per the right-fold rule on multi-value-param `fn` signatures.
/// Walks N-1 `Product(L, R)` layers from the left and yields each
/// `L`; the Nth binding is the remaining type (which may itself be
/// a `Product` if the user named fewer params than the full
/// right-spine, or a terminal type otherwise).
///
/// Returns `None` if N exceeds the spine length (the user wrote more
/// `fn` params than the type can accommodate). For N=0, returns an
/// empty vec; the caller is responsible for verifying that the type
/// is `Unit` in the 0-param case (the canonical "no value args"
/// shape).
pub(super) fn collect_fn_param_bindings_interned<P>(
    param_ty: &super::InternedType<P>,
    n: usize,
    ctx: &AliasCtx<'_, '_, P>,
) -> Option<Vec<super::InternedType<P>>>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    if n == 0 {
        return Some(Vec::new());
    }
    let mut out = Vec::with_capacity(n);
    let mut cur = param_ty.clone();
    for i in 0..n {
        if i + 1 == n {
            out.push(cur);
            return Some(out);
        }
        let (unfolded, canonical) = super::aliases::canonicalize_for_comparison(
            cur.as_type(),
            ctx,
            cur.identity_is_canonical(),
        );
        match unfolded {
            crate::ast::Type::Product { left, right, .. } => {
                out.push(super::InternedType::fresh_with_identity(*left, canonical));
                cur = super::InternedType::fresh_with_identity(*right, canonical);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Build the scheme for a newtype's constructor or projector member.
/// Returns `Err` if `member` is neither.
///
/// The constructor's scheme always takes the newtype's universals
/// **plus** existentials as additional inference-driven type-params
/// — the existentials are recovered from the value-arg's structure
/// at the construction call site, then hidden in the result type.
///
/// The projector's scheme depends on whether the newtype declares
/// existential binders:
/// - **No existentials**: direct-return form
///   `[Universals] Name(Universals) -> payload`.
/// - **With existentials**: CPS form
///   `[Universals] Name(Universals) -> [R] ([Existentials] payload -> R) -> R`.
///   The continuation's universal-binder list scopes the existential
///   witnesses, which therefore cannot leak into the result `r` by
///   ordinary System F scope rules — the same escape discipline the
///   existential's CPS continuation enforces, baked into the
///   projector itself. Callers consume the result either via the
///   `let .(<U> x) = e;` sugar (which appends the continuation
///   automatically) or by writing the call out explicitly.
///
/// Phase-polymorphic with the same `TypeLabelSugar = Never` bound
/// as [`clone_with_span`].
pub fn newtype_member_scheme<P>(
    d: &crate::ast::Newtype<P>,
    member: &str,
    nominal_segments: &[crate::ast::PathSegment],
    span: Span,
) -> Result<Synth<P>, Error>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let payload = clone_with_span(&d.payload, span);
    newtype_member_scheme_with_payload(d, member, nominal_segments, payload, span)
}

/// [`newtype_member_scheme`] for a newtype whose declaration lives in
/// *another* module: qualify the member payload's nominal heads against
/// the **declaring** module before building the scheme.
///
/// [`newtype_member_scheme`] leaves the raw AST payload, which works for
/// a same-module newtype because the identity-exact canonicalizer in
/// [`crate::pass::typecheck_core::aliases`] later re-qualifies bare
/// nominal heads in the *checking* module — and for a same-module
/// newtype the checking module *is* the declaring one. A cross-module
/// newtype breaks that assumption: a bare `String` in a dependency's
/// `pub newtype Token : String` payload would canonicalize in the
/// **caller**, conflating two distinct same-named newtypes. Qualifying
/// the payload here in the declaring module gives every nominal head its
/// `(module, name)` form up front; the later canonicalization is then
/// idempotent (qualifying an already-qualified multi-segment head is a
/// no-op).
///
/// This mirrors what the Rust emitter and the `sig` recorder already do:
/// both qualify a newtype's payload in the newtype's own module entry
/// (`exported_contract_type(&newtype.payload, module_entry, …)`), so the
/// typer now reaches the same identity-exact payload they do.
///
/// Open-world: the qualification resolves names through the **declaring**
/// module's own imports and local declarations — never the caller's, so
/// the same safety property as [`crate::pass::resolve::exported_contract_type`]
/// applied to that module's own exports.
pub fn newtype_member_scheme_in_module<P>(
    d: &crate::ast::Newtype<P>,
    member: &str,
    nominal_segments: &[crate::ast::PathSegment],
    declaring_module: &crate::ast::Module<P>,
    package: Option<&crate::pass::resolve::Package<P>>,
    span: Span,
) -> Result<Synth<P>, Error>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    // The newtype's universal + existential binders are in scope inside
    // its payload, so they must not be mistaken for nominal heads during
    // qualification.
    let mut locals = std::collections::HashMap::new();
    for tp in d.type_params.iter().chain(d.existential_params.iter()) {
        locals.insert(tp.name.clone(), tp.effective_kind());
    }
    let payload = crate::pass::resolve::qualify_contract_type_in_module(
        &clone_with_span(&d.payload, span),
        declaring_module,
        &locals,
    );
    let synth = newtype_member_scheme_with_payload(d, member, nominal_segments, payload, span)?;
    let inferred_type_arg_suffix = synth.inferred_type_arg_suffix;
    let ty = super::aliases::follow_type_reexports_deep(synth.ty.as_type(), package);
    Ok((if synth.is_scheme {
        Synth::canonical_complete_scheme(ty)
    } else {
        Synth::canonical_value(ty)
    })
    .with_inferred_type_arg_suffix(inferred_type_arg_suffix))
}

fn newtype_member_scheme_with_payload<P>(
    d: &crate::ast::Newtype<P>,
    member: &str,
    nominal_segments: &[crate::ast::PathSegment],
    payload: crate::ast::Type<P>,
    span: Span,
) -> Result<Synth<P>, Error>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let nominal = ty_nominal_segments::<P>(nominal_segments, &d.type_params, span);

    if member == d.constructor.name {
        // Constructor: existentials behave as additional inference-
        // driven type-params at the call site; the result type carries
        // only the universals.
        let mut params: Vec<crate::ast::SignatureParam<P>> = d
            .type_params
            .iter()
            .map(|tp_| {
                crate::ast::SignatureParam::Type(crate::ast::TypeParam {
                    name: tp_.name.clone(),
                    span,
                    kind: tp_.kind.clone(),
                })
            })
            .collect();
        for tp_ in &d.existential_params {
            params.push(crate::ast::SignatureParam::Type(crate::ast::TypeParam {
                name: tp_.name.clone(),
                span,
                // Existential binders are kind-`*` only.
                kind: None,
            }));
        }
        params.push(vp("payload", payload, span));
        let has_type_params = params
            .iter()
            .any(|p| matches!(p, crate::ast::SignatureParam::Type(_)));
        let ty = crate::ast::Type::synth_scheme_from_signature_params(&params, nominal, span);
        return Ok((if has_type_params {
            Synth::complete_scheme(ty)
        } else {
            Synth::value(ty)
        })
        .with_inferred_type_arg_suffix(d.existential_params.len()));
    }
    if member != d.projector.name {
        return Err(Error::type_(
            span,
            format!(
                "newtype `{}` has no member `{member}` (expected `{}` or `{}`)",
                d.name, d.constructor.name, d.projector.name
            ),
        ));
    }

    // Projector
    let universal_signature: Vec<crate::ast::SignatureParam<P>> = d
        .type_params
        .iter()
        .map(|tp_| {
            crate::ast::SignatureParam::Type(crate::ast::TypeParam {
                name: tp_.name.clone(),
                span,
                kind: tp_.kind.clone(),
            })
        })
        .collect();

    if d.existential_params.is_empty() {
        // Direct-return form for non-existential newtypes.
        let mut params = universal_signature;
        params.push(vp("value", nominal, span));
        let has_type_params = params
            .iter()
            .any(|p| matches!(p, crate::ast::SignatureParam::Type(_)));
        let ty = crate::ast::Type::synth_scheme_from_signature_params(&params, payload, span);
        return Ok(if has_type_params {
            Synth::complete_scheme(ty)
        } else {
            Synth::value(ty)
        });
    }

    // CPS form for existential-bearing newtypes. The projector is
    // **curried**: the outer scheme takes only the universals and the
    // newtype value, and returns the inner polymorphic CPS function
    // `[R]([Existentials] payload -> R) -> R`. This lets
    // the `let .(<U> x) = e;` sugar desugar to `e(_, .[U](x) { rest })`
    // — `e` evaluates to the inner CPS function which is then applied
    // to the continuation.
    //
    // The synthetic result-type binder `__r__` is `__name__`-shaped so
    // it cannot collide with a user-supplied identifier (the parser
    // rejects user identifiers starting with `__`).
    let r_name = "__r__".to_string();
    let r_path = crate::ast::Type::synth_path(vec![r_name.clone()], Vec::new(), span);

    // Continuation type: `[Existentials] payload -> R`.
    let cont_inner_fn = crate::ast::Type::synth_function(vec![payload], r_path.clone(), span);
    let mut cont_ty = cont_inner_fn;
    for tp_ in d.existential_params.iter().rev() {
        cont_ty = crate::ast::Type::Forall {
            param: crate::ast::TypeParam {
                name: tp_.name.clone(),
                span,
                // Existential binders are kind-`*` only.
                kind: None,
            },
            body: Box::new(cont_ty),
            meta: Meta::new(span),
        };
    }

    // Inner CPS function: `[R] cont_ty -> R`.
    let cps_inner_fn = crate::ast::Type::synth_function(vec![cont_ty], r_path.clone(), span);
    let cps_ty = crate::ast::Type::Forall {
        param: crate::ast::TypeParam {
            name: r_name,
            span,
            // The synthetic CPS result binder is an ordinary kind-`*`
            // type variable.
            kind: None,
        },
        body: Box::new(cps_inner_fn),
        meta: Meta::new(span),
    };

    // Outer scheme: `[Universals] Name(Universals) -> CPSType`.
    let mut params = universal_signature;
    params.push(vp("value", nominal, span));
    let has_type_params = params
        .iter()
        .any(|p| matches!(p, crate::ast::SignatureParam::Type(_)));
    let ty = crate::ast::Type::synth_scheme_from_signature_params(&params, cps_ty, span);
    Ok(if has_type_params {
        Synth::complete_scheme(ty)
    } else {
        Synth::value(ty)
    })
}

struct ApplicationShapeHoles<'a, P>
where
    P: crate::ast::Phase,
{
    names: &'a std::collections::HashSet<String>,
    subst: &'a mut HashMap<String, crate::ast::Type<P>>,
}

fn normalize_fn_shape_expected<P>(
    expected: super::InternedType<P>,
    holes: Option<&ApplicationShapeHoles<'_, P>>,
) -> super::InternedType<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let Some(holes) = holes else {
        return expected;
    };
    let mut resolved = expected.clone_type();
    for _ in 0..=holes.subst.len() {
        resolved = subst_type(&resolved, holes.subst);
    }
    super::InternedType::fresh_with_identity(resolved, expected.identity_is_canonical())
}

fn check_planned_fn_shape_annotation<P>(
    annotation: &super::annotation_plan::PlannedHeaderAnnotation<'_, P>,
    expected: &super::InternedType<P>,
    span: Span,
    binder_names: &std::collections::HashSet<String>,
    holes: Option<&mut ApplicationShapeHoles<'_, P>>,
    tcx: &TypeCtx<'_, '_, P>,
) -> Result<(), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    annotation.plan.require_admitted()?;
    assert!(
        annotation.plan.holes().is_empty(),
        "the Kio' header plan retained a source placeholder"
    );
    if let Some(holes) = holes {
        let mut protected = binder_names.clone();
        protected.extend(holes.names.iter().cloned());
        return unify_pattern_with_holes_state(
            expected.as_type(),
            annotation.plan.ty(),
            holes.names,
            &std::collections::HashSet::new(),
            holes.subst,
            span,
            &tcx.binder_alias_ctx(&protected),
            expected.identity_is_canonical(),
            annotation.plan.identity_is_canonical(),
        );
    }
    super::aliases::require_type_equiv_state(
        annotation.plan.ty(),
        expected.as_type(),
        span,
        &tcx.binder_alias_ctx(binder_names),
        annotation.plan.identity_is_canonical(),
        expected.identity_is_canonical(),
    )
}

pub(crate) fn fn_shape_against_source_plan_with_holes<P>(
    plan: &super::annotation_plan::PlannedSourceLambdaHeader<'_, P>,
    expected: &super::InternedType<P>,
    fn_span: Span,
    ambient_binders: &std::collections::HashSet<String>,
    holes: (
        &std::collections::HashSet<String>,
        &mut HashMap<String, crate::ast::Type<P>>,
    ),
    tcx: &TypeCtx<'_, '_, P>,
) -> Result<(super::InternedType<P>, Vec<super::InternedType<P>>), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    fn_shape_against_source_plan(
        plan,
        expected,
        fn_span,
        ambient_binders,
        Some(ApplicationShapeHoles {
            names: holes.0,
            subst: holes.1,
        }),
        tcx,
    )
}

fn fn_shape_against_source_plan<P>(
    plan: &super::annotation_plan::PlannedSourceLambdaHeader<'_, P>,
    expected: &super::InternedType<P>,
    fn_span: Span,
    ambient_binders: &std::collections::HashSet<String>,
    mut holes: Option<ApplicationShapeHoles<'_, P>>,
    tcx: &TypeCtx<'_, '_, P>,
) -> Result<(super::InternedType<P>, Vec<super::InternedType<P>>), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let mut expected_layer = expected.clone();
    let mut binder_names = ambient_binders.clone();
    let mut value_param_types = Vec::new();
    let mut source_value_index = 0usize;
    let implicit_value_group =
        (!plan.signature.has_value_group()).then_some(crate::ast::SignatureGroupRef::Value(&[]));
    for group in plan.group_refs().chain(implicit_value_group) {
        expected_layer = normalize_fn_shape_expected(expected_layer, holes.as_ref());
        match group {
            crate::ast::SignatureGroupRef::Type(params) => {
                for param in params {
                    let crate::ast::SignatureParam::Type(fn_param) = param else {
                        unreachable!("signature type group contains only type params")
                    };
                    let (unfolded, canonical) = super::aliases::canonicalize_for_comparison(
                        expected_layer.as_type(),
                        &tcx.binder_alias_ctx(&binder_names),
                        expected_layer.identity_is_canonical(),
                    );
                    let crate::ast::Type::Forall {
                        param: expected_param,
                        body,
                        ..
                    } = unfolded
                    else {
                        return Err(Error::type_(
                            fn_span,
                            format!(
                                "`fn` declares type parameter `{}` but the expected type has \
                                 no matching forall layer",
                                fn_param.name
                            ),
                        ));
                    };
                    if expected_param.effective_kind() != fn_param.effective_kind() {
                        return Err(Error::type_(
                            fn_param.span,
                            format!(
                                "`fn` type parameter `{}` has kind `{}`, but the matching \
                                 expected forall parameter has kind `{}`",
                                fn_param.name,
                                fn_param.effective_kind(),
                                expected_param.effective_kind()
                            ),
                        ));
                    }
                    expected_layer = super::InternedType::fresh_with_identity(
                        subst_type(
                            &body,
                            &HashMap::from([(
                                expected_param.name,
                                crate::ast::Type::synth_path(
                                    vec![fn_param.name.clone()],
                                    Vec::new(),
                                    fn_param.span,
                                ),
                            )]),
                        ),
                        canonical,
                    );
                    binder_names.insert(fn_param.name.clone());
                }
            }
            crate::ast::SignatureGroupRef::Value(params) => {
                let value_params = params
                    .iter()
                    .map(|param| {
                        let crate::ast::SignatureParam::Value(param) = param else {
                            unreachable!("signature value group contains only value params")
                        };
                        param
                    })
                    .collect::<Vec<_>>();
                let (unfolded, canonical) = super::aliases::canonicalize_for_comparison(
                    expected_layer.as_type(),
                    &tcx.binder_alias_ctx(&binder_names),
                    expected_layer.identity_is_canonical(),
                );
                let crate::ast::Type::Function {
                    param: param_ty,
                    ret: ret_ty,
                    ..
                } = unfolded
                else {
                    return Err(Error::type_(
                        fn_span,
                        format!(
                            "lambda expression used where a function type was expected, but \
                             the expected type is `{}`",
                            display_type(&expected_layer)
                        ),
                    ));
                };
                let param_ty = super::InternedType::fresh_with_identity(*param_ty, canonical);
                let collected = collect_fn_param_bindings_interned(
                    &param_ty,
                    value_params.len(),
                    &tcx.binder_alias_ctx(&binder_names),
                )
                .ok_or_else(|| {
                    Error::type_(
                        fn_span,
                        format!(
                            "`fn` value-parameter count ({}) exceeds the expected function arity \
                             (the param type's right-spine has fewer leaves)",
                            value_params.len()
                        ),
                    )
                })?;
                if value_params.is_empty()
                    && !matches!(
                        super::aliases::unfold_and_qualify_state(
                            param_ty.as_type(),
                            &tcx.binder_alias_ctx(&binder_names),
                            param_ty.identity_is_canonical(),
                        )
                        .0,
                        crate::ast::Type::Unit { .. }
                    )
                {
                    return Err(Error::type_(
                        fn_span,
                        format!(
                            "`fn` has no value parameters but the expected function takes `{}`",
                            display_type(param_ty.as_type())
                        ),
                    ));
                }
                for (param, ty) in value_params.iter().zip(&collected) {
                    let slot = plan
                        .value_slots
                        .get(source_value_index)
                        .expect("the source plan has one slot per value parameter");
                    debug_assert!(std::ptr::eq(slot.param, *param));
                    if let Some(annotation) = &slot.annotation {
                        check_planned_fn_shape_annotation(
                            annotation,
                            ty,
                            param.meta.span,
                            &binder_names,
                            holes.as_mut(),
                            tcx,
                        )?;
                    }
                    source_value_index += 1;
                }
                value_param_types.extend(collected);
                expected_layer = super::InternedType::fresh_with_identity(*ret_ty, canonical);
            }
        }
    }

    expected_layer = normalize_fn_shape_expected(expected_layer, holes.as_ref());
    if let Some(planned) = &plan.return_annotation {
        check_planned_fn_shape_annotation(
            planned,
            &expected_layer,
            planned.written.span(),
            &binder_names,
            holes.as_mut(),
            tcx,
        )?;
        expected_layer = normalize_fn_shape_expected(expected_layer, holes.as_ref());
    }
    debug_assert_eq!(source_value_index, plan.value_slots.len());
    Ok((expected_layer, value_param_types))
}

pub(crate) fn check_prime_fn_against<'m>(
    sig: &'m crate::ast::Signature<crate::ast::Prime>,
    fn_ret_ty: Option<&'m crate::ast::Type<crate::ast::Prime>>,
    body: &'m crate::ast::Expr<crate::ast::Prime>,
    expected: &super::InternedType<crate::ast::Prime>,
    fn_site: crate::ast::ExpressionSite,
    tcx: &mut TypeCtx<'m, '_, crate::ast::Prime>,
) -> Result<(), Error> {
    let fn_span = fn_site.span;
    #[cfg(test)]
    super::annotation_plan::record_prime_check_consumer();
    let plan = super::annotation_plan::plan_source_lambda_header(sig, fn_ret_ty, tcx)?;
    let binder_names = tcx.in_scope_type_param_binders();
    let (body_expected, value_param_types) =
        fn_shape_against_source_plan(&plan, expected, fn_span, &binder_names, None, tcx)?;
    plan.require_admitted_in_source_order()?;

    let mark = tcx.save();
    let checked = (|| {
        let mut value_param_types_iter = value_param_types.iter();
        for group in plan.group_refs() {
            match group {
                crate::ast::SignatureGroupRef::Type(params) => {
                    for param in params {
                        let crate::ast::SignatureParam::Type(param) = param else {
                            unreachable!("a type signature group contains only type parameters")
                        };
                        tcx.push_type_param_kinded(&param.name, param.effective_kind(), param.span);
                        <crate::prime::typer::PrimeTyper as Typer<crate::ast::Prime>>::record_binder_decl_at(
                            param.span,
                            super::ResolvedBinderKind::TypeParam,
                            &param.name,
                            tcx,
                        );
                    }
                }
                crate::ast::SignatureGroupRef::Value(params) => {
                    for param in params {
                        let crate::ast::SignatureParam::Value(param) = param else {
                            unreachable!("a value signature group contains only value parameters")
                        };
                        let ty = value_param_types_iter
                            .next()
                            .expect("shape checking returns one type per value parameter");
                        tcx.push_value_interned(param.name.clone(), ty.clone());
                        <crate::prime::typer::PrimeTyper as Typer<crate::ast::Prime>>::record_binder_decl_at(
                            param.meta.span,
                            super::ResolvedBinderKind::Local,
                            &param.name,
                            tcx,
                        );
                    }
                }
            }
        }
        debug_assert!(value_param_types_iter.next().is_none());

        <crate::prime::typer::PrimeTyper as Typer<crate::ast::Prime>>::check_value_against_interned(
            body,
            &body_expected,
            tcx,
        )?;
        for (index, ty) in value_param_types.iter().cloned().enumerate() {
            <crate::prime::typer::PrimeTyper as Typer<crate::ast::Prime>>::record_fn_param_type(
                fn_site, index, ty, tcx,
            );
        }
        Ok(())
    })();
    tcx.restore(mark);
    checked
}

// =========================================================================
// Value-boundary and anonymous-function synthesis
// =========================================================================

/// Consume a synthesized expression at a value-type boundary.
///
/// A named callable carries [`Synth::is_scheme`] so genuinely monomorphic
/// consumers can require explicit application. A complete structural function
/// scheme is nevertheless a value type, including as a lambda result or an
/// `equiv` arm. Incomplete schemes retain the ordinary monomorphic-position
/// rejection. Consume the producer's classification rather than rescanning the
/// type spine at each value boundary.
pub(crate) fn synth_value_type<P>(synth: Synth<P>) -> Result<super::InternedType<P>, Error>
where
    P: crate::ast::Phase + Clone,
{
    #[cfg(test)]
    super::kind_scheme::record_synthesized_result_consumer();
    if !synth.is_scheme || synth.complete_scheme.is_complete() {
        return Ok(synth.ty);
    }
    Err(super::types::scheme_in_mono_position(synth.ty.span()))
}

/// Complete a closed lambda from the same pure source-header plan used by the
/// Lowered annotation pipeline. The caller's phase admits no source
/// `Type::Infer` payload here, so every present annotation is closed and
/// missing parameter slots are the only values supplied by immediate-call
/// inference.
pub(crate) fn synth_closed_fn_with_source_plan<'m, P>(
    full: &'m crate::ast::Expr<P>,
    plan: super::annotation_plan::PlannedSourceLambdaHeader<'m, P>,
    body: &'m crate::ast::Expr<P>,
    inferred_param_types: &[Option<super::InternedType<P>>],
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<Synth<P>, Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    assert_eq!(
        inferred_param_types.len(),
        plan.value_slots.len(),
        "Prime immediate-call inference has one result per planned value slot"
    );
    plan.require_admitted_in_source_order()?;
    let mark = tcx.save();
    let synthesized: Result<_, Error> = (|| {
        let mut value_param_types = Vec::with_capacity(plan.value_slots.len());
        let mut value_slots = plan.value_slots.iter().enumerate();
        for group in plan.group_refs() {
            match group {
                crate::ast::SignatureGroupRef::Type(params) => {
                    for param in params {
                        let crate::ast::SignatureParam::Type(param) = param else {
                            unreachable!("a type signature group contains only type parameters")
                        };
                        tcx.push_type_param_kinded(&param.name, param.effective_kind(), param.span);
                        <P::Typer as Typer<P>>::record_binder_decl_at(
                            param.span,
                            super::ResolvedBinderKind::TypeParam,
                            &param.name,
                            tcx,
                        );
                    }
                }
                crate::ast::SignatureGroupRef::Value(params) => {
                    for param in params {
                        let crate::ast::SignatureParam::Value(param) = param else {
                            unreachable!("a value signature group contains only value parameters")
                        };
                        let (index, slot) = value_slots
                            .next()
                            .expect("the source plan has one slot per value parameter");
                        debug_assert!(std::ptr::eq(slot.param, param));
                        let ty = match &slot.annotation {
                            None => inferred_param_types[index].clone().ok_or_else(|| {
                                Error::type_(
                                    param.meta.span,
                                    format!(
                                        "cannot synthesize the type of `fn` parameter `{}`: add a concrete annotation or use the lambda in a checking position",
                                        param.name
                                    ),
                                )
                            })?,
                            Some(annotation) => {
                                assert!(
                                    annotation.plan.holes().is_empty(),
                                    "Kio' admitted a source annotation placeholder"
                                );
                                assert!(
                                    inferred_param_types[index].is_none(),
                                    "an immediate-call parameter hole overrode a written annotation"
                                );
                                super::InternedType::fresh_with_identity(
                                    annotation.plan.ty().clone(),
                                    annotation.plan.identity_is_canonical(),
                                )
                            }
                        };
                        tcx.push_value_interned(param.name.clone(), ty.clone());
                        <P::Typer as Typer<P>>::record_binder_decl_at(
                            param.meta.span,
                            super::ResolvedBinderKind::Local,
                            &param.name,
                            tcx,
                        );
                        value_param_types.push(ty);
                    }
                }
            }
        }
        debug_assert!(value_slots.next().is_none());

        let body_ty = match &plan.return_annotation {
            Some(annotation) => {
                assert!(
                    annotation.plan.holes().is_empty(),
                    "Kio' admitted a source annotation placeholder"
                );
                <P::Typer as Typer<P>>::check_value_against(body, annotation.plan.ty(), tcx)?;
                super::InternedType::fresh_with_identity(
                    annotation.plan.ty().clone(),
                    annotation.plan.identity_is_canonical(),
                )
            }
            None => synth_value_type(<P::Typer as Typer<P>>::synth_expr(body, tcx)?)?,
        };
        Ok((body_ty, value_param_types))
    })();
    tcx.restore(mark);
    let (body_ty, value_param_types) = synthesized?;
    Ok(finish_synth_fn_from_body_type(
        full,
        plan.signature,
        body_ty,
        value_param_types,
        tcx,
    ))
}

pub(crate) fn finish_synth_fn_from_body_type<'m, P>(
    full: &'m crate::ast::Expr<P>,
    sig: &'m crate::ast::Signature<P>,
    body_ty: super::InternedType<P>,
    value_param_types: Vec<super::InternedType<P>>,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Synth<P>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    for (index, ty) in value_param_types.iter().enumerate() {
        <P::Typer as Typer<P>>::record_fn_param_type(full.site(), index, ty.clone(), tcx);
    }
    Synth::canonical_value(build_fn_type_from_body_type(
        sig,
        body_ty,
        value_param_types,
        full.span(),
        tcx,
    ))
}

/// Build the exact anonymous-function type after its body has completed.
/// Metadata publication is deliberately left to the caller so speculative
/// Lowered frontiers can stage it atomically.
pub(crate) fn build_fn_type_from_body_type<P>(
    sig: &crate::ast::Signature<P>,
    body_ty: super::InternedType<P>,
    value_param_types: Vec<super::InternedType<P>>,
    fn_span: Span,
    tcx: &TypeCtx<'_, '_, P>,
) -> crate::ast::Type<P>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    // Rebuild the `FnDef`-shaped param list with each value-param's
    // concrete type attached, then feed it through the same
    // right-fold builder used for top-level declarations. This
    // preserves source-order semantics for mid-position binders —
    // `.(P0)[A](P1)` produces `Function(P0, Forall([A],
    // Function(P1, body_ty)))`, not `Forall([A], Function(Product(P0,
    // P1), body_ty))`.
    let mut binder_names = tcx.in_scope_type_param_binders();
    let mut value_iter = value_param_types.into_iter();
    let resolved_params: Vec<crate::ast::SignatureParam<P>> = sig
        .params
        .iter()
        .map(|p| match p {
            crate::ast::SignatureParam::Type(tp) => {
                binder_names.insert(tp.name.clone());
                crate::ast::SignatureParam::Type(tp.clone())
            }
            crate::ast::SignatureParam::Value(vp) => {
                let resolved_ty = value_iter
                    .next()
                    .expect("synth_fn: value-param types and value-param positions stay parallel");
                let alias_ctx = tcx.binder_alias_ctx(&binder_names);
                crate::ast::SignatureParam::Value(crate::ast::Param {
                    name: vp.name.clone(),
                    ty: Some(
                        super::aliases::canonicalize_for_comparison(
                            resolved_ty.as_type(),
                            &alias_ctx,
                            resolved_ty.identity_is_canonical(),
                        )
                        .0,
                    ),
                    pattern: Default::default(),
                    meta: crate::ast::Meta::new(vp.meta.span),
                })
            }
        })
        .collect();
    debug_assert!(value_iter.next().is_none());
    let resolved_sig = crate::ast::Signature::from_parts(resolved_params, sig.groups.clone());
    let alias_ctx = tcx.binder_alias_ctx(&binder_names);
    let body_ty = super::aliases::canonicalize_for_comparison(
        body_ty.as_type(),
        &alias_ctx,
        body_ty.identity_is_canonical(),
    )
    .0;
    crate::ast::Type::synth_scheme_from_signature(&resolved_sig, body_ty, fn_span)
}

// =========================================================================
// Value-arg synthesis (monomorphic-position helper)
// =========================================================================

/// Synthesize a subexpression after its caller has classified this occurrence
/// as a monomorphic slot. Errors with the canonical "polymorphic in
/// monomorphic position" message if the expression instead produces a scheme.
/// This helper does not classify every `let` RHS or call argument as
/// monomorphic; complete schemes remain structural values where the declared
/// slot admits them.
///
/// Phase-polymorphic via the [`Typer<P>`] dispatch trait.
pub fn synth_value_arg<'m, P>(
    e: &'m crate::ast::Expr<P>,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<crate::ast::Type<P>, Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    <P::Typer as Typer<P>>::synth_expr(e, tcx)?.into_mono()
}

pub(crate) fn synth_value_arg_interned<'m, P>(
    e: &'m crate::ast::Expr<P>,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<super::InternedType<P>, Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    <P::Typer as Typer<P>>::synth_expr(e, tcx)?.into_mono_interned()
}

// =========================================================================
// Let-binding synthesis
// =========================================================================

pub(crate) fn let_annotation_is_elided<P>(annotation: Option<&crate::ast::Type<P>>) -> bool
where
    P: crate::ast::Phase,
{
    annotation.is_none_or(|ty| matches!(ty, crate::ast::Type::Infer { .. }))
}

/// Synthesize the type of an `e;` expression statement followed by
/// a block continuation `body`. Per `specs/language.md` § Blocks
/// and local bindings, `e` MUST have type `.` — discarding a
/// non-unit value is a compile-time error. The result type is
/// `body`'s synthesized type.
///
/// The "use `let _ = e;` to discard a non-unit value deliberately"
/// hint surfaces in the diagnostic so the user sees the spec-canon
/// recovery without reading the spec.
pub fn synth_seq<'m, P>(
    value: &'m crate::ast::Expr<P>,
    body: &'m crate::ast::Expr<P>,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<Synth<P>, Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let value_concrete = <P::Typer as Typer<P>>::synth_expr(value, tcx)?.into_mono()?;
    if !matches!(value_concrete, crate::ast::Type::Unit { .. }) {
        return Err(Error::type_(
            value.span(),
            format!(
                "discarded expression has type `{}`; expression statements must have type `.`. \
                 Bind the result with `let _ = e;` to discard it deliberately, or `let x = e;` \
                 to use it",
                display_type(&value_concrete)
            ),
        ));
    }
    <P::Typer as Typer<P>>::synth_expr(body, tcx)
}

// =========================================================================
// Path resolution (single-segment + qualified two-segment)
// =========================================================================

/// Synthesize the type of a `Path` expression: resolve the segments
/// against locals and the per-module env, return either a `Mono`
/// (for a value binding) or a `Scheme` (for a
/// polymorphic top-level fn / host fn / newtype member).
///
/// Single-segment paths look up locals first, then top-level
/// items, host fn_defs, and intrinsics. Two-segment paths resolve to
/// a newtype member or a qualified-import alias's fn.
///
/// Phase-polymorphic — uses only helpers already in
/// [`typecheck_core`]. The four elaboration-bearing variants don't
/// appear in this helper, so it's reusable at any `P: TyperPhase`.
pub fn synth_path<'m, P>(
    segments: &'m [crate::ast::PathSegment],
    span: Span,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<Synth<P>, Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    synth_path_with_binder_recorder(
        segments,
        span,
        tcx,
        |segment_span, kind, name, parent, qualifier, _local_index, tcx| {
            <P::Typer as Typer<P>>::record_binder_at(
                segment_span,
                kind,
                name,
                parent,
                qualifier,
                tcx,
            );
        },
    )
}

pub(crate) fn synth_path_with_binder_recorder<'m, 'e, P>(
    segments: &'m [crate::ast::PathSegment],
    span: Span,
    tcx: &mut TypeCtx<'m, 'e, P>,
    mut record_binder_at: impl FnMut(
        Span,
        crate::pass::typecheck_core::ResolvedBinderKind,
        &str,
        Option<&str>,
        Option<&str>,
        Option<usize>,
        &mut TypeCtx<'m, 'e, P>,
    ),
) -> Result<Synth<P>, Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    use crate::pass::typecheck_core::ResolvedBinderKind;
    if segments.len() == 1 {
        let head = segments[0].as_str();
        let head_span = segments[0].span;
        if let Some((index, ty, _decl_span)) = tcx.lookup_value_binding_with_index(head) {
            // Local binding (let-local or fn value param).
            // Type-param references go through this
            // path too when a `[A]`-typed binding masks `A` as a
            // value — but the more common case is a name shadowing
            // a type param, so we record `Local`; type-params that
            // reach this synth path (rare) are recorded the same
            // way for hover purposes. `is_type_param` is what
            // distinguishes them at the call-site classifier; this
            // index is informational.
            record_binder_at(
                head_span,
                ResolvedBinderKind::Local,
                head,
                None,
                None,
                Some(index),
                tcx,
            );
            return Ok(Synth::value_interned(ty));
        }
        if let Some(d) = tcx.env.fn_defs.get(head) {
            tcx.require_pure_fn(span, "fn", head, &d.purity)?;
            record_binder_at(
                head_span,
                ResolvedBinderKind::Fn,
                head,
                None,
                None,
                None,
                tcx,
            );
            return Ok(
                match tcx
                    .env
                    .package
                    .and_then(|package| package.module(&tcx.env.module_path))
                {
                    Some(entry) => {
                        synth_top_fn_def_in_module(d, &entry.module, tcx.env.package, span)
                    }
                    None => synth_top_fn_def(d, span),
                },
            );
        }
        if let Some(d) = tcx.env.cross_module_fn_defs.get(head) {
            tcx.require_pure_fn(span, "fn", head, &d.purity)?;
            // A fn imported from another module in the same package
            // looks just like a same-module call: the typer pulls the
            // scheme over and substitutes against the call-site args.
            // Its signature types qualify in the *declaring* module so a
            // bare nominal head keeps that module's identity.
            record_binder_at(
                head_span,
                ResolvedBinderKind::CrossModuleFn,
                head,
                None,
                None,
                None,
                tcx,
            );
            return Ok(match tcx.env.cross_module_fn_def_modules.get(head) {
                Some(declaring) => synth_top_fn_def_in_module(d, declaring, tcx.env.package, span),
                None => synth_top_fn_def(d, span),
            });
        }
        if let Some(h) = tcx.env.env_fn_defs.get(head) {
            tcx.reject_pure_external(span, "host fn", head)?;
            record_binder_at(
                head_span,
                ResolvedBinderKind::HostEnvFn,
                head,
                None,
                None,
                None,
                tcx,
            );
            // A selectively-imported host fn qualifies its signature in
            // the *declaring* module; a same-module / ambient
            // host fn already resolves there, so `synth_env_fn_def` (which
            // leaves the raw signature for the same-module canonicalizer)
            // is correct for it.
            return Ok(match tcx.env.cross_module_env_fn_modules.get(head) {
                Some(declaring) => synth_env_fn_def_in_module(h, declaring, tcx.env.package, span),
                None => match tcx
                    .env
                    .package
                    .and_then(|package| package.module(&tcx.env.module_path))
                {
                    Some(entry) => {
                        synth_env_fn_def_in_module(h, &entry.module, tcx.env.package, span)
                    }
                    None => synth_env_fn_def(h, span),
                },
            });
        }
        if tcx.env.comptime_in_scope
            && let Some(builtin) = ComptimeBuiltin::from_public_name(head)
            && builtin.is_value_name()
            && let Some(scheme) = comptime_scheme(builtin, span, &tcx.env.roles_in_scope)?
        {
            return Ok(scheme);
        }
        if tcx.env.intrinsics_in_scope {
            let bool_role = tcx.resolve_exact_role(crate::ast::Role::Bool);
            let ambiguity = match bool_role {
                super::RoleResolution::Ambiguous { first, second } => {
                    Some(tcx.describe_exact_role_ambiguity(crate::ast::Role::Bool, first, second))
                }
                super::RoleResolution::Missing | super::RoleResolution::Unique(_) => None,
            };
            if let Some(scheme) =
                intrinsic_scheme_resolved(head, span, bool_role, ambiguity.as_deref())
            {
                record_binder_at(
                    head_span,
                    ResolvedBinderKind::Intrinsic,
                    head,
                    None,
                    None,
                    None,
                    tcx,
                );
                return scheme;
            }
        }
        if let Some(builtin) = ComptimeBuiltin::from_public_name(head)
            && builtin.is_value_name()
        {
            return Err(Error::type_(
                span,
                format!(
                    "compile-time helper `{head}` is not in scope (use `import __comptime__;`)"
                ),
            ));
        }
        if head.starts_with("__") && head.ends_with("__") {
            return Err(Error::type_(
                span,
                format!("intrinsic `{head}` is not in scope (use `import __intrinsics__;`)"),
            ));
        }
        if let Some(error) = type_name_in_value_position(segments, span, tcx) {
            return Err(error);
        }
        // No scope contains `head`. The resolver catches this for
        // regular module bodies (with a tighter "unbound name" error),
        // but bridge adapter bodies don't go through the resolver, so
        // we surface the name-resolution failure here instead — with
        // the same `did you mean …?` near-miss suggestion, drawn from
        // the fns and imports the adapter body can call.
        let mut candidates: Vec<&str> = tcx
            .env
            .fn_defs
            .keys()
            .chain(tcx.env.cross_module_fn_defs.keys())
            .chain(tcx.env.env_fn_defs.keys())
            .copied()
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        let err =
            Error::name_res(span, format!("unbound name `{head}`")).with_unresolved_name(head);
        return Err(
            match crate::error::closest_name(head, candidates.iter().copied()) {
                Some(near) => err
                    .with_help(format!(
                        "a name `{near}` is in scope — did you mean `{near}`?"
                    ))
                    .with_suggestion(span, near.to_owned()),
                None => err,
            },
        );
    }
    if segments.len() == 2 {
        let head = segments[0].as_str();
        let member = segments[1].as_str();
        let head_span = segments[0].span;
        let member_span = segments[1].span;
        if let Some(resolved) = tcx.env.resolve_member_newtype(&segments[..1]) {
            require_member_head_visibility(&resolved, head, head_span, tcx)?;
            require_member_visibility(&resolved, head, member, member_span, tcx)?;
            record_member_resolution(&resolved, member, head_span, member_span, tcx);
            let nominal_segments = tcx.env.qualify_nominal_segments(&segments[..1]);
            return newtype_member_scheme_in_module(
                resolved.newtype,
                member,
                &nominal_segments,
                resolved.declaring_module,
                tcx.env.package,
                span,
            );
        }
        // Qualified-import access: `import pkg/helper as h; ... h.foo`
        // resolves `h` to its target module; `foo` must be a `pub
        // fn` there. The resolver has already validated the alias.
        if let Some(target) = tcx.env.qualified_imports.get(head) {
            for item in &target.items {
                if let crate::ast::Item::FnDef(d) = item
                    && d.name == member
                    && crate::pass::resolve::is_visible(&d.vis, &tcx.env.module.path)
                {
                    tcx.require_pure_fn(span, "fn", member, &d.purity)?;
                    record_binder_at(
                        head_span,
                        ResolvedBinderKind::QualifiedImport,
                        head,
                        None,
                        None,
                        None,
                        tcx,
                    );
                    record_binder_at(
                        member_span,
                        ResolvedBinderKind::QualifiedImportMember,
                        member,
                        Some(head),
                        None,
                        None,
                        tcx,
                    );
                    // The aliased module is the fn's declaring module, so
                    // qualify its signature there.
                    return Ok(synth_top_fn_def_in_module(d, target, tcx.env.package, span));
                }
            }
            // A qualified alias also exposes the target module's `host
            // fn`s, e.g. `import testapi/io as io; io.print(...)`. Host
            // environment fns are impure, so a pure context rejects them
            // exactly as it does for an ambient host-env fn reference.
            if let Some(h) = host_env_fn_in_module(target, member) {
                tcx.reject_pure_external(span, "host fn", member)?;
                record_binder_at(
                    head_span,
                    ResolvedBinderKind::QualifiedImport,
                    head,
                    None,
                    None,
                    None,
                    tcx,
                );
                record_binder_at(
                    member_span,
                    ResolvedBinderKind::HostEnvFn,
                    member,
                    Some(head),
                    None,
                    None,
                    tcx,
                );
                // `target` is the host fn's declaring module.
                return Ok(synth_env_fn_def_in_module(h, target, tcx.env.package, span));
            }
            if let Some(error) = type_name_in_value_position(segments, span, tcx) {
                return Err(error);
            }
            return Err(Error::type_(
                span,
                format!("module aliased as `{head}` has no `pub fn` named `{member}`"),
            ));
        }
    }
    // Three-segment form: `<alias>.<TypeName>.<member>` for a
    // newtype member reached through a qualified-import alias.
    // Sub-module access through an alias (`<alias>.<sub>.<fn>`)
    // is *not* supported by design — a qualified import binds one
    // module's surface, not a tree of sub-modules. The middle
    // segment must therefore name a `pub newtype` in the aliased
    // module; anything else is rejected.
    if segments.len() == 3 && tcx.env.qualified_imports.contains_key(segments[0].as_str()) {
        let alias = segments[0].as_str();
        let type_name = segments[1].as_str();
        let member = segments[2].as_str();
        let alias_span = segments[0].span;
        let type_name_span = segments[1].span;
        let member_span = segments[2].span;
        if let Some(resolved) = tcx.env.resolve_member_newtype(&segments[..2]) {
            let display_name = format!("{alias}.{type_name}");
            require_member_head_visibility(&resolved, &display_name, type_name_span, tcx)?;
            require_member_visibility(&resolved, &display_name, member, member_span, tcx)?;
            record_binder_at(
                alias_span,
                ResolvedBinderKind::QualifiedImport,
                alias,
                None,
                None,
                None,
                tcx,
            );
            record_member_resolution(&resolved, member, type_name_span, member_span, tcx);
            let nominal_segments = tcx.env.qualify_nominal_segments(&segments[..2]);
            return newtype_member_scheme_in_module(
                resolved.newtype,
                member,
                &nominal_segments,
                resolved.declaring_module,
                tcx.env.package,
                span,
            );
        }
        return Err(Error::type_(
            span,
            format!(
                "module aliased as `{alias}` has no `pub newtype` named `{type_name}` \
                 (sub-module access through a qualified-import alias is not supported)"
            ),
        ));
    }
    Err(Error::type_(
        span,
        format!(
            "dotted-path `{}` is not a recognised value reference \
             (sub-module access through a qualified-import alias is not supported; \
             import the module declaring `{}` directly)",
            segments.join("."),
            segments.join("."),
        ),
    ))
}

fn type_name_in_value_position<P>(
    segments: &[crate::ast::PathSegment],
    span: Span,
    tcx: &TypeCtx<'_, '_, P>,
) -> Option<Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    use crate::pass::resolve::{NominalProvider, NominalSelection};

    let display = segments.join(".");
    let make_error = || {
        let message = format!("`{display}` is a type, not a value");
        if segments.len() == 1 {
            Error::name_res(span, message)
        } else {
            Error::type_(span, message)
        }
    };
    if let [head] = segments
        && tcx.is_type_param(head.as_str())
    {
        return None;
    }

    let provider = NominalProvider::new(Some(tcx.env.module), tcx.env.package);
    let NominalSelection::Selected(selected) = provider.select(provider.root(), segments, false)
    else {
        return None;
    };
    let owner = selected.owner.module()?;
    let same_owner = std::ptr::eq(owner, tcx.env.module);
    if same_owner {
        // A module catalogue includes later declarations; only the active
        // source-order boundary proves that this binding is available here.
        let cutoff = tcx.local_item_cutoff()?;
        if selected.id.item_index() >= cutoff {
            return None;
        }
    } else if !crate::pass::resolve::is_visible(
        &selected.declaration.visibility(),
        &tcx.env.module.path,
    ) {
        return None;
    }
    let (kind, declaration_span) = if let Some(alias) = selected.declaration.type_alias() {
        ("type alias", alias.name_span)
    } else if let Some(newtype) = selected.declaration.newtype() {
        ("newtype", newtype.name_span)
    } else if let Some(host) = selected.declaration.host_type() {
        ("host type", host.meta.span)
    } else {
        return None;
    };
    let label = format!("{kind} declared here");
    let error = make_error();
    Some(if same_owner {
        error.with_secondary(declaration_span, label)
    } else {
        error.with_secondary_in_file(selected.owner.provider_file()?, declaration_span, label)
    })
}

fn newtype_member_visibility<'a, P: crate::ast::Phase>(
    newtype: &'a crate::ast::Newtype<P>,
    member: &str,
) -> Option<&'a crate::ast::Visibility> {
    if member == newtype.constructor.name {
        Some(&newtype.constructor.vis)
    } else if member == newtype.projector.name {
        Some(&newtype.projector.vis)
    } else {
        None
    }
}

fn module_path_string<P: crate::ast::Phase>(module: &crate::ast::Module<P>) -> String {
    module
        .path
        .segments
        .iter()
        .map(crate::ast::PathSegment::as_str)
        .collect::<Vec<_>>()
        .join("/")
}

fn require_member_head_visibility<P>(
    resolved: &super::env::MemberNewtypeResolution<'_, P>,
    display_name: &str,
    span: Span,
    tcx: &TypeCtx<'_, '_, P>,
) -> Result<(), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let owner = resolved.source_module;
    if owner.path == tcx.env.module.path {
        return Ok(());
    }
    let visibility = resolved.source.visibility();
    if crate::pass::resolve::is_visible(&visibility, &tcx.env.module.path) {
        return Ok(());
    }
    let declaration_kind = if resolved.source.type_alias().is_some() {
        "type alias"
    } else {
        "newtype"
    };
    let mut error = Error::type_(
        span,
        format!(
            "{declaration_kind} `{display_name}` is not visible from module `{}`",
            tcx.env.module_path
        ),
    );
    if let crate::ast::Visibility::PublicIn(path) = visibility {
        error = error.with_help(format!(
            "`{display_name}` is restricted to `pub({})`; use it only from that module subtree",
            path.segments.join("/")
        ));
    }
    Err(error)
}

fn require_member_visibility<P>(
    resolved: &super::env::MemberNewtypeResolution<'_, P>,
    display_name: &str,
    member: &str,
    span: Span,
    tcx: &TypeCtx<'_, '_, P>,
) -> Result<(), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let Some(visibility) = newtype_member_visibility(resolved.newtype, member) else {
        return Err(cross_module_newtype_member_not_found(
            resolved.newtype,
            display_name,
            member,
            &tcx.env.module.path,
            span,
        ));
    };
    if resolved.declaring_module.path == tcx.env.module.path
        || crate::pass::resolve::is_visible(visibility, &tcx.env.module.path)
    {
        return Ok(());
    }
    Err(cross_module_newtype_member_not_visible(
        display_name,
        member,
        visibility,
        &tcx.env.module_path,
        span,
    ))
}

fn record_member_resolution<P>(
    resolved: &super::env::MemberNewtypeResolution<'_, P>,
    member: &str,
    head_span: Span,
    member_span: Span,
    tcx: &mut TypeCtx<'_, '_, P>,
) where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let source_owner = resolved.source_module;
    let source_module = module_path_string(source_owner);
    let (kind, source_name) = if let Some(alias) = resolved.source.type_alias() {
        (super::NominalTypeBinderKind::TypeAlias, alias.name.as_str())
    } else {
        (
            super::NominalTypeBinderKind::Newtype,
            resolved
                .source
                .newtype()
                .expect("a member source is an identity alias or newtype")
                .name
                .as_str(),
        )
    };
    <P::Typer as Typer<P>>::record_nominal_type_binder_at(
        head_span,
        kind,
        &source_module,
        source_name,
        tcx,
    );
    let terminal_module = module_path_string(resolved.declaring_module);
    <P::Typer as Typer<P>>::record_newtype_member_binder_at(
        member_span,
        &terminal_module,
        &resolved.newtype.name,
        member,
        tcx,
    );
}

fn cross_module_newtype_member_not_visible(
    display_newtype: &str,
    member: &str,
    visibility: &crate::ast::Visibility,
    importer_display: &str,
    span: Span,
) -> Error {
    let help = match visibility {
        crate::ast::Visibility::Private => None,
        crate::ast::Visibility::PublicIn(path) => {
            let scope = path
                .segments
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>()
                .join("/");
            Some(format!(
                "`{member}` is restricted to `pub({scope})`; use it only from that module subtree"
            ))
        }
        crate::ast::Visibility::Public => {
            unreachable!("a public newtype member is visible from every module")
        }
    };
    let error = Error::type_(
        span,
        format!(
            "newtype member `{display_newtype}.{member}` is not visible from module `{importer_display}`"
        ),
    );
    match help {
        Some(help) => error.with_help(help),
        None => error,
    }
}

fn cross_module_newtype_member_not_found<P: crate::ast::Phase>(
    newtype: &crate::ast::Newtype<P>,
    display_newtype: &str,
    member: &str,
    importer: &crate::ast::ModulePath,
    span: Span,
) -> Error {
    let constructor_visible = crate::pass::resolve::is_visible(&newtype.constructor.vis, importer);
    let projector_visible = crate::pass::resolve::is_visible(&newtype.projector.vis, importer);
    let message = match (constructor_visible, projector_visible) {
        (true, true) => format!(
            "newtype `{display_newtype}` has no member `{member}` (expected `{}` or `{}`)",
            newtype.constructor.name, newtype.projector.name
        ),
        (true, false) => format!(
            "newtype `{display_newtype}` has no member `{member}` (expected `{}`)",
            newtype.constructor.name
        ),
        (false, true) => format!(
            "newtype `{display_newtype}` has no member `{member}` (expected `{}`)",
            newtype.projector.name
        ),
        (false, false) => {
            format!("newtype `{display_newtype}` has no accessible member `{member}`")
        }
    };
    Error::type_(span, message)
}

// =========================================================================
// Literal typing
// =========================================================================

/// Tier-3 literal resolution (`specs/language.md` § Literals): the
/// unique role-bearing host type — or, when no host type carries the
/// role, the unique role-inheriting `type` alias (the rehosted-module
/// case) — whose role the literal's `shape` admits. Returns `None`
/// when the candidate is not unique (zero candidates, or an ambiguous
/// pool). Shared by [`synth_literal`]'s synthesis arm and the
/// `substitute` pass's bare-literal annotation fill-in
/// (`walk_literal_annotation`), so both derive the same host type for a
/// bare Lowered literal; [`literal_resolution_error`] renders the
/// diagnostic for the non-unique case.
pub fn unique_role_admitted_type<P: crate::ast::Phase>(
    shape: crate::ast::RoleShape,
    env: &ModuleEnv<'_, P>,
    span: Span,
) -> Option<crate::ast::Type<P>> {
    unique_role_admitted_type_at(shape, env, None, span)
}

pub(crate) fn unique_role_admitted_type_at<P: crate::ast::Phase>(
    shape: crate::ast::RoleShape,
    env: &ModuleEnv<'_, P>,
    local_item_cutoff: Option<usize>,
    span: Span,
) -> Option<crate::ast::Type<P>> {
    match env.resolve_direct_role_candidates(|role| shape.admits(role.shape()), local_item_cutoff) {
        super::RoleResolution::Unique(name) => {
            return Some(crate::ast::Type::synth_path(
                vec![name.to_owned()],
                Vec::new(),
                span,
            ));
        }
        // Two or more host types carry the role — ambiguous; an alias
        // never enlarges a host type's candidate set, so stop here.
        super::RoleResolution::Ambiguous { .. } => return None,
        super::RoleResolution::Missing => {}
    }
    match env.resolve_alias_role_candidates(|role| shape.admits(role.shape()), local_item_cutoff) {
        super::RoleResolution::Unique(name) => Some(crate::ast::Type::synth_path(
            vec![name.to_owned()],
            Vec::new(),
            span,
        )),
        super::RoleResolution::Missing | super::RoleResolution::Ambiguous { .. } => None,
    }
}

pub(crate) fn unique_role_admitted_interned_at<P>(
    shape: crate::ast::RoleShape,
    span: Span,
    tcx: &TypeCtx<'_, '_, P>,
) -> Option<super::InternedType<P>>
where
    P: TyperPhase + Clone,
{
    let resolved = unique_role_admitted_type_at(shape, tcx.env, tcx.local_item_cutoff(), span)?;
    let resolved = match resolved {
        crate::ast::Type::Path {
            segments,
            args,
            meta,
        } => crate::ast::Type::Path {
            segments: tcx.env.qualify_nominal_segments(&segments),
            args,
            meta,
        },
        other => other,
    };
    Some(super::InternedType::fresh_with_identity(resolved, true))
}

/// The tier-3 diagnostic for a literal whose host type
/// [`unique_role_admitted_type`] could not uniquely resolve — a
/// no-candidate "has no type" error or an ambiguous-pool error naming
/// two candidates. Kept beside the resolver so the two stay in step.
fn literal_resolution_error<P: crate::ast::Phase>(
    shape: crate::ast::RoleShape,
    env: &ModuleEnv<'_, P>,
    local_item_cutoff: Option<usize>,
    span: Span,
) -> Error {
    if let super::RoleResolution::Ambiguous {
        first: a,
        second: b,
    } = env.resolve_direct_role_candidates(|role| shape.admits(role.shape()), local_item_cutoff)
    {
        let candidates = env
            .describe_direct_role_ambiguity(|role| shape.admits(role.shape()), local_item_cutoff)
            .unwrap_or_else(|| format!("`{a}`, `{b}`, …"));
        return Error::type_(
            span,
            format!(
                "this {} literal is ambiguous: the current host declares more \
                 than one role-bearing host type its shape admits ({candidates}). \
                 Annotate the literal with the intended host type — `<literal>(T)`",
                literal_shape_noun(shape)
            ),
        );
    }
    if let super::RoleResolution::Ambiguous {
        first: a,
        second: b,
    } = env.resolve_alias_role_candidates(|role| shape.admits(role.shape()), local_item_cutoff)
    {
        return Error::type_(
            span,
            format!(
                "this {} literal is ambiguous: more than one type its shape \
                 admits is in scope (`{a}`, `{b}`, …). Annotate the literal with \
                 the intended type — `<literal>(T)`",
                literal_shape_noun(shape)
            ),
        );
    }
    Error::type_(
        span,
        format!(
            "this {} literal has no type: the current host declares no \
             role-bearing host type its shape admits. Annotate the literal \
             with a host type — `<literal>(T)` — or declare a `role(...)` \
             host type",
            literal_shape_noun(shape)
        ),
    )
}

/// Type a literal expression against the current role-bearing
/// host types, per the three-tier resolution of `specs/language.md`
/// § Literals. This is the **synthesis-mode** entry — it covers
/// tier 1 (an explicit `(Type)` annotation on the literal) and
/// tier 3 (the unique role-admitted host type); tier 2
/// (an expected type from the surrounding position) is reached only
/// in check mode and handled by [`resolve_literal_against`].
///
/// `shape` is the literal's lexical shape; `annotation` is its
/// optional trailing `(Type)` call form (a `_` placeholder
/// annotation is equivalent to a bare literal and falls through to
/// tier 3). On success the phase publication hook receives the resolved host
/// type. Lowered records it for substitution; Prime synthesis uses its
/// mandatory annotation directly and bypasses this helper.
pub fn synth_literal<'m, P>(
    shape: crate::ast::RoleShape,
    annotation: Option<&crate::ast::Type<P>>,
    site: crate::ast::ExpressionSite,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<super::InternedType<P>, Error>
where
    P: TyperPhase + Clone,
{
    let span = site.span;
    let resolved = synth_literal_unrecorded(shape, annotation, span, tcx)?;
    <P::Typer as Typer<P>>::record_literal_resolution(site, resolved.clone(), tcx);
    Ok(resolved)
}

/// Resolve a literal in synthesis mode without publishing its selected host
/// type.  Lowered's retained application frontier uses this form so the
/// resolution becomes visible only with the enclosing publication close.
pub(crate) fn synth_literal_unrecorded<'m, P>(
    shape: crate::ast::RoleShape,
    annotation: Option<&crate::ast::Type<P>>,
    span: Span,
    tcx: &TypeCtx<'m, '_, P>,
) -> Result<super::InternedType<P>, Error>
where
    P: TyperPhase + Clone,
{
    // Tier 1 — explicit `(Type)` annotation on the literal.
    if let Some(annot) = annotation
        && !matches!(annot, crate::ast::Type::Infer { .. })
    {
        let resolved = check_literal_annotation(shape, annot, tcx.env)?;
        return Ok(super::InternedType::fresh(resolved));
    }
    // Tier 3 — the unique host type (or, in the rehosted-module case,
    // the unique role-inheriting `type` alias) whose role the shape
    // admits. Tier 2 (expected type) does not apply in synthesis mode;
    // if tier 3 finds no unique candidate, the literal is a type error
    // (tier 4). Shared with the `substitute` pass's bare-literal
    // fill-in via [`unique_role_admitted_type`].
    let Some(resolved) = unique_role_admitted_interned_at(shape, span, tcx) else {
        return Err(literal_resolution_error(
            shape,
            tcx.env,
            tcx.local_item_cutoff(),
            span,
        ));
    };
    Ok(resolved)
}

/// Resolve a literal in **check mode**: tier 1 (an explicit `(Type)`
/// annotation), tier 2 (the `expected` type itself, when it is a
/// host type whose role the literal's shape admits), then a tier 3 /
/// 4 fallback to [`synth_literal`]. Returns the literal's resolved
/// type and invokes [`Typer::record_literal_resolution`]. Lowered records the
/// result for substitution; Prime's hook is a no-op because Kio' already
/// carries the mandatory annotation. The caller equiv-checks the result against
/// `expected`, which rejects a tier-1 annotation that disagrees with the
/// position.
pub fn resolve_literal_against<'m, P>(
    shape: crate::ast::RoleShape,
    annotation: Option<&crate::ast::Type<P>>,
    expected: &super::InternedType<P>,
    site: crate::ast::ExpressionSite,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<super::InternedType<P>, Error>
where
    P: TyperPhase + Clone,
{
    let span = site.span;
    let resolved = resolve_literal_against_unrecorded(shape, annotation, expected, span, tcx)?;
    <P::Typer as Typer<P>>::record_literal_resolution(site, resolved.clone(), tcx);
    Ok(resolved)
}

/// Resolve a literal in checking mode without publishing the resolution.
/// This preserves the language's tier ordering while letting the ordinary
/// Lowered frontier journal the selected type transactionally.
pub(crate) fn resolve_literal_against_unrecorded<'m, P>(
    shape: crate::ast::RoleShape,
    annotation: Option<&crate::ast::Type<P>>,
    expected: &super::InternedType<P>,
    span: Span,
    tcx: &TypeCtx<'m, '_, P>,
) -> Result<super::InternedType<P>, Error>
where
    P: TyperPhase + Clone,
{
    if shape == crate::ast::RoleShape::Str
        && let crate::ast::Type::Path { segments, args, .. } = expected.as_type()
        && segments.len() == 1
        && segments[0].as_str() == "__Diagnostic_text__"
        && args.is_empty()
    {
        return Ok(expected.clone());
    }
    // Tier 1 — explicit annotation. It must itself be a shape-admitted
    // host type; the equality against `expected` is the caller's job
    // (check mode compares the resolved type to the expected one). A
    // `_` placeholder annotation is equivalent to a bare literal.
    if let Some(annot) = annotation
        && !matches!(annot, crate::ast::Type::Infer { .. })
    {
        let resolved = check_literal_annotation(shape, annot, tcx.env)?;
        return Ok(super::InternedType::fresh(resolved));
    }
    // Tier 2 — the expected type, when it is a role-bearing host type
    // (or a `type` alias that inherits a role from one) whose role the
    // literal's shape admits. The `tcx.env` borrow is confined to this
    // block so the recording call can reborrow `tcx`.
    let tier2_hit = expected_host_type_admits(
        shape,
        expected.as_type(),
        expected.identity_is_canonical(),
        tcx.env,
    );
    if tier2_hit {
        return Ok(expected.clone());
    }
    // Neither tier applies — fall back to unrecorded synthesis (tier 3 / 4).
    // The enclosing caller publishes the selected type exactly once.
    synth_literal_unrecorded(shape, annotation, span, tcx)
}

/// Whether `expected` is a role-bearing host type (or a `type` alias that
/// inherits a role from one) the literal `shape` admits — the literal's
/// tier-2 (expected-type) resolution. Handles both a **bare** host-type
/// name (or alias) and an identity-exact **qualified** host type
/// (`testapi.I64`), which a cross-module host fn's now-qualified signature
/// produces in the expected slot. The qualified form resolves the expected
/// type's exact `(module, name)` identity through the consumer's imports and
/// reads only the declaration at that identity. It never searches a growing
/// candidate set, so unrelated declarations cannot redirect the match.
fn expected_host_type_admits<P>(
    shape: crate::ast::RoleShape,
    expected: &crate::ast::Type<P>,
    expected_is_canonical: bool,
    env: &ModuleEnv<'_, P>,
) -> bool
where
    P: crate::pass::resolve::ResolvePhase,
{
    let crate::ast::Type::Path { segments, args, .. } = expected else {
        return false;
    };
    if !args.is_empty() {
        return false;
    }
    // Bare form: match the single segment directly against the role
    // table, or against a `type` alias that inherits a role from a host
    // type (`type I32 = provider.I32` — the form a `rehost` materializes).
    // An alias is always referenced by a single bare name, so the
    // qualified branch below need not consult `alias_roles`.
    if segments.len() == 1 {
        let name = segments[0].as_str();
        return env
            .host_env_roles
            .iter()
            .any(|(role, n)| *n == name && shape.admits(role.shape()))
            || env
                .alias_roles
                .get(name)
                .is_some_and(|role| shape.admits(role.shape()));
    }
    // Qualified form: resolve the expected type's identity, then read the
    // role from the host type or visible transparent alias declared at exactly
    // that identity. Imports owned by the target module are not declarations
    // in its qualified namespace.
    let expected_identity = if expected_is_canonical {
        canonical_nominal_identity(segments)
    } else {
        env.nominal_ref_identity(segments)
    };
    let Some(expected_identity) = expected_identity else {
        return false;
    };
    env.declared_role_for_nominal_identity(&expected_identity.0, &expected_identity.1)
        .is_some_and(|role| shape.admits(role.shape()))
}

fn canonical_nominal_identity(segments: &[crate::ast::PathSegment]) -> Option<(String, String)> {
    let (name, module_segments) = segments.split_last()?;
    if module_segments.is_empty() {
        return None;
    }
    Some((
        module_segments
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/"),
        name.as_str().to_owned(),
    ))
}

pub fn check_literal_annotation<P>(
    shape: crate::ast::RoleShape,
    annot: &crate::ast::Type<P>,
    env: &ModuleEnv<'_, P>,
) -> Result<crate::ast::Type<P>, Error>
where
    P: crate::pass::resolve::ResolvePhase + Clone,
{
    let crate::ast::Type::Path { segments, args, .. } = annot else {
        return Err(Error::type_(
            annot.span(),
            "a literal's `(Type)` annotation must be a host type name".to_owned(),
        ));
    };
    if !args.is_empty() {
        return Err(Error::type_(
            annot.span(),
            "a literal's `(Type)` annotation must be a host type name, \
                      not a parametric type"
                .to_owned(),
        ));
    }
    // Bare annotations read the lexical role maps. Qualified annotations and
    // identity-exact annotations inserted by tier 2 read only the declaration
    // at the resolved identity, never a provider candidate set.
    let name = segments.last().map(|s| s.as_str()).unwrap_or_default();
    let role = if segments.len() == 1 {
        env.host_env_roles
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(role, _)| *role)
            .or_else(|| env.alias_roles.get(name).copied())
    } else {
        env.nominal_ref_identity(segments)
            .and_then(|identity| env.declared_role_for_nominal_identity(&identity.0, &identity.1))
    };
    let Some(role) = role else {
        return Err(Error::type_(
            annot.span(),
            format!(
                "`{name}` is not a role-bearing host type — a literal annotation \
                 must name a type declared with a `role(...)` annotation"
            ),
        ));
    };
    if !shape.admits(role.shape()) {
        return Err(Error::type_(
            annot.span(),
            format!(
                "a {} literal cannot be typed as `{name}` — `{name}` carries `role({})`, \
                 which this literal's shape does not admit",
                literal_shape_noun(shape),
                role.as_str()
            ),
        ));
    }
    Ok(annot.clone())
}

/// English noun for a literal shape, for diagnostics.
fn literal_shape_noun(shape: crate::ast::RoleShape) -> &'static str {
    match shape {
        crate::ast::RoleShape::Int => "integer",
        crate::ast::RoleShape::Float => "float",
        crate::ast::RoleShape::Str => "string",
        crate::ast::RoleShape::Bool => "boolean",
    }
}

// =========================================================================
// Intrinsic schemes
// =========================================================================

fn refl_ty<P>(name: &str, span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    ty_path(name, span)
}

fn refl_type_ty<P>(span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    refl_ty("__Type__", span)
}

fn comptime_proof_ty<P>(span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    refl_ty("__Comptime__", span)
}

fn fill_ctx_ty<P>(span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    refl_ty("__Fill_ctx__", span)
}

fn refl_checked_term_ty<P>(span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    refl_ty("__Checked_term__", span)
}

fn refl_type_var_ty<P>(span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    refl_ty("__Type_var__", span)
}

fn refl_type_name_ty<P>(span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    refl_ty("__Type_name__", span)
}

fn refl_type_arity_ty<P>(span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    refl_ty("__Type_arity__", span)
}

fn diagnostic_text_ty<P>(span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    refl_ty("__Diagnostic_text__", span)
}

fn refl_predicate_ty<P>(
    span: Span,
    _roles: &HashMap<crate::ast::Role, &str>,
) -> Result<crate::ast::Type<P>, Error>
where
    P: crate::ast::Phase,
{
    Ok(crate::ast::Type::synth_path(
        vec!["Comptime_bool".to_owned()],
        Vec::new(),
        span,
    ))
}

fn refl_sum_many<P>(mut branches: Vec<crate::ast::Type<P>>, span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    let mut acc = branches
        .pop()
        .expect("reflection sum ABI must contain at least one branch");
    while let Some(next) = branches.pop() {
        acc = ty_sum(next, acc, span);
    }
    acc
}

fn refl_pair3<P>(
    a: crate::ast::Type<P>,
    b: crate::ast::Type<P>,
    c: crate::ast::Type<P>,
    span: Span,
) -> crate::ast::Type<P>
where
    P: crate::ast::Phase,
{
    ty_product(a, ty_product(b, c, span), span)
}

fn type_view_ty<P>(span: Span) -> crate::ast::Type<P>
where
    P: crate::ast::Phase + Clone,
{
    let unit = crate::ast::Type::Unit {
        meta: Meta::new(span),
    };
    let type_ty = refl_type_ty(span);
    refl_sum_many(
        vec![
            unit.clone(),
            unit,
            ty_product(type_ty.clone(), type_ty.clone(), span),
            ty_product(type_ty.clone(), type_ty.clone(), span),
            refl_pair3(
                type_ty.clone(),
                refl_type_arity_ty(span),
                type_ty.clone(),
                span,
            ),
            refl_pair3(
                refl_type_var_ty(span),
                refl_type_arity_ty(span),
                type_ty.clone(),
                span,
            ),
            refl_type_var_ty(span),
            refl_type_name_ty(span),
        ],
        span,
    )
}

fn refl_scheme_from_parts<P>(
    mut params: Vec<crate::ast::SignatureParam<P>>,
    ret: crate::ast::Type<P>,
    span: Span,
) -> Synth<P>
where
    P: crate::ast::Phase + Clone,
{
    params.insert(0, vp("ct", comptime_proof_ty(span), span));
    Synth::complete_scheme(crate::ast::Type::synth_scheme_from_signature_params(
        &params, ret, span,
    ))
}

pub fn comptime_scheme<P>(
    builtin: ComptimeBuiltin,
    span: Span,
    roles: &HashMap<crate::ast::Role, &str>,
) -> Result<Option<Synth<P>>, Error>
where
    P: crate::ast::Phase + Clone,
{
    if builtin.is_type_name() {
        return Ok(None);
    }
    let name = builtin.public_name();
    let type_ty = || refl_type_ty(span);
    let checked_term_ty = || refl_checked_term_ty(span);
    let type_var_ty = || refl_type_var_ty(span);
    let type_name_ty = || refl_type_name_ty(span);
    let type_arity_ty = || refl_type_arity_ty(span);
    let diagnostic_ty = || diagnostic_text_ty(span);
    let r_ty = || ty_path::<P>("r", span);
    let fuel_ty = || ty_path::<P>("fuel", span);
    let input_ty = || ty_path::<P>("input", span);
    let result_ty = || ty_path::<P>("result", span);

    let scheme = match name {
        "__reflect_type__" => refl_scheme_from_parts(vec![tp("a", span)], type_ty(), span),
        "__type_view__" => {
            refl_scheme_from_parts(vec![vp("typ", type_ty(), span)], type_view_ty(span), span)
        }
        "__type_is_host__" => refl_scheme_from_parts(
            vec![vp("typ", type_ty(), span)],
            refl_predicate_ty(span, roles)?,
            span,
        ),
        "__type_host_converters__" => refl_scheme_from_parts(
            vec![
                vp("string_type", type_ty(), span),
                vp("source_type", type_ty(), span),
            ],
            type_ty(),
            span,
        ),
        "__type_equal__" => refl_scheme_from_parts(
            vec![vp("left", type_ty(), span), vp("right", type_ty(), span)],
            refl_predicate_ty(span, roles)?,
            span,
        ),
        "__type_name_equal__" => refl_scheme_from_parts(
            vec![
                vp("left", type_name_ty(), span),
                vp("right", type_name_ty(), span),
            ],
            refl_predicate_ty(span, roles)?,
            span,
        ),
        "__type_var_equal__" => refl_scheme_from_parts(
            vec![
                vp("left", type_var_ty(), span),
                vp("right", type_var_ty(), span),
            ],
            refl_predicate_ty(span, roles)?,
            span,
        ),
        "__type_is_product_root__" | "__type_is_sum_root__" => refl_scheme_from_parts(
            vec![vp("typ", type_ty(), span)],
            refl_predicate_ty(span, roles)?,
            span,
        ),
        "__type_unit__" => refl_scheme_from_parts(Vec::new(), type_ty(), span),
        "__type_bottom__" => refl_scheme_from_parts(Vec::new(), type_ty(), span),
        "__type_product__" | "__type_sum__" => refl_scheme_from_parts(
            vec![vp("left", type_ty(), span), vp("right", type_ty(), span)],
            type_ty(),
            span,
        ),
        "__type_arrow__" => refl_scheme_from_parts(
            vec![
                vp("params", type_ty(), span),
                vp("arity", type_arity_ty(), span),
                vp("result", type_ty(), span),
            ],
            type_ty(),
            span,
        ),
        "__type_forall__" => refl_scheme_from_parts(
            vec![
                vp("arity", type_arity_ty(), span),
                vp("body", ty_func(vec![type_var_ty()], type_ty(), span), span),
            ],
            type_ty(),
            span,
        ),
        "__type_var__" => {
            refl_scheme_from_parts(vec![vp("var", type_var_ty(), span)], type_ty(), span)
        }
        "__type_name_type__" => {
            refl_scheme_from_parts(vec![vp("name", type_name_ty(), span)], type_ty(), span)
        }
        "__type_apply__" => refl_scheme_from_parts(
            vec![vp("head", type_ty(), span), vp("arg", type_ty(), span)],
            type_ty(),
            span,
        ),
        "__type_instantiate__" => refl_scheme_from_parts(
            vec![vp("scheme", type_ty(), span), vp("arg", type_ty(), span)],
            ty_sum(type_ty(), diagnostic_ty(), span),
            span,
        ),
        "__type_arity__" => {
            refl_scheme_from_parts(vec![vp("typ", type_ty(), span)], type_arity_ty(), span)
        }
        "__type_var_arity__" => {
            refl_scheme_from_parts(vec![vp("var", type_var_ty(), span)], type_arity_ty(), span)
        }
        "__type_arity_zero__" => refl_scheme_from_parts(Vec::new(), type_arity_ty(), span),
        "__type_arity_succ__" => refl_scheme_from_parts(
            vec![vp("prev", type_arity_ty(), span)],
            type_arity_ty(),
            span,
        ),
        "__type_arity_equal__" => refl_scheme_from_parts(
            vec![
                vp("left", type_arity_ty(), span),
                vp("right", type_arity_ty(), span),
            ],
            refl_predicate_ty(span, roles)?,
            span,
        ),
        "__type_product_spine_fold__"
        | "__type_sum_spine_fold__"
        | "__type_args_fold__"
        | "__type_function_params_fold__" => refl_scheme_from_parts(
            vec![
                tp("r", span),
                vp("typ", type_ty(), span),
                vp("init", r_ty(), span),
                vp("step", ty_func(vec![r_ty(), type_ty()], r_ty(), span), span),
            ],
            r_ty(),
            span,
        ),
        "__type_arity_fold__" => refl_scheme_from_parts(
            vec![
                tp("r", span),
                vp("arity", type_arity_ty(), span),
                vp("init", r_ty(), span),
                vp("step", ty_func(vec![r_ty()], r_ty(), span), span),
            ],
            r_ty(),
            span,
        ),
        "__type_name_param_arities_fold__" => refl_scheme_from_parts(
            vec![
                tp("r", span),
                vp("name", type_name_ty(), span),
                vp("init", r_ty(), span),
                vp(
                    "step",
                    ty_func(vec![r_ty(), type_arity_ty()], r_ty(), span),
                    span,
                ),
            ],
            r_ty(),
            span,
        ),
        "__type_display__" | "__type_short_name__" => {
            refl_scheme_from_parts(vec![vp("typ", type_ty(), span)], diagnostic_ty(), span)
        }
        "__type_name_display__" => refl_scheme_from_parts(
            vec![vp("name", type_name_ty(), span)],
            diagnostic_ty(),
            span,
        ),
        "__type_var_display__" => {
            refl_scheme_from_parts(vec![vp("var", type_var_ty(), span)], diagnostic_ty(), span)
        }
        "__type_arity_display__" => refl_scheme_from_parts(
            vec![vp("arity", type_arity_ty(), span)],
            diagnostic_ty(),
            span,
        ),
        "__diagnostic_concat__" => refl_scheme_from_parts(
            vec![
                vp("left", diagnostic_ty(), span),
                vp("right", diagnostic_ty(), span),
            ],
            diagnostic_ty(),
            span,
        ),
        "__elab_error__" => refl_scheme_from_parts(
            vec![vp("message", diagnostic_ty(), span)],
            checked_term_ty(),
            span,
        ),
        "__type_error__" => refl_scheme_from_parts(
            vec![vp("message", diagnostic_ty(), span)],
            checked_term_ty(),
            span,
        ),
        "__structural_recur__" => refl_scheme_from_parts(
            vec![
                tp("fuel", span),
                tp("input", span),
                tp("result", span),
                vp("fuel", fuel_ty(), span),
                vp("input", input_ty(), span),
                vp(
                    "step",
                    ty_func(
                        vec![
                            ty_func(vec![fuel_ty(), input_ty()], result_ty(), span),
                            fuel_ty(),
                            input_ty(),
                        ],
                        result_ty(),
                        span,
                    ),
                    span,
                ),
            ],
            result_ty(),
            span,
        ),
        "__term_type__" => {
            refl_scheme_from_parts(vec![vp("term", checked_term_ty(), span)], type_ty(), span)
        }
        "__term_let__" => refl_scheme_from_parts(
            vec![
                vp("value_type", type_ty(), span),
                vp("value", checked_term_ty(), span),
                vp(
                    "body",
                    ty_func(vec![checked_term_ty()], checked_term_ty(), span),
                    span,
                ),
            ],
            checked_term_ty(),
            span,
        ),
        "__term_fn__" => refl_scheme_from_parts(
            vec![
                vp("fn_type", type_ty(), span),
                vp(
                    "body",
                    ty_func(vec![checked_term_ty()], checked_term_ty(), span),
                    span,
                ),
            ],
            checked_term_ty(),
            span,
        ),
        "__term_call__" => refl_scheme_from_parts(
            vec![
                vp("fn_type", type_ty(), span),
                vp("fn_value", checked_term_ty(), span),
                vp("arg_packet", checked_term_ty(), span),
            ],
            checked_term_ty(),
            span,
        ),
        "__term_type_fn__" => refl_scheme_from_parts(
            vec![
                vp("var_arity", type_arity_ty(), span),
                vp(
                    "body",
                    ty_func(vec![type_var_ty()], checked_term_ty(), span),
                    span,
                ),
            ],
            checked_term_ty(),
            span,
        ),
        "__term_type_app__" => refl_scheme_from_parts(
            vec![
                vp("fn_value", checked_term_ty(), span),
                vp("arg", type_ty(), span),
            ],
            checked_term_ty(),
            span,
        ),
        "__term_unit__" => refl_scheme_from_parts(Vec::new(), checked_term_ty(), span),
        "__term_host_convert__" => refl_scheme_from_parts(
            vec![
                vp("string_type", type_ty(), span),
                vp("source_type", type_ty(), span),
                vp("converters", checked_term_ty(), span),
                vp("value", checked_term_ty(), span),
            ],
            checked_term_ty(),
            span,
        ),
        "__fill__" => refl_scheme_from_parts(
            vec![
                vp("fills", fill_ctx_ty(span), span),
                vp("destination", type_ty(), span),
                vp("candidate", type_ty(), span),
            ],
            fill_ctx_ty(span),
            span,
        ),
        "__term_specialize__" => refl_scheme_from_parts(
            vec![
                vp("term", checked_term_ty(), span),
                vp("pattern", type_ty(), span),
                vp("target", type_ty(), span),
            ],
            ty_sum(
                checked_term_ty(),
                ty_sum(
                    crate::ast::Type::Unit {
                        meta: Meta::new(span),
                    },
                    diagnostic_ty(),
                    span,
                ),
                span,
            ),
            span,
        ),
        // Reflected intrinsic constructors intentionally mirror `__intrinsics__`
        // names so the elaborator ABI stays a mechanical mapping.
        "__intrinsic_pair__" => refl_scheme_from_parts(
            vec![
                vp("left", checked_term_ty(), span),
                vp("right", checked_term_ty(), span),
            ],
            checked_term_ty(),
            span,
        ),
        "__intrinsic_fst__" | "__intrinsic_snd__" => refl_scheme_from_parts(
            vec![
                vp("product_type", type_ty(), span),
                vp("value", checked_term_ty(), span),
            ],
            checked_term_ty(),
            span,
        ),
        "__intrinsic_left__" | "__intrinsic_right__" => refl_scheme_from_parts(
            vec![
                vp("sum_type", type_ty(), span),
                vp("value", checked_term_ty(), span),
            ],
            checked_term_ty(),
            span,
        ),
        "__intrinsic_either__" => refl_scheme_from_parts(
            vec![
                vp("sum_type", type_ty(), span),
                vp("result_type", type_ty(), span),
                vp("value", checked_term_ty(), span),
                vp(
                    "on_left",
                    ty_func(vec![checked_term_ty()], checked_term_ty(), span),
                    span,
                ),
                vp(
                    "on_right",
                    ty_func(vec![checked_term_ty()], checked_term_ty(), span),
                    span,
                ),
            ],
            checked_term_ty(),
            span,
        ),
        "__intrinsic_absurd__" => refl_scheme_from_parts(
            vec![
                vp("bottom_value", checked_term_ty(), span),
                vp("result_type", type_ty(), span),
            ],
            checked_term_ty(),
            span,
        ),
        "__intrinsic_if_then_else__" => refl_scheme_from_parts(
            vec![
                vp("result_type", type_ty(), span),
                vp("condition", checked_term_ty(), span),
                vp(
                    "on_true",
                    ty_func(Vec::new(), checked_term_ty(), span),
                    span,
                ),
                vp(
                    "on_false",
                    ty_func(Vec::new(), checked_term_ty(), span),
                    span,
                ),
            ],
            checked_term_ty(),
            span,
        ),
        _ => return Ok(None),
    };
    Ok(Some(scheme))
}

/// Returns the scheme [`Synth`] for an intrinsic value reference,
/// or `None` if `name` isn't one of the supported intrinsics.
///
/// The supported intrinsics are `__left__`, `__right__`,
/// `__either__`, `__pair__`, `__fst__`, `__snd__`, `__absurd__`,
/// and `__if_then_else__`.
///
/// `bool_role` is the consuming module's exact `role(bool)` resolution.
/// Most entries ignore it; `__if_then_else__` requires a singleton and
/// returns `Some(Err(...))` when it is missing or ambiguous.
///
/// The outer `Option` distinguishes "name not an intrinsic" (`None`)
/// from "name is an intrinsic but its scheme can't be built right
/// now" (`Some(Err)`). Only `__if_then_else__` can return the inner `Err`;
/// the role-independent intrinsics always succeed.
///
/// Phase-polymorphic — the helper builds AST nodes via
/// [`ty_path`] / [`ty_sum`] / [`ty_product`] / [`ty_func`] /
/// [`tp`] / [`vp`], all of which work on any `P: Phase`.
pub(crate) fn intrinsic_scheme_resolved<P>(
    name: &str,
    span: Span,
    bool_role: super::RoleResolution<'_>,
    bool_role_ambiguity: Option<&str>,
) -> Option<Result<Synth<P>, Error>>
where
    P: crate::ast::Phase + Clone,
{
    let a = || ty_path::<P>("a", span);
    let b = || ty_path::<P>("b", span);
    let c = || ty_path::<P>("c", span);
    let sum_ab = || ty_sum(a(), b(), span);
    let prod_ab = || ty_product(a(), b(), span);

    let (params, ret) = match name {
        // __left__ : [A][B](x: A) -> A | B
        "__left__" => (
            vec![tp("a", span), tp("b", span), vp("x", a(), span)],
            sum_ab(),
        ),
        // __right__ : [A][B](x: B) -> A | B
        "__right__" => (
            vec![tp("a", span), tp("b", span), vp("x", b(), span)],
            sum_ab(),
        ),
        // __either__ : [A][B][C] ((A | B) & (A -> C) & (B -> C)) -> C
        "__either__" => (
            vec![
                tp("a", span),
                tp("b", span),
                tp("c", span),
                vp("s", sum_ab(), span),
                vp("fl", ty_func(vec![a()], c(), span), span),
                vp("fr", ty_func(vec![b()], c(), span), span),
            ],
            c(),
        ),
        // __pair__ : [A][B](x: A, y: B) -> A & B
        "__pair__" => (
            vec![
                tp("a", span),
                tp("b", span),
                vp("x", a(), span),
                vp("y", b(), span),
            ],
            prod_ab(),
        ),
        // __fst__ : [A][B](p: A & B) -> A
        "__fst__" => (
            vec![tp("a", span), tp("b", span), vp("p", prod_ab(), span)],
            a(),
        ),
        // __snd__ : [A][B](p: A & B) -> B
        "__snd__" => (
            vec![tp("a", span), tp("b", span), vp("p", prod_ab(), span)],
            b(),
        ),
        // __absurd__ : [A](x: !) -> A
        "__absurd__" => (
            vec![
                tp("a", span),
                vp(
                    "x",
                    crate::ast::Type::Bottom {
                        meta: Meta::new(span),
                    },
                    span,
                ),
            ],
            a(),
        ),
        // __if_then_else__ : [A](c: Bool, t: . -> A, e: . -> A) -> A
        //
        // The unique role-bound intrinsic: `Bool` is the singleton host
        // type, or singleton role-inheriting alias fallback, carrying
        // `role(bool)` in the consuming module. A missing or ambiguous
        // binding leaves no single scheme for the intrinsic.
        "__if_then_else__" => {
            let bool_name = match bool_role {
                super::RoleResolution::Unique(name) => name,
                super::RoleResolution::Missing => {
                    return Some(Err(Error::type_(
                        span,
                        "no `role(bool)` host type is in unqualified lexical scope; \
                         `__if_then_else__` requires one",
                    )
                    .with_help(
                        "declare or selectively import a host type marked `role(bool)`, \
                         or define a role-inheriting alias in unqualified lexical scope",
                    )));
                }
                super::RoleResolution::Ambiguous { first, second } => {
                    let candidates = bool_role_ambiguity
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("`{first}`, `{second}`, …"));
                    return Some(Err(Error::type_(
                        span,
                        format!(
                            "more than one `role(bool)` type is in unqualified lexical scope \
                             ({candidates}); \
                             `__if_then_else__` requires exactly one"
                        ),
                    )
                    .with_help(
                        "leave exactly one `role(bool)` identity in unqualified lexical scope; \
                         use qualified imports for the others",
                    )));
                }
            };
            let bool_ty =
                crate::ast::Type::synth_path(vec![bool_name.to_owned()], Vec::new(), span);
            let unit_to_a = || ty_func(vec![], a(), span);
            (
                vec![
                    tp("a", span),
                    vp("c", bool_ty, span),
                    vp("t", unit_to_a(), span),
                    vp("e", unit_to_a(), span),
                ],
                a(),
            )
        }
        _ => return None,
    };
    Some(Ok(Synth::complete_scheme(
        crate::ast::Type::synth_scheme_from_signature_params(&params, ret, span),
    )))
}

/// Build an intrinsic scheme from a caller-supplied singleton role table.
/// Module typing uses [`intrinsic_scheme_resolved`] so ambiguity remains
/// observable; this entry point serves callers that already selected roles.
pub fn intrinsic_scheme<P>(
    name: &str,
    span: Span,
    roles: &HashMap<crate::ast::Role, &str>,
) -> Option<Result<Synth<P>, Error>>
where
    P: crate::ast::Phase + Clone,
{
    let bool_role = roles
        .get(&crate::ast::Role::Bool)
        .copied()
        .map(super::RoleResolution::Unique)
        .unwrap_or(super::RoleResolution::Missing);
    intrinsic_scheme_resolved(name, span, bool_role, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Lowered, Surface};

    #[cfg(feature = "prime")]
    #[test]
    fn wrong_namespace_nominal_diagnostic_requires_the_actual_local_cutoff() {
        let module = crate::prime::lower::lower_module(
            crate::pass::parser::parse("module main; type Value = .;").unwrap(),
        )
        .unwrap();
        let env = super::super::ModuleEnv::build(&module, None, None, None).unwrap();
        let span = Span::new(50, 55);
        let segments = vec![crate::ast::PathSegment::new("Value".to_owned(), span)];
        let mut elaborations = crate::prime::typer::PrimeElaborations::default();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        assert!(type_name_in_value_position(&segments, span, &tcx).is_none());
        let tcx = tcx.at_item(0);
        assert!(type_name_in_value_position(&segments, span, &tcx).is_none());
        let mut tcx = tcx.at_item(1);
        let error = type_name_in_value_position(&segments, span, &tcx).unwrap();
        assert_eq!(error.diagnostic().message, "`Value` is a type, not a value");
        assert_eq!(
            error.diagnostic().secondary()[0].text,
            "type alias declared here"
        );
        tcx.push_type_param("Value", Span::new(30, 35));
        assert!(
            type_name_in_value_position(&segments, span, &tcx).is_none(),
            "synthesis must not guess a pre-normalization lexical identity"
        );
    }

    fn alias_ctx_with<'a, 'm>(
        name: &'m str,
        body: &'m crate::ast::Type<Lowered>,
    ) -> AliasCtx<'a, 'm, Lowered> {
        let mut local = HashMap::new();
        local.insert(
            name,
            super::super::AliasDef {
                type_params: &[],
                body,
                owner_module: None,
            },
        );
        let local = Box::leak(Box::new(local));
        let cross = Box::leak(Box::new(HashMap::new()));
        AliasCtx {
            local,
            cross_module: cross,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        }
    }

    #[test]
    fn value_type_rejects_incomplete_named_scheme() {
        let span = Span::new(0, 8);
        let incomplete = crate::ast::Type::<Lowered>::Forall {
            param: crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            },
            body: Box::new(crate::ast::Type::synth_path(
                vec!["A".to_owned()],
                Vec::new(),
                span,
            )),
            meta: Meta::new(span),
        };

        let error = synth_value_type(Synth::scheme(incomplete))
            .expect_err("an incomplete named scheme is not a value type");
        assert!(
            error
                .diag()
                .1
                .contains("polymorphic value used in a monomorphic position"),
            "got: {:?}",
            error.diag()
        );
    }

    #[test]
    fn value_type_uses_the_producer_scheme_summary_without_rescanning() {
        let span = Span::new(0, 8);
        let a = crate::ast::Type::<Lowered>::synth_path(vec!["A".to_owned()], Vec::new(), span);
        let unclassified = crate::ast::Type::Forall {
            param: crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            },
            body: Box::new(crate::ast::Type::synth_function(vec![a.clone()], a, span)),
            meta: Meta::new(span),
        };

        super::super::kind_scheme::reset_complete_scheme_consumer_work();
        let error = synth_value_type(Synth::scheme(unclassified))
            .expect_err("a raw callable shape cannot replace the producer's scheme summary");
        assert!(
            error
                .diag()
                .1
                .contains("polymorphic value used in a monomorphic position"),
            "got: {:?}",
            error.diag()
        );
        assert_eq!(
            super::super::kind_scheme::complete_scheme_consumer_work().synthesized_result,
            1,
            "the value classifier must consume the producer summary exactly once"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn top_fn_producers_carry_the_complete_scheme_summary() {
        let surface =
            crate::pass::parser::parse("module main; fn identity[A](value: A) -> A { value }")
                .expect("parse polymorphic function");
        let (modules, _) =
            <crate::pass::full::FullPipeline as crate::pipeline::Pipeline>::lower_package(
                vec![(std::path::PathBuf::from("main.kio"), surface)],
                None,
            )
            .expect("lower polymorphic function");
        let module = &modules[0].1;
        let def = module
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::FnDef(def) if def.name == "identity" => Some(def),
                _ => None,
            })
            .expect("identity function");
        let span = def.meta.span;

        for synth in [
            synth_top_fn_def(def, span),
            synth_top_fn_def_in_module(def, module, None, span),
        ] {
            assert!(synth.is_scheme);
            assert!(
                synth.complete_scheme.is_complete(),
                "a signature-built callable producer must carry its complete-scheme summary"
            );
        }
    }

    #[test]
    fn collect_fn_param_bindings_unfolds_alias_spine() {
        let span = Span::new(0, 0);
        let unit = crate::ast::Type::Unit {
            meta: Meta::new(span),
        };
        let pair = Box::leak(Box::new(ty_product(unit.clone(), unit, span)));
        let ctx = alias_ctx_with("Pair", pair);
        let aliased_pair =
            crate::ast::Type::<Lowered>::synth_path(vec!["Pair".to_owned()], Vec::new(), span);

        let slots = collect_fn_param_bindings_interned(
            &super::super::InternedType::fresh(aliased_pair),
            2,
            &ctx,
        )
        .unwrap();

        assert_eq!(slots.len(), 2);
        assert!(matches!(slots[0].as_type(), crate::ast::Type::Unit { .. }));
        assert!(matches!(slots[1].as_type(), crate::ast::Type::Unit { .. }));
    }

    #[test]
    fn comptime_value_helpers_require_proof_first() {
        let span = Span::new(0, 0);
        let mut roles = HashMap::new();
        roles.insert(crate::ast::Role::Bool, "Comptime_bool");
        roles.insert(crate::ast::Role::Str, "Comptime_str");

        let mut checked = 0usize;
        for name in crate::comptime::PUBLIC_COMPTIME_NAMES {
            let builtin = crate::comptime::ComptimeBuiltin::from_public_name(name)
                .expect("public comptime name should map to builtin");
            if !builtin.is_value_name() {
                continue;
            }
            checked += 1;
            let scheme = comptime_scheme::<Surface>(builtin, span, &roles)
                .expect("comptime scheme should build")
                .expect("value builtin should have scheme");
            let crate::ast::Type::Function {
                param, abi_arity, ..
            } = scheme.ty.as_type()
            else {
                panic!("`{name}` scheme does not start with a value parameter");
            };
            let value_params = crate::ast::Type::right_spine_take(param, *abi_arity);
            let Some(first_param) = value_params.first() else {
                panic!("`{name}` scheme has no value parameters");
            };
            let crate::ast::Type::Path { segments, args, .. } = first_param else {
                panic!("`{name}` first value parameter is not `__Comptime__`: {param:?}");
            };
            assert!(
                args.is_empty() && segments.len() == 1 && segments[0].as_str() == "__Comptime__",
                "`{name}` first value parameter is not `__Comptime__`: {param:?}"
            );
        }
        assert!(checked > 0, "no comptime value helpers were checked");
    }

    #[test]
    fn fill_helpers_have_the_exact_split_capability_schemes() {
        let span = Span::new(0, 0);
        let mut roles = HashMap::new();
        roles.insert(crate::ast::Role::Bool, "Comptime_bool");
        roles.insert(crate::ast::Role::Str, "Comptime_str");

        let fill = comptime_scheme::<Surface>(ComptimeBuiltin::Fill, span, &roles)
            .unwrap()
            .unwrap();
        assert_eq!(
            crate::pretty::pretty_type(&fill.ty),
            "(__Comptime__ & __Fill_ctx__ & __Type__ & __Type__) -> __Fill_ctx__"
        );

        let specialize = comptime_scheme::<Surface>(ComptimeBuiltin::TermSpecialize, span, &roles)
            .unwrap()
            .unwrap();
        assert_eq!(
            crate::pretty::pretty_type(&specialize.ty),
            "(__Comptime__ & __Checked_term__ & __Type__ & __Type__) ->\n  __Checked_term__ | . | __Diagnostic_text__\n"
        );
    }
}
