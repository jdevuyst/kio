//! Variance lattice and strict-positivity check for `newtype`
//! payloads. The lattice + composition rules are the direct port
//! of `specs/formal/prime.md` § 2.5; the [`compute_variance_env`]
//! fixpoint and [`check_newtype_payload`] walk together discharge
//! Kio's strict-positivity rule on every recursive use site.
//!
//! Extracted from [`super::typecheck_core`] for navigability;
//! depends on [`super::PayloadCtx`] / [`super::AliasCtx`] /
//! [`super::unfold_top`] which still live in the umbrella.

use std::collections::{HashMap, HashSet};

use crate::error::Error;

use super::{PayloadCtx, aliases::unfold_and_qualify_state};

/// Variance lattice for the strict-positivity check. The four
/// elements form a complete lattice under [`Variance::join`]; the
/// composition operator [`Variance::compose`] threads variance
/// through nested type positions (e.g., descending into the LHS of
/// a `.>` composes with `Neg`).
///
/// See `specs/formal/prime.md` § 2.5 for the formal account; this
/// is a direct port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variance {
    /// `0` — the parameter doesn't occur in this position (phantom).
    Unused,
    /// `+` — covariant.
    Pos,
    /// `−` — contravariant.
    Neg,
    /// `*` — invariant. Used for host-type parameter slots (whose
    /// variance is unknowable) and for parameters that appear in
    /// both covariant and contravariant positions.
    Inv,
}

impl Variance {
    /// Composition (descending into nested positions): `outer ∘ inner`.
    /// Reads "the variance of an `inner`-varying child position
    /// inside an `outer`-varying parent."
    ///
    /// `+ ∘ v = v` (positive preserves), `- ∘ v` flips, `* ∘ v` is
    /// `0` if `v` is `0` and `*` otherwise (invariant absorbs),
    /// `0 ∘ v = 0` (unused stays unused).
    pub fn compose(self, inner: Variance) -> Variance {
        match (self, inner) {
            (Variance::Unused, _) | (_, Variance::Unused) => Variance::Unused,
            (Variance::Pos, v) => v,
            (Variance::Neg, Variance::Pos) => Variance::Neg,
            (Variance::Neg, Variance::Neg) => Variance::Pos,
            (Variance::Neg, Variance::Inv) => Variance::Inv,
            (Variance::Inv, _) => Variance::Inv,
        }
    }

    /// Join (combining variances when a parameter appears in
    /// multiple positions).
    ///
    /// `0` is identity (`0 ∨ v = v`); `+ ∨ - = *` (a parameter that
    /// appears in both covariant and contravariant positions is
    /// invariant); `* ∨ v = *` (invariant absorbs).
    pub fn join(self, other: Variance) -> Variance {
        match (self, other) {
            (Variance::Unused, v) | (v, Variance::Unused) => v,
            (Variance::Inv, _) | (_, Variance::Inv) => Variance::Inv,
            (Variance::Pos, Variance::Pos) => Variance::Pos,
            (Variance::Neg, Variance::Neg) => Variance::Neg,
            (Variance::Pos, Variance::Neg) | (Variance::Neg, Variance::Pos) => Variance::Inv,
        }
    }

    /// True iff this variance is strictly-positive: `+` or `0`.
    /// Used as the strict-positivity acceptance criterion at every
    /// recursive use site: a reference to the current newtype or to
    /// another member of its recursive type SCC.
    pub fn is_strictly_positive(self) -> bool {
        matches!(self, Variance::Pos | Variance::Unused)
    }
}

/// Per-newtype variance table computed by [`compute_variance_env`].
/// Imported provider modules are memoized for the lifetime of one
/// strict-positivity request.
pub struct VarianceEnv {
    local: HashMap<String, Vec<Variance>>,
    imported: ImportedVarianceCache,
}

impl VarianceEnv {
    pub fn get(&self, name: &str) -> Option<&Vec<Variance>> {
        self.local.get(name)
    }
}

enum ImportedVarianceState {
    InProgress,
    Ready(HashMap<String, Vec<Variance>>),
}

#[derive(Default)]
struct ImportedVarianceCache {
    modules: HashMap<String, ImportedVarianceState>,
    #[cfg(test)]
    derivations: HashMap<String, usize>,
    // Frozen causal comparator: production always reuses `Ready` entries.
    #[cfg(test)]
    bypass_ready_reuse: bool,
}

#[cfg(all(test, feature = "surface"))]
impl ImportedVarianceCache {
    fn without_ready_reuse() -> Self {
        Self {
            bypass_ready_reuse: true,
            ..Self::default()
        }
    }
}

/// Partition of the module's `newtype`s into strongly-connected
/// components under the "appears in payload" relation: maps each
/// newtype name to the set of newtype names sharing its SCC (the set
/// always includes the key itself). Computed by
/// [`compute_newtype_sccs`] and consulted by [`check_newtype_payload`]
/// so the strict-positivity check sees the *whole* recursive cycle,
/// not just a newtype's direct self-references.
///
/// Strict positivity is a property of the recursive group, not of one
/// declaration: `rec { newtype Na : . | (Nb -> .); newtype Nb : . |
/// (Na -> .); }` and `rec { newtype P : . | (Q -> .); newtype Q : P; }` each
/// unfold to a self-reference under an arrow LHS, yet a per-newtype
/// check that treats sibling nominals as opaque admits both and breaks
/// strong normalization (an `Ω` is constructible). Enforcing strict
/// positivity across the SCC — no SCC member may occur left-of-arrow
/// in any SCC member's payload — closes that gap.
pub type NewtypeSccs = HashMap<String, HashSet<String>>;

fn same_module_newtype_name<'a, P>(
    segments: &'a [crate::ast::PathSegment],
    ctx: &PayloadCtx<'_, '_, P>,
) -> Option<&'a str>
where
    P: crate::ast::Phase,
{
    let (name, module_segments) = segments.split_last()?;
    if module_segments.is_empty() {
        return Some(name.as_str());
    }
    let module = ctx.aliases.source_module?;
    (module_segments.len() == module.path.segments.len()
        && module_segments
            .iter()
            .zip(&module.path.segments)
            .all(|(actual, expected)| actual.as_str() == expected.name.as_str()))
    .then(|| name.as_str())
}

fn unfold_variance_top<P>(
    ty: &crate::ast::Type<P>,
    ctx: &PayloadCtx<'_, '_, P>,
    descendants_are_canonical: bool,
    bound: &HashSet<String>,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    if !descendants_are_canonical
        && let crate::ast::Type::Path { segments, .. } = ty
        && matches!(segments.as_slice(), [name] if bound.contains(name.as_str()))
    {
        return (ty.clone(), false);
    }
    let aliases = super::AliasCtx {
        binder_locals: Some(bound),
        ..ctx.aliases
    };
    unfold_and_qualify_state(ty, &aliases, descendants_are_canonical)
}

/// Collect the path heads in `ty` that identify another `newtype` in
/// the module, respecting alias unfolding and binder shadowing. These
/// are the out-edges of `ty`'s owner in the newtype
/// dependency graph that [`compute_newtype_sccs`] builds its SCCs
/// over. Type-parameter and `Forall`-binder names are excluded: a
/// single-segment path equal to an in-scope binder refers to the
/// binder, not to a same-named newtype.
fn collect_newtype_refs<P>(
    ty: &crate::ast::Type<P>,
    shadowed: &HashSet<String>,
    ctx: &PayloadCtx<'_, '_, P>,
    out: &mut HashSet<String>,
) where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    collect_newtype_refs_state(ty, false, shadowed, ctx, out);
}

fn collect_newtype_refs_state<P>(
    ty: &crate::ast::Type<P>,
    descendants_are_canonical: bool,
    shadowed: &HashSet<String>,
    ctx: &PayloadCtx<'_, '_, P>,
    out: &mut HashSet<String>,
) where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let (unfolded, descendants_are_canonical) =
        unfold_variance_top(ty, ctx, descendants_are_canonical, shadowed);
    match &unfolded {
        crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => {}
        crate::ast::Type::Function { param, ret, .. } => {
            collect_newtype_refs_state(param, descendants_are_canonical, shadowed, ctx, out);
            collect_newtype_refs_state(ret, descendants_are_canonical, shadowed, ctx, out);
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            collect_newtype_refs_state(left, descendants_are_canonical, shadowed, ctx, out);
            collect_newtype_refs_state(right, descendants_are_canonical, shadowed, ctx, out);
        }
        crate::ast::Type::Path { segments, args, .. } => {
            if let Some(name) = same_module_newtype_name(segments, ctx)
                && (segments.len() > 1 || !shadowed.contains(name))
                && ctx.newtypes.contains_key(name)
            {
                out.insert(name.to_owned());
            }
            for a in args {
                collect_newtype_refs_state(a, descendants_are_canonical, shadowed, ctx, out);
            }
        }
        crate::ast::Type::Forall { param, body, .. } => {
            let mut inner = shadowed.clone();
            inner.insert(param.name.clone());
            collect_newtype_refs_state(body, descendants_are_canonical, &inner, ctx, out);
        }
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        crate::ast::Type::Infer { .. } => unreachable!(
            "Type::Infer should be resolved by the typer's substitution pass; check_well_formed_type catches it at the boundary"
        ),
        crate::ast::Type::Goal { .. } => {
            unreachable!("an internal Type::Goal cannot occur in a newtype declaration payload")
        }
    }
}

/// Compute the strongly-connected components of the module's
/// `newtype`s under the "appears in payload" relation (see
/// [`collect_newtype_refs`] for what counts as an edge). The result
/// maps every newtype name to the set of names in its SCC; a newtype
/// with no recursive participation maps to the singleton of itself.
///
/// `M` and `N` share an SCC iff `N` reaches `M` and `M` reaches `N`
/// in the dependency graph. We materialize the forward-reachability
/// closure once (each newtype's set of reachable newtypes, including
/// itself), then intersect with the same relation read backwards. The
/// graph is the module's finite set of newtypes, so the transitive
/// closure converges by monotone fixpoint.
pub fn compute_newtype_sccs<P>(ctx: &PayloadCtx<'_, '_, P>) -> NewtypeSccs
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let mut reach: HashMap<String, HashSet<String>> = ctx
        .newtypes
        .iter()
        .map(|(name, d)| {
            let bound: HashSet<String> = d
                .type_params
                .iter()
                .chain(d.existential_params.iter())
                .map(|tp| tp.name.clone())
                .collect();
            let mut seed = HashSet::new();
            collect_newtype_refs(&d.payload, &bound, ctx, &mut seed);
            seed.insert((*name).to_owned());
            ((*name).to_owned(), seed)
        })
        .collect();

    let mut changed = true;
    while changed {
        changed = false;
        let names: Vec<String> = reach.keys().cloned().collect();
        for name in &names {
            let succ: Vec<String> = reach[name].iter().cloned().collect();
            let mut additions: HashSet<String> = HashSet::new();
            for s in &succ {
                if let Some(s_reach) = reach.get(s) {
                    for t in s_reach {
                        if !reach[name].contains(t) {
                            additions.insert(t.clone());
                        }
                    }
                }
            }
            if !additions.is_empty() {
                let entry = reach.get_mut(name).expect("name in reach");
                for t in additions {
                    entry.insert(t);
                }
                changed = true;
            }
        }
    }

    reach
        .keys()
        .map(|name| {
            let component: HashSet<String> = reach
                .keys()
                .filter(|other| reach[name].contains(*other) && reach[*other].contains(name))
                .cloned()
                .collect();
            (name.clone(), component)
        })
        .collect()
}

/// Compute the per-newtype variance table for a module's parametric
/// `newtype`s by mutual monotone fixpoint. Each parameter starts at
/// [`Variance::Unused`] (`0`) and is iteratively raised by joining
/// in the variance computed from a walk of the newtype's payload.
/// Composition through nested newtype applications consults the
/// table being built — which is monotone and finite, so the
/// fixpoint converges in O(parameters × lattice height) iterations.
///
/// Host types and other non-newtype heads contribute [`Variance::Inv`]
/// at every parameter slot (their variance is unknowable from inside
/// the package). This is the conservative-by-default rule that makes
/// any recursive use site under a host-type type-argument fail the
/// strict-positivity check — closing the soundness hole where a host
/// type's unknown variance could otherwise let a non-strictly-
/// positive recursive use slip through the payload check.
pub fn compute_variance_env<P>(ctx: &PayloadCtx<'_, '_, P>) -> VarianceEnv
where
    P: crate::pass::resolve::ResolvePhase + Clone,
{
    let mut imported = ImportedVarianceCache::default();
    let local = compute_local_variance_env(ctx, &mut imported);
    VarianceEnv { local, imported }
}

#[cfg(all(test, feature = "surface"))]
fn compute_variance_env_without_imported_ready_reuse<P>(ctx: &PayloadCtx<'_, '_, P>) -> VarianceEnv
where
    P: crate::pass::resolve::ResolvePhase + Clone,
{
    let mut imported = ImportedVarianceCache::without_ready_reuse();
    let local = compute_local_variance_env(ctx, &mut imported);
    VarianceEnv { local, imported }
}

fn compute_local_variance_env<P>(
    ctx: &PayloadCtx<'_, '_, P>,
    imported: &mut ImportedVarianceCache,
) -> HashMap<String, Vec<Variance>>
where
    P: crate::pass::resolve::ResolvePhase + Clone,
{
    let mut env = ctx
        .newtypes
        .iter()
        .map(|(name, d)| {
            (
                (*name).to_owned(),
                vec![Variance::Unused; d.type_params.len()],
            )
        })
        .collect();

    let mut changed = true;
    while changed {
        changed = false;
        for (name, d) in ctx.newtypes.iter() {
            let bound: HashSet<String> = d
                .type_params
                .iter()
                .chain(d.existential_params.iter())
                .map(|tp| tp.name.clone())
                .collect();
            for (i, tp) in d.type_params.iter().enumerate() {
                let new_v =
                    variance_in_type(&d.payload, &tp.name, &env, imported, ctx, false, &bound);
                let combined = env[*name][i].join(new_v);
                if combined != env[*name][i] {
                    env.get_mut(*name).expect("name in env")[i] = combined;
                    changed = true;
                }
            }
        }
    }
    env
}

fn imported_slot_variance<P>(
    segments: &[crate::ast::PathSegment],
    index: usize,
    ctx: &PayloadCtx<'_, '_, P>,
    imported: &mut ImportedVarianceCache,
) -> Option<Variance>
where
    P: crate::pass::resolve::ResolvePhase + Clone,
{
    let (name, module_segments) = segments.split_last()?;
    if module_segments.is_empty() || same_module_newtype_name(segments, ctx).is_some() {
        return None;
    }
    let module_path = module_segments
        .iter()
        .map(crate::ast::PathSegment::as_str)
        .collect::<Vec<_>>()
        .join("/");
    // The resolved package's import graph is acyclic, so following an exact
    // provider declaration recursively terminates. The `(module, name)`
    // key is fixed by the written import and cannot be perturbed by adding
    // another declaration elsewhere.
    match imported.modules.get(&module_path) {
        #[cfg(not(test))]
        Some(ImportedVarianceState::Ready(variances)) => {
            return variances.get(name.as_str())?.get(index).copied();
        }
        #[cfg(test)]
        Some(ImportedVarianceState::Ready(variances)) if !imported.bypass_ready_reuse => {
            return variances.get(name.as_str())?.get(index).copied();
        }
        #[cfg(test)]
        Some(ImportedVarianceState::Ready(_)) => {}
        // A cycle is impossible after resolved-package validation. Keeping
        // the slot unknown here is nevertheless proof-safe: the caller
        // treats it as invariant and therefore cannot admit bad recursion.
        Some(ImportedVarianceState::InProgress) => return None,
        None => {}
    }

    imported
        .modules
        .insert(module_path.clone(), ImportedVarianceState::InProgress);
    #[cfg(test)]
    {
        *imported.derivations.entry(module_path.clone()).or_default() += 1;
    }

    let computed = (|| {
        let package = ctx.aliases.package?;
        let entry = package.module(&module_path)?;
        let package_file = package.package_file().map(|entry| &entry.package_file);
        let package_name = package
            .package_file()
            .map(|entry| entry.package_name.as_str());
        let env = super::ModuleEnv::build(&entry.module, package_file, package_name, Some(package))
            .ok()?;
        Some(compute_local_variance_env(&env.payload_ctx(), imported))
    })()
    .unwrap_or_default();
    imported
        .modules
        .insert(module_path.clone(), ImportedVarianceState::Ready(computed));

    match imported.modules.get(&module_path)? {
        ImportedVarianceState::Ready(variances) => {
            variances.get(name.as_str())?.get(index).copied()
        }
        ImportedVarianceState::InProgress => unreachable!("provider variance derivation completed"),
    }
}

/// Variance of `param` in `ty`, given the per-newtype variance
/// table `env`. Recursive descent on the type structure with the
/// composition rules of [`Variance::compose`] and the join rules of
/// [`Variance::join`]. Type aliases are unfolded; the `Forall` case
/// shadows when a binder collides with `param`.
fn variance_in_type<P>(
    ty: &crate::ast::Type<P>,
    param: &str,
    env: &HashMap<String, Vec<Variance>>,
    imported: &mut ImportedVarianceCache,
    ctx: &PayloadCtx<'_, '_, P>,
    descendants_are_canonical: bool,
    bound: &HashSet<String>,
) -> Variance
where
    P: crate::pass::resolve::ResolvePhase + Clone,
{
    let (unfolded, descendants_are_canonical) =
        unfold_variance_top(ty, ctx, descendants_are_canonical, bound);
    match &unfolded {
        crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => Variance::Unused,
        crate::ast::Type::Function {
            param: fn_param,
            ret,
            ..
        } => {
            let v_ret = variance_in_type(
                ret,
                param,
                env,
                imported,
                ctx,
                descendants_are_canonical,
                bound,
            );
            let v_param = Variance::Neg.compose(variance_in_type(
                fn_param,
                param,
                env,
                imported,
                ctx,
                descendants_are_canonical,
                bound,
            ));
            v_ret.join(v_param)
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => variance_in_type(
            left,
            param,
            env,
            imported,
            ctx,
            descendants_are_canonical,
            bound,
        )
        .join(variance_in_type(
            right,
            param,
            env,
            imported,
            ctx,
            descendants_are_canonical,
            bound,
        )),
        crate::ast::Type::Path { segments, args, .. } => {
            // Single-segment, no args, matching name → this *is* the
            // parameter occurrence: contributes `+` (covariant).
            if segments.len() == 1 && segments[0] == param && args.is_empty() {
                return Variance::Pos;
            }
            // Otherwise, descend into args, composing each with the
            // target's slot variance. A non-newtype head (host type,
            // unknown name) contributes `Inv` for every slot.
            let target_variances = same_module_newtype_name(segments, ctx)
                .filter(|name| segments.len() > 1 || !bound.contains(*name))
                .and_then(|name| env.get(name));
            let args_variance = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let v_arg = variance_in_type(
                        a,
                        param,
                        env,
                        imported,
                        ctx,
                        descendants_are_canonical,
                        bound,
                    );
                    let v_slot = target_variances
                        .and_then(|vs| vs.get(i).copied())
                        .or_else(|| imported_slot_variance(segments, i, ctx, imported))
                        .unwrap_or(Variance::Inv);
                    v_slot.compose(v_arg)
                })
                .fold(Variance::Unused, Variance::join);
            // Abstract brand application `F(A)` where the kind-`*→*`
            // binder `f` *is* the variance parameter: the brand's
            // variance is unknown, so the head occurrence contributes
            // `Inv` conservatively (the same treatment unknown / host
            // slots get above).
            if !args.is_empty() && segments.len() == 1 && segments[0] == param {
                return args_variance.join(Variance::Inv);
            }
            args_variance
        }
        crate::ast::Type::Forall {
            param: binder,
            body,
            ..
        } => {
            if binder.name == param {
                // Binder shadows `param`; the body's occurrences
                // refer to the inner binder.
                Variance::Unused
            } else {
                let mut inner = bound.clone();
                inner.insert(binder.name.clone());
                variance_in_type(
                    body,
                    param,
                    env,
                    imported,
                    ctx,
                    descendants_are_canonical,
                    &inner,
                )
            }
        }
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        crate::ast::Type::Infer { .. } => unreachable!(
            "Type::Infer should be resolved by the typer's substitution pass; check_well_formed_type catches it at the boundary"
        ),
        crate::ast::Type::Goal { .. } => {
            unreachable!("an internal Type::Goal cannot occur in a newtype declaration payload")
        }
    }
}

#[derive(Clone, Copy)]
struct RecursiveGroup<'a> {
    current: &'a str,
    members: &'a HashSet<String>,
}

/// Validate a `newtype` payload. Enforces strict positivity of every
/// recursive use site (a `Type::Path` whose head names *any* member of
/// `current_newtype`'s SCC) and arity-checks the newtype's own
/// references against its declared parameters.
///
/// **Strict positivity**, per `specs/formal/prime.md` § 2.5: strict
/// positivity is a property of the recursive group, not of one
/// declaration. For every newtype `N`, no member of `N`'s
/// strongly-connected component (under "appears in payload") may occur
/// in `N`'s payload at a position whose **composed variance** (working
/// outward from the occurrence to the root of the payload) is not `+`
/// or `0`. Checking only the newtype's own direct self-references and
/// treating sibling nominals as opaque admits mutual / wrapper-indirect
/// cycles that unfold to a negative self-occurrence — enough to encode
/// `Ω` — so the check ranges over the whole SCC (`scc`, computed once by
/// [`compute_newtype_sccs`]). The walk threads the current position
/// through the type as a [`Variance`] value:
///
/// - Function LHS composes with `Neg` (arrow LHS flips).
/// - Function RHS, sums, and products preserve.
/// - Entering `Bar(args)` at slot `I` composes with `V[Bar][I]`,
///   the variance of `Bar`'s i-th parameter as computed by
///   [`compute_variance_env`].
/// - Non-newtype heads (host types, unknown names) contribute
///   [`Variance::Inv`] at every slot, conservatively. This is what
///   makes a self-reference under a host-type type-argument fail the
///   check.
///
/// The fixpoint variance environment composes through chains of
/// newtypes, so `newtype Neg[A] : A -> A; newtype Wrapped[A] : Neg(A);
/// rec newtype Loop : Wrapped(Loop);` is rejected (transitive contravariance
/// via two indirections), where the original one-step check would
/// have admitted it.
///
/// Type-alias references are unfolded during the walk so an alias
/// hiding a `.>` can't smuggle a self-reference past the check.
/// A `Forall` binder that shadows an SCC member's name takes that
/// name out of the recursive group for the binder's body.
///
/// Phase-polymorphic with `P::TypeLabelSugar = Never`.
pub fn check_newtype_payload<P>(
    ty: &crate::ast::Type<P>,
    pos: Variance,
    var_env: &mut VarianceEnv,
    ctx: &PayloadCtx<'_, '_, P>,
    current_newtype: &str,
    scc: &HashSet<String>,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ResolvePhase + Clone,
{
    let bound = ctx
        .newtypes
        .get(current_newtype)
        .map(|newtype| {
            newtype
                .type_params
                .iter()
                .chain(newtype.existential_params.iter())
                .map(|param| param.name.clone())
                .collect()
        })
        .unwrap_or_default();
    check_newtype_payload_shadowed(
        ty,
        pos,
        var_env,
        ctx,
        RecursiveGroup {
            current: current_newtype,
            members: scc,
        },
        &bound,
        false,
    )
}

fn check_newtype_payload_shadowed<P>(
    ty: &crate::ast::Type<P>,
    pos: Variance,
    var_env: &mut VarianceEnv,
    ctx: &PayloadCtx<'_, '_, P>,
    group: RecursiveGroup<'_>,
    shadowed: &HashSet<String>,
    descendants_are_canonical: bool,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ResolvePhase + Clone,
{
    let (unfolded, descendants_are_canonical) =
        unfold_variance_top(ty, ctx, descendants_are_canonical, shadowed);
    match &unfolded {
        crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => Ok(()),
        crate::ast::Type::Function { param, ret, .. } => {
            // LHS flips: pos ∘ Neg. RHS preserves: pos ∘ Pos = pos.
            let lhs_pos = pos.compose(Variance::Neg);
            check_newtype_payload_shadowed(
                param,
                lhs_pos,
                var_env,
                ctx,
                group,
                shadowed,
                descendants_are_canonical,
            )?;
            check_newtype_payload_shadowed(
                ret,
                pos,
                var_env,
                ctx,
                group,
                shadowed,
                descendants_are_canonical,
            )
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            // & and | preserve variance.
            check_newtype_payload_shadowed(
                left,
                pos,
                var_env,
                ctx,
                group,
                shadowed,
                descendants_are_canonical,
            )?;
            check_newtype_payload_shadowed(
                right,
                pos,
                var_env,
                ctx,
                group,
                shadowed,
                descendants_are_canonical,
            )
        }
        crate::ast::Type::Path {
            segments,
            args,
            meta: crate::ast::Meta { span, .. },
            ..
        } => {
            // Recursive use site: a bare local path or a canonical
            // same-module path whose leaf is a member of the newtype's
            // SCC (its own name or a sibling nominal that mutually
            // depends on it). Apply strict positivity — the position
            // composed from the payload root
            // must be `+` or `0`. The variance env *additionally*
            // composes through parametric newtype slots via fixpoint,
            // so transitive contravariance through chains is reflected
            // in `pos`; the SCC check catches the nominal-recursion
            // cycles the per-occurrence variance walk alone cannot see.
            let local_name = same_module_newtype_name(segments, ctx);
            let is_scc_member = local_name.is_some_and(|name| {
                (segments.len() > 1 || !shadowed.contains(name)) && group.members.contains(name)
            });
            if is_scc_member {
                if !pos.is_strictly_positive() {
                    let occurrence = local_name.expect("SCC member has a local name");
                    let detail = match pos {
                        Variance::Neg => "it occurs under contravariant composition",
                        Variance::Inv => {
                            "it occurs under invariant composition (e.g., under a \
                             host type's type-argument or a parameter that appears \
                             in both covariant and contravariant positions)"
                        }
                        Variance::Pos | Variance::Unused => "",
                    };
                    let subject = if occurrence == group.current {
                        format!("`{}`", group.current)
                    } else {
                        format!(
                            "`{occurrence}` (in the same recursive group as `{}`)",
                            group.current
                        )
                    };
                    return Err(Error::totality(
                        *span,
                        format!("{subject} is not in a strictly positive position; {detail}"),
                    ));
                }
                if local_name == Some(group.current)
                    && let Some(target) = ctx.newtypes.get(group.current)
                    && target.type_params.len() != args.len()
                {
                    return Err(Error::type_(
                        *span,
                        format!(
                            "`{}` arity mismatch: newtype takes {} type \
                             argument(s), {} given",
                            group.current,
                            target.type_params.len(),
                            args.len(),
                        ),
                    ));
                }
            }
            // Same-module and imported newtype heads both carry declared
            // slot variance. Imported lookup uses exact `(module, name)`
            // identity recovered by the resolver; host types and unknown
            // heads remain invariant.
            let local_target =
                local_name.filter(|name| segments.len() > 1 || !shadowed.contains(*name));
            for (i, a) in args.iter().enumerate() {
                let slot_v = local_target
                    .and_then(|name| var_env.local.get(name))
                    .and_then(|vs| vs.get(i).copied())
                    .or_else(|| imported_slot_variance(segments, i, ctx, &mut var_env.imported))
                    .unwrap_or(Variance::Inv);
                let arg_pos = pos.compose(slot_v);
                check_newtype_payload_shadowed(
                    a,
                    arg_pos,
                    var_env,
                    ctx,
                    group,
                    shadowed,
                    descendants_are_canonical,
                )?;
            }
            Ok(())
        }
        crate::ast::Type::Forall { param, body, .. } => {
            // Quantifiers are erased at evaluation; positivity is a
            // structural property of the body. A binder that collides
            // with an SCC member's name shadows it: occurrences of that
            // name in the body refer to the binder, not the newtype.
            let mut inner = shadowed.clone();
            inner.insert(param.name.clone());
            check_newtype_payload_shadowed(
                body,
                pos,
                var_env,
                ctx,
                group,
                &inner,
                descendants_are_canonical,
            )
        }
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        crate::ast::Type::Infer { .. } => unreachable!(
            "Type::Infer should be resolved by the typer's substitution pass; check_well_formed_type catches it at the boundary"
        ),
        crate::ast::Type::Goal { .. } => {
            unreachable!("an internal Type::Goal cannot occur in a newtype declaration payload")
        }
    }
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::Lowered;
    use std::path::{Path, PathBuf};

    fn package(sources: &[(&str, &str)]) -> crate::pass::resolve::Package<Lowered> {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                let module = crate::pass::parser::parse(source).expect("parse");
                let module = crate::pass::desugar::desugar_module(module).expect("desugar");
                (PathBuf::from(path), module)
            })
            .collect::<Vec<_>>();
        let (lowered, _) =
            crate::pass::label_elab::elaborate_package(parsed, None).expect("label elaboration");
        let package =
            crate::pass::resolve::Package::build(Path::new(""), lowered, None).expect("package");
        package.resolve_imports().expect("resolve imports");
        package
    }

    #[test]
    fn imported_contravariant_alias_keeps_recursive_newtype_identity() {
        let package = package(&[
            ("dep.kio", "module dep; pub type Contra[A] = A -> .;"),
            (
                "consumer.kio",
                "module consumer; import dep as dep; \
                 newtype Link : dep.Contra(Loop) { \
                   pub constructor mk_link; pub projector un_link; \
                 }; \
                 newtype Loop : Link { \
                   pub constructor mk_loop; pub projector un_loop; \
                 };",
            ),
        ]);
        let consumer = &package.module("consumer").expect("consumer").module;
        let env = super::super::ModuleEnv::build(consumer, None, None, Some(&package))
            .expect("consumer environment");
        let ctx = env.payload_ctx();
        let mut var_env = compute_variance_env(&ctx);
        let sccs = compute_newtype_sccs(&ctx);
        let link_def = ctx.newtypes.get("Link").expect("Link newtype");
        let link_scc = sccs.get("Link").expect("Link SCC");
        assert!(
            link_scc.contains("Loop"),
            "the canonical alias argument must contribute the Link -> Loop SCC edge"
        );

        let err = check_newtype_payload(
            &link_def.payload,
            Variance::Pos,
            &mut var_env,
            &ctx,
            "Link",
            link_scc,
        )
        .expect_err("the alias puts the recursive Loop member in a contravariant position");
        match err {
            Error::Totality(diagnostic) => {
                assert!(
                    diagnostic.message.contains("strictly positive"),
                    "got: {}",
                    diagnostic.message
                );
            }
            other => panic!("expected totality error, got {other:?}"),
        }
    }

    #[test]
    fn applied_higher_kinded_binder_does_not_resolve_to_same_named_newtype() {
        let package = package(&[(
            "consumer.kio",
            "module consumer; \
             newtype F[A] : A { pub constructor mk_f; pub projector un_f; }; \
             newtype Wrapper[*F][A] : F(A) { \
               pub constructor mk_wrapper; pub projector un_wrapper; \
             };",
        )]);
        let consumer = &package.module("consumer").expect("consumer").module;
        let env = super::super::ModuleEnv::build(consumer, None, None, Some(&package))
            .expect("consumer environment");
        let variances = compute_variance_env(&env.payload_ctx());

        assert_eq!(
            variances.get("Wrapper").expect("Wrapper variances")[1],
            Variance::Inv,
            "the abstract `F` binder, not the same-named nominal, owns its argument slot"
        );
    }

    #[test]
    fn imported_newtype_slot_variance_controls_recursive_uses() {
        let package = package(&[
            (
                "positive.kio",
                "module positive; \
                 pub newtype Wrap[A] : A { constructor mk_wrap; projector un_wrap; };",
            ),
            (
                "nonpositive.kio",
                "module nonpositive; \
                 pub newtype Wrap[A] : A -> . { constructor mk_wrap; projector un_wrap; }; \
                 pub newtype Invariant[A] : A & (A -> .) { \
                   constructor mk_invariant; projector un_invariant; \
                 };",
            ),
            (
                "consumer.kio",
                "module consumer; import positive as positive; import nonpositive as nonpositive; \
                 newtype Good : positive.Wrap(Good) { constructor mk_good; projector un_good; }; \
                 newtype Bad_contra : nonpositive.Wrap(Bad_contra) { \
                   constructor mk_bad_contra; projector un_bad_contra; \
                 }; \
                 newtype Bad_invariant : nonpositive.Invariant(Bad_invariant) { \
                   constructor mk_bad_invariant; projector un_bad_invariant; \
                 };",
            ),
        ]);
        let consumer = &package.module("consumer").expect("consumer").module;
        let env = super::super::ModuleEnv::build(consumer, None, None, Some(&package))
            .expect("consumer environment");
        let ctx = env.payload_ctx();
        let mut variances = compute_variance_env(&ctx);
        let sccs = compute_newtype_sccs(&ctx);

        let mut check = |name: &str| {
            let newtype = ctx.newtypes.get(name).expect("consumer newtype");
            check_newtype_payload(
                &newtype.payload,
                Variance::Pos,
                &mut variances,
                &ctx,
                name,
                sccs.get(name).expect("newtype SCC"),
            )
        };

        check("Good").expect("an imported covariant slot preserves strict positivity");
        for name in ["Bad_contra", "Bad_invariant"] {
            let err = check(name).expect_err("a non-positive imported slot must be rejected");
            assert!(
                matches!(err, Error::Totality(_)),
                "expected totality error for {name}, got {err:?}"
            );
        }
        assert_eq!(variances.imported.derivations["positive"], 1);
        assert_eq!(variances.imported.derivations["nonpositive"], 1);
    }

    #[test]
    fn imported_variance_memo_avoids_branching_provider_rederivations() {
        let package = package(&[
            (
                "p0.kio",
                "module p0; \
                 pub newtype Wrap[A] : A -> . { constructor mk_wrap; projector un_wrap; };",
            ),
            (
                "p1.kio",
                "module p1; import p0 as previous; \
                 pub newtype Wrap[A] : previous.Wrap(A) & previous.Wrap(A) { \
                   constructor mk_wrap; projector un_wrap; \
                 };",
            ),
            (
                "p2.kio",
                "module p2; import p1 as previous; \
                 pub newtype Wrap[A] : previous.Wrap(A) & previous.Wrap(A) { \
                   constructor mk_wrap; projector un_wrap; \
                 };",
            ),
            (
                "p3.kio",
                "module p3; import p2 as previous; \
                 pub newtype Wrap[A] : previous.Wrap(A) & previous.Wrap(A) { \
                   constructor mk_wrap; projector un_wrap; \
                 };",
            ),
            (
                "p4.kio",
                "module p4; import p3 as previous; \
                 pub newtype Wrap[A] : previous.Wrap(A) & previous.Wrap(A) { \
                   constructor mk_wrap; projector un_wrap; \
                 };",
            ),
            (
                "consumer.kio",
                "module consumer; import p4 as outer; \
                 newtype Good : outer.Wrap(outer.Wrap(Good)) { \
                   constructor mk_good; projector un_good; \
                 };",
            ),
        ]);
        let consumer = &package.module("consumer").expect("consumer").module;
        let env = super::super::ModuleEnv::build(consumer, None, None, Some(&package))
            .expect("consumer environment");
        let ctx = env.payload_ctx();
        let sccs = compute_newtype_sccs(&ctx);
        let good = ctx.newtypes.get("Good").expect("Good newtype");

        let check = |variances: &mut VarianceEnv| {
            check_newtype_payload(
                &good.payload,
                Variance::Pos,
                variances,
                &ctx,
                "Good",
                sccs.get("Good").expect("Good SCC"),
            )
            .expect("two imported contravariant slots compose to a positive position");
        };

        let mut variances = compute_variance_env(&ctx);
        check(&mut variances);

        for module in ["p0", "p1", "p2", "p3", "p4"] {
            assert_eq!(
                variances.imported.derivations[module], 1,
                "provider `{module}` should be derived once"
            );
            assert!(matches!(
                variances.imported.modules.get(module),
                Some(ImportedVarianceState::Ready(_))
            ));
        }

        let mut without_ready_reuse = compute_variance_env_without_imported_ready_reuse(&ctx);
        check(&mut without_ready_reuse);
        assert_eq!(
            without_ready_reuse.imported.derivations,
            HashMap::from([
                ("p0".to_owned(), 512),
                ("p1".to_owned(), 128),
                ("p2".to_owned(), 32),
                ("p3".to_owned(), 8),
                ("p4".to_owned(), 2),
            ]),
            "the frozen no-ready-reuse comparator should expose the branching derivation work"
        );
        assert_eq!(
            variances.imported.derivations.values().sum::<usize>(),
            5,
            "the memo should derive each exact provider once"
        );
        assert_eq!(
            without_ready_reuse
                .imported
                .derivations
                .values()
                .sum::<usize>(),
            682,
            "the comparator should perform the former repeated provider derivations"
        );
    }
}
