//! Module-level type-checking entry points: per-module walks and
//! per-fn checks.
//!
//! Every binary (`kio`, `kio-prime`) drives type-checking through the scoped
//! package/module entry points below. Both phases share the same surface
//! here — Lowered's `typecheck_full` and Prime's `prime::typer`
//! differ only at the dispatch layer ([`super::Typer`] impl). The
//! [`check_fn_def`] / [`check_equiv`] helpers are the per-item
//! building blocks the entry point calls.
//!
//! Extracted from [`super::typecheck_core`] for navigability;
//! depends on the rest of the umbrella for the synth / apply /
//! kind-check machinery.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::ast::FnPurityExt;
use crate::error::{Error, Fix, FixEdit};
use crate::pass::resolve::{
    LocatedError, NominalProvider, NominalSelection, Package, intersect_visibility,
};
#[cfg(feature = "parallel")]
use rayon::prelude::*;

use super::{
    ModuleEnv, NewtypeSccs, PackageTypecheckScope, PayloadCtx, TypeCtx, Typer, TyperPhase,
    Variance, VarianceEnv, check_newtype_payload, check_no_infer, check_well_formed_type,
    compute_newtype_sccs, compute_variance_env,
};

// =========================================================================
// Per-module check
// =========================================================================

/// Type-check a single module with full package context. The
/// package supplies cross-module value imports — `import pkg/helper(foo);`
/// resolves `foo` to its source-module `fn` so the
/// typer can pull the scheme over.
///
/// `fn` bodies route through `check_fn_def` (which dispatches via
/// `Typer<P>::check_value_against`); newtype payloads route
/// through `check_newtype_payload`. Both are
/// phase-polymorphic.
#[cfg(feature = "parallel")]
pub(crate) fn check_module_in_package_with_typecheck_scope_collect_errors<'m, P>(
    module: &'m crate::ast::Module<P>,
    package_file: Option<&'m crate::ast::PackageFile<P>>,
    package_name: Option<&str>,
    package: Option<&'m crate::pass::resolve::Package<P>>,
    elaborations: &mut P::Elaborations,
    typecheck_scope: std::sync::Arc<PackageTypecheckScope<P>>,
) -> Result<(), Vec<Error>>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
    crate::ast::FnDef<P>: Sync,
    ModuleEnv<'m, P>: Sync,
    P::Elaborations: Send,
{
    check_module_in_package_with_typecheck_scope_collect_errors_with_execution(
        module,
        package_file,
        package_name,
        package,
        elaborations,
        typecheck_scope,
        TypecheckExecution::AllowParallel,
    )
}

#[cfg(feature = "parallel")]
fn check_module_in_package_with_typecheck_scope_collect_errors_with_execution<'m, P>(
    module: &'m crate::ast::Module<P>,
    package_file: Option<&'m crate::ast::PackageFile<P>>,
    package_name: Option<&str>,
    package: Option<&'m crate::pass::resolve::Package<P>>,
    elaborations: &mut P::Elaborations,
    typecheck_scope: std::sync::Arc<PackageTypecheckScope<P>>,
    execution: TypecheckExecution,
) -> Result<(), Vec<Error>>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
    crate::ast::FnDef<P>: Sync,
    ModuleEnv<'m, P>: Sync,
    P::Elaborations: Send,
{
    let env = ModuleEnv::build_with_typecheck_scope_and_execution(
        module,
        package_file,
        package_name,
        package,
        typecheck_scope,
        execution,
    )
    .map_err(|error| vec![error])?;
    let payload_ctx = env.payload_ctx();
    let mut var_env = compute_variance_env(&payload_ctx);
    let sccs = compute_newtype_sccs(&payload_ctx);
    let fn_defs: Vec<(usize, &crate::ast::FnDef<P>)> = module
        .items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| match item {
            crate::ast::Item::FnDef(d) => Some((idx, d)),
            _ => None,
        })
        .collect();
    let check_fn = |(idx, d): &(usize, &crate::ast::FnDef<P>)| {
        let mut local_elaborations = P::Elaborations::default();
        (
            *idx,
            check_fn_def(d, *idx, &env, &mut local_elaborations).map(|()| local_elaborations),
        )
    };
    let fn_results: Vec<(usize, Result<P::Elaborations, Error>)> =
        if execution.parallelizes(fn_defs.len()) {
            crate::maybe_par_iter!(fn_defs).map(check_fn).collect()
        } else {
            fn_defs.iter().map(check_fn).collect()
        };
    finish_checking_module_collect_errors(
        module,
        &env,
        &payload_ctx,
        &mut var_env,
        &sccs,
        elaborations,
        fn_results,
    )
}

#[cfg(not(feature = "parallel"))]
pub(crate) fn check_module_in_package_with_typecheck_scope_collect_errors<'m, P>(
    module: &'m crate::ast::Module<P>,
    package_file: Option<&'m crate::ast::PackageFile<P>>,
    package_name: Option<&str>,
    package: Option<&'m crate::pass::resolve::Package<P>>,
    elaborations: &mut P::Elaborations,
    typecheck_scope: std::sync::Arc<PackageTypecheckScope<P>>,
) -> Result<(), Vec<Error>>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    check_module_in_package_with_typecheck_scope_collect_errors_with_execution(
        module,
        package_file,
        package_name,
        package,
        elaborations,
        typecheck_scope,
        TypecheckExecution::AllowParallel,
    )
}

#[cfg(not(feature = "parallel"))]
fn check_module_in_package_with_typecheck_scope_collect_errors_with_execution<'m, P>(
    module: &'m crate::ast::Module<P>,
    package_file: Option<&'m crate::ast::PackageFile<P>>,
    package_name: Option<&str>,
    package: Option<&'m crate::pass::resolve::Package<P>>,
    elaborations: &mut P::Elaborations,
    typecheck_scope: std::sync::Arc<PackageTypecheckScope<P>>,
    execution: TypecheckExecution,
) -> Result<(), Vec<Error>>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    let env = ModuleEnv::build_with_typecheck_scope_and_execution(
        module,
        package_file,
        package_name,
        package,
        typecheck_scope,
        execution,
    )
    .map_err(|error| vec![error])?;
    let payload_ctx = env.payload_ctx();
    let mut var_env = compute_variance_env(&payload_ctx);
    let sccs = compute_newtype_sccs(&payload_ctx);
    let fn_results: Vec<(usize, Result<P::Elaborations, Error>)> = module
        .items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| match item {
            crate::ast::Item::FnDef(d) => Some((idx, d)),
            _ => None,
        })
        .map(|(idx, d)| {
            let mut local_elaborations = P::Elaborations::default();
            (
                idx,
                check_fn_def(d, idx, &env, &mut local_elaborations).map(|()| local_elaborations),
            )
        })
        .collect();
    finish_checking_module_collect_errors(
        module,
        &env,
        &payload_ctx,
        &mut var_env,
        &sccs,
        elaborations,
        fn_results,
    )
}

fn first_collected_error(errors: Vec<Error>) -> Error {
    errors
        .into_iter()
        .next()
        .expect("collected module errors are non-empty")
}

/// Validate every declaration and function signature in a resolved package
/// without inspecting function bodies.
pub fn check_package_signatures<P>(package: &Package<P>) -> Result<(), LocatedError>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    let Some((_, first)) = package.modules().next() else {
        return Ok(());
    };
    let typecheck_scope = PackageTypecheckScope::fresh_for_module(
        &first.module,
        Some(package),
        std::sync::Arc::new(super::TypeInterner::default()),
    );
    for (_, entry) in package.modules() {
        check_module_signatures_with_scope(package, entry, typecheck_scope.clone())?;
    }
    Ok(())
}

/// Validate one module's declarations and function signatures with the full
/// package available for imported type resolution, without inspecting any
/// function body.
#[cfg(any(feature = "cli", all(test, feature = "prime")))]
pub(crate) fn check_module_signatures<P>(
    package: &Package<P>,
    entry: &crate::pass::resolve::ModuleEntry<P>,
) -> Result<(), LocatedError>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    let typecheck_scope = PackageTypecheckScope::fresh_for_module(
        &entry.module,
        Some(package),
        std::sync::Arc::new(super::TypeInterner::default()),
    );
    check_module_signatures_with_scope(package, entry, typecheck_scope)
}

fn check_module_signatures_with_scope<P>(
    package: &Package<P>,
    entry: &crate::pass::resolve::ModuleEntry<P>,
    typecheck_scope: std::sync::Arc<PackageTypecheckScope<P>>,
) -> Result<(), LocatedError>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    let package_file = package.package_file().map(|entry| &entry.package_file);
    let package_name = package
        .package_file()
        .map(|entry| entry.package_name.as_str());
    let mut elaborations = P::Elaborations::default();
    let env = ModuleEnv::build_with_typecheck_scope_and_execution(
        &entry.module,
        package_file,
        package_name,
        Some(package),
        typecheck_scope,
        TypecheckExecution::AllowParallel,
    )
    .map_err(|error| LocatedError::new(entry.file_path.clone(), error))?;
    let payload_ctx = env.payload_ctx();
    let mut var_env = compute_variance_env(&payload_ctx);
    let sccs = compute_newtype_sccs(&payload_ctx);
    let fn_results = entry
        .module
        .items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let crate::ast::Item::FnDef(def) = item else {
                return None;
            };
            let mut local_elaborations = P::Elaborations::default();
            Some((
                idx,
                check_fn_signature(def, &env, &mut local_elaborations).map(|()| local_elaborations),
            ))
        })
        .collect();
    finish_checking_module_collect_errors(
        &entry.module,
        &env,
        &payload_ctx,
        &mut var_env,
        &sccs,
        &mut elaborations,
        fn_results,
    )
    .map_err(|errors| LocatedError::new(entry.file_path.clone(), first_collected_error(errors)))
}

/// Reject transparent-alias cycles before any kind or normalization walk can
/// recurse through them. Ordinary source resolution catches the same defect
/// from source-ordered bare heads; this package-level form is also required
/// for identity-qualified fresh artifacts, whose heads no longer depend on
/// declaration order.
pub(crate) fn check_no_alias_only_cycles<P>(package: &Package<P>) -> Result<(), LocatedError>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    struct AliasNode<'a, P: crate::ast::Phase> {
        module_path: String,
        file_path: &'a std::path::Path,
        alias: &'a crate::ast::TypeAlias<P>,
        module: &'a crate::ast::Module<P>,
    }

    let mut nodes = Vec::new();
    for (module_path, entry) in package.modules() {
        for item in &entry.module.items {
            match item {
                crate::ast::Item::TypeAlias(alias) => nodes.push(AliasNode {
                    module_path: (*module_path).to_owned(),
                    file_path: &entry.file_path,
                    alias,
                    module: &entry.module,
                }),
                crate::ast::Item::TypeRecGroup(group) => {
                    for member in &group.members {
                        if let crate::ast::TypeRecMember::TypeAlias(alias) = member {
                            nodes.push(AliasNode {
                                module_path: (*module_path).to_owned(),
                                file_path: &entry.file_path,
                                alias,
                                module: &entry.module,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let indices = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| ((node.module_path.clone(), node.alias.name.clone()), index))
        .collect::<BTreeMap<_, _>>();
    let mut edge_spans = vec![Vec::new(); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        let provider = NominalProvider::new(Some(node.module), Some(package));
        let scope = provider.root();
        let mut bound = node
            .alias
            .type_params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        collect_alias_edges(
            &node.alias.body,
            &mut bound,
            &provider,
            scope,
            &indices,
            &mut edge_spans[index],
        );
        edge_spans[index].sort_unstable_by_key(|(target, span)| (*target, span.start, span.end));
        edge_spans[index].dedup_by_key(|(target, _)| *target);
    }
    let edges = edge_spans
        .iter()
        .map(|edges| edges.iter().map(|(target, _)| *target).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let Some(component) = crate::pass::resolve::strongly_connected_components(&edges, |_| true)
        .into_iter()
        .find(|component| component.len() > 1 || edges[component[0]].contains(&component[0]))
    else {
        return Ok(());
    };
    let first = component[0];
    let primary = edge_spans[first]
        .iter()
        .find(|(target, _)| component.contains(target))
        .map_or(nodes[first].alias.meta.span, |(_, span)| *span);
    let mut error = Error::totality(
        primary,
        "recursive type component has no `newtype` boundary",
    );
    for &index in &component {
        error = error.with_secondary(
            nodes[index].alias.name_span,
            format!(
                "transparent alias `{}.{}` participates in this cycle",
                nodes[index].module_path, nodes[index].alias.name
            ),
        );
    }
    Err(LocatedError {
        file_path: nodes[first].file_path.to_path_buf(),
        error: error
            .with_help("every recursive type cycle must cross a nominal `newtype` boundary"),
    })
}

fn collect_alias_edges<'m, P>(
    ty: &'m crate::ast::Type<P>,
    bound: &mut Vec<&'m str>,
    provider: &NominalProvider<'m, P>,
    scope: crate::pass::resolve::NominalScope<'m, P>,
    indices: &BTreeMap<(String, String), usize>,
    out: &mut Vec<(usize, crate::span::Span)>,
) where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    match ty {
        crate::ast::Type::Path {
            segments,
            args,
            meta,
        } => {
            if !matches!(segments.as_slice(), [head] if bound.iter().rev().any(|name| *name == head.as_str()))
            {
                let (qualified, canonical) = provider.qualify(scope, segments, false);
                if let NominalSelection::Selected(selected) =
                    provider.select(scope, &qualified, canonical)
                    && let Some(alias) = selected.declaration.type_alias()
                    && let Some(owner) = selected.owner.module()
                {
                    let module_path = owner
                        .path
                        .segments
                        .iter()
                        .map(crate::ast::PathSegment::as_str)
                        .collect::<Vec<_>>()
                        .join("/");
                    if let Some(target) = indices.get(&(module_path, alias.name.clone())) {
                        out.push((*target, meta.span));
                    }
                }
            }
            for argument in args {
                collect_alias_edges(argument, bound, provider, scope, indices, out);
            }
        }
        crate::ast::Type::Function { param, ret, .. } => {
            collect_alias_edges(param, bound, provider, scope, indices, out);
            collect_alias_edges(ret, bound, provider, scope, indices, out);
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            collect_alias_edges(left, bound, provider, scope, indices, out);
            collect_alias_edges(right, bound, provider, scope, indices, out);
        }
        crate::ast::Type::Forall { param, body, .. } => {
            bound.push(param.name.as_str());
            collect_alias_edges(body, bound, provider, scope, indices, out);
            bound.pop();
        }
        crate::ast::Type::Goal { args, .. } => {
            for argument in args {
                collect_alias_edges(argument, bound, provider, scope, indices, out);
            }
        }
        crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => {}
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        crate::ast::Type::Infer { .. } => {}
    }
}

fn finish_checking_module_collect_errors<'m, P>(
    module: &'m crate::ast::Module<P>,
    env: &ModuleEnv<'m, P>,
    payload_ctx: &PayloadCtx<'_, 'm, P>,
    var_env: &mut VarianceEnv,
    sccs: &NewtypeSccs,
    elaborations: &mut P::Elaborations,
    fn_results: Vec<(usize, Result<P::Elaborations, Error>)>,
) -> Result<(), Vec<Error>>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    // The per-newtype variance table and the newtype SCC partition are
    // computed once, up front. The strict-positivity check on each
    // newtype's payload composes through the variance table when
    // descending into nested newtype applications (transitive
    // contravariance through parametric chains) and ranges over the
    // newtype's SCC (so a sibling nominal occurring left-of-arrow in a
    // mutual/wrapper-indirect cycle is rejected, not treated as opaque).
    let mut first_non_fn_error = None;
    for (idx, item) in module.items.iter().enumerate() {
        let result = (|| -> Result<(), Error> {
            match item {
                crate::ast::Item::FnDef(_) => Ok(()),
                // Type-alias type-param scoping is already enforced by
                // the resolver, but the body still needs the well-
                // formedness walk: it rejects stray `Type::Infer`
                // placeholders (type-alias bodies are public-contract
                // positions, where `_` is not allowed) and any other
                // ill-formed type that the parser admits but the
                // typer doesn't accept. Every `Item::TypeAlias` reaching the
                // typer is a type.
                crate::ast::Item::TypeAlias(alias) => {
                    check_type_alias_declaration(alias, env, elaborations)
                }
                crate::ast::Item::Newtype(newtype) => check_newtype_declaration(
                    newtype,
                    false,
                    env,
                    payload_ctx,
                    var_env,
                    sccs,
                    elaborations,
                ),
                crate::ast::Item::TypeRecGroup(group) => {
                    for member in &group.members {
                        match member {
                            crate::ast::TypeRecMember::TypeAlias(alias) => {
                                check_type_alias_declaration(alias, env, elaborations)?;
                            }
                            crate::ast::TypeRecMember::Newtype(newtype) => {
                                check_newtype_declaration(
                                    newtype,
                                    true,
                                    env,
                                    payload_ctx,
                                    var_env,
                                    sccs,
                                    elaborations,
                                )?;
                            }
                            crate::ast::TypeRecMember::Labels(_, ext) => match *ext {},
                        }
                    }
                    if let Some(deferred) = &group.deferred_rec_labels_diagnostic {
                        Err(missing_recursive_labels_marker_error(deferred))
                    } else {
                        Ok(())
                    }
                }
                // Host items are declaration-only (no body). Their
                // signature types are public-contract positions, so run
                // the well-formedness + kind-check walks; there is no
                // expression to synthesize.
                crate::ast::Item::HostType(h) => check_host_type_parameters(h),
                crate::ast::Item::HostFn(h) => {
                    let provider = NominalProvider::new(Some(env.module), env.package);
                    let mut tcx = TypeCtx::new(env, elaborations);
                    let mut bound = HashSet::new();
                    for p in &h.params {
                        match p {
                            crate::ast::HostFnParam::Type(tp) => {
                                bound.insert(tp.name.as_str());
                                tcx.push_type_param_kinded(
                                    tp.name.as_str(),
                                    tp.effective_kind(),
                                    tp.span,
                                );
                                <P::Typer as Typer<P>>::record_binder_decl_at(
                                    tp.span,
                                    super::ResolvedBinderKind::TypeParam,
                                    tp.name.as_str(),
                                    &mut tcx,
                                );
                            }
                            crate::ast::HostFnParam::Value(v) => {
                                check_signature_type_visibility_with_provider(
                                    &v.ty,
                                    &bound,
                                    &crate::ast::Visibility::Public,
                                    &env.module.path,
                                    &format!("host function `{}`", h.name),
                                    env,
                                    &provider,
                                )?;
                                check_well_formed_type(&v.ty)?;
                                super::types::check_kinds_in_type_with_provider(
                                    &v.ty, &mut tcx, &provider,
                                )?;
                            }
                        }
                    }
                    check_signature_type_visibility_with_provider(
                        &h.ret,
                        &bound,
                        &crate::ast::Visibility::Public,
                        &env.module.path,
                        &format!("host function `{}`", h.name),
                        env,
                        &provider,
                    )?;
                    check_well_formed_type(&h.ret)?;
                    super::types::check_kinds_in_type_with_provider(&h.ret, &mut tcx, &provider)?;
                    Ok(())
                }
                // Statically uninhabited per the
                // `ItemLabels = Never` bound on `P`.
                crate::ast::Item::Labels(_, ext) => match *ext {},
                crate::ast::Item::LabelForward(_, ext) => match *ext {},
                // `equiv` validation: each arm is typechecked, then
                // all arms are required to share a type.
                crate::ast::Item::Equiv(e, _ext) => check_equiv(e, idx, env, elaborations),
                crate::ast::Item::Elaborator(s, _ext) => {
                    let provider = NominalProvider::new(Some(env.module), env.package);
                    check_signature_type_visibility_with_provider(
                        &s.call_ty,
                        &HashSet::new(),
                        &s.vis,
                        &env.module.path,
                        &format!("elaborator `{}`", s.name),
                        env,
                        &provider,
                    )?;
                    <P::Typer as Typer<P>>::check_user_elaborator_item(s, idx, env, elaborations)
                }
                // Statically uninhabited at every typer phase.
                crate::ast::Item::LiteralAlias(_, ext) => match *ext {},
                crate::ast::Item::Op(_, ext) => match *ext {},
                crate::ast::Item::VariadicOperator(_, ext) => match *ext {},
                crate::ast::Item::RecGroup(_, ext) => match *ext {},
            }
        })();
        if let Err(err) = result {
            first_non_fn_error = Some((idx, err));
            break;
        }
    }

    if let Some((_, err)) = first_non_fn_error {
        return Err(vec![err]);
    }

    let mut fn_errors = Vec::new();
    let mut fn_elaborations = Vec::new();
    for (idx, result) in fn_results {
        match result {
            Ok(local_elaborations) => {
                fn_elaborations.push(local_elaborations);
            }
            Err(err) => {
                let (span, _) = err.diag();
                fn_errors.push(((span.start, span.end, idx), err));
            }
        }
    }
    if !fn_errors.is_empty() {
        fn_errors.sort_by_key(|(key, _)| *key);
        return Err(fn_errors.into_iter().map(|(_, err)| err).collect());
    }
    for local_elaborations in fn_elaborations {
        P::merge_elaborations(elaborations, local_elaborations);
    }
    // Every fn body in the module is typed; the module's alias context
    // (`env`) is still live. Drain the deferred elaboration
    // queue now — off the per-fn-body critical path — producing each
    // structural elaborator's Kio' glue and gating it through the coherence
    // check. No-op at Prime (empty queue).
    <P::Typer as Typer<P>>::discharge_module_deferrals(env, elaborations)
        .map_err(|error| vec![error])?;
    Ok(())
}

fn missing_recursive_labels_marker_error(
    deferred: &crate::ast::DeferredRecLabelsDiagnostic,
) -> Error {
    Error::name_res(
        deferred.reference_span,
        "recursive data declaration requires `rec`",
    )
    .with_secondary(
        deferred.head_span,
        "this labels declaration does not enable its atomic recursive scope",
    )
    .with_help(if deferred.has_named_alias {
        "write `rec labels`; the named alias remains transparent and a generated label newtype grounds the recursive component"
    } else {
        "write `rec labels`; the generated label newtype grounds the recursive component"
    })
    .with_fix(
        Fix::machine_applicable(
            "Add `rec` to this recursive labels declaration",
            vec![FixEdit::new(
                crate::span::Span::new(
                    deferred.declaration_span.start,
                    deferred.declaration_span.start,
                ),
                "rec ",
            )],
        )
        .allowing_follow_on_reanalysis_outside(deferred.declaration_span),
    )
}

fn check_type_alias_declaration<'m, P>(
    alias: &'m crate::ast::TypeAlias<P>,
    env: &ModuleEnv<'m, P>,
    elaborations: &mut P::Elaborations,
) -> Result<(), Error>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    let provider = NominalProvider::new(Some(env.module), env.package);
    let bound = alias
        .type_params
        .iter()
        .map(|param| param.name.as_str())
        .collect();
    check_signature_type_visibility_with_provider(
        alias.type_body(),
        &bound,
        &alias.vis,
        &env.module.path,
        &format!("type alias `{}`", alias.name),
        env,
        &provider,
    )?;
    check_well_formed_type(alias.type_body())?;
    let mut tcx = TypeCtx::new(env, elaborations);
    for tp in &alias.type_params {
        tcx.push_type_param_kinded(tp.name.as_str(), tp.effective_kind(), tp.span);
        <P::Typer as Typer<P>>::record_binder_decl_at(
            tp.span,
            super::ResolvedBinderKind::TypeParam,
            tp.name.as_str(),
            &mut tcx,
        );
    }
    super::types::check_kind_tree_with_provider(alias.type_body(), &mut tcx, &provider).map(|_| ())
}

fn check_newtype_declaration<'m, P>(
    newtype: &'m crate::ast::Newtype<P>,
    recursive_group_member: bool,
    env: &ModuleEnv<'m, P>,
    payload_ctx: &PayloadCtx<'_, 'm, P>,
    var_env: &mut VarianceEnv,
    sccs: &NewtypeSccs,
    elaborations: &mut P::Elaborations,
) -> Result<(), Error>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    let provider = NominalProvider::new(Some(env.module), env.package);
    let bound = newtype
        .type_params
        .iter()
        .chain(&newtype.existential_params)
        .map(|param| param.name.as_str())
        .collect();
    check_newtype_member_visibility_with_provider(
        newtype,
        &newtype.constructor,
        "constructor",
        &bound,
        env,
        &provider,
    )?;
    check_newtype_member_visibility_with_provider(
        newtype,
        &newtype.projector,
        "projector",
        &bound,
        env,
        &provider,
    )?;
    check_no_infer(
        &newtype.payload,
        "`_` is not allowed here: top-level binding annotations must be \
         explicit. `_` placeholders are legal only in `fn` signatures and \
         call type-argument lists",
    )?;
    check_no_phantom_existentials(newtype)?;
    let scc = sccs
        .get(newtype.name.as_str())
        .expect("every newtype has an SCC entry");
    check_newtype_payload(
        &newtype.payload,
        Variance::Pos,
        var_env,
        payload_ctx,
        &newtype.name,
        scc,
    )?;
    let mut tcx = TypeCtx::new(env, elaborations);
    for tp in &newtype.type_params {
        tcx.push_type_param_kinded(tp.name.as_str(), tp.effective_kind(), tp.span);
        <P::Typer as Typer<P>>::record_binder_decl_at(
            tp.span,
            super::ResolvedBinderKind::TypeParam,
            tp.name.as_str(),
            &mut tcx,
        );
    }
    for tp in &newtype.existential_params {
        tcx.push_type_param_kinded(tp.name.as_str(), tp.effective_kind(), tp.span);
        <P::Typer as Typer<P>>::record_binder_decl_at(
            tp.span,
            super::ResolvedBinderKind::TypeParam,
            tp.name.as_str(),
            &mut tcx,
        );
    }
    super::types::check_kinds_in_type_with_provider(&newtype.payload, &mut tcx, &provider)?;
    if !recursive_group_member
        && let Some(error) =
            crate::pass::resolve::missing_recursive_newtype_marker_error(newtype, &env.module.path)
    {
        return Err(error);
    }
    Ok(())
}

#[derive(Debug)]
struct ResolvedSignatureType {
    owner: crate::ast::ModulePath,
    visibility: crate::ast::Visibility,
    description: String,
}

fn resolved_signature_type_with_provider<P>(
    segments: &[crate::ast::PathSegment],
    bound: &HashSet<&str>,
    _env: &ModuleEnv<'_, P>,
    provider: &NominalProvider<'_, P>,
) -> Option<ResolvedSignatureType>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    if matches!(segments, [name] if bound.contains(name.as_str())) {
        return None;
    }

    let (qualified, canonical) = provider.qualify(provider.root(), segments, false);
    let NominalSelection::Selected(selected) =
        provider.select(provider.root(), &qualified, canonical)
    else {
        return None;
    };
    let owner = selected.owner.module()?.path.clone();
    if let Some(alias) = selected.declaration.type_alias() {
        Some(ResolvedSignatureType {
            owner,
            visibility: alias.vis.clone(),
            description: format!("type alias `{}`", alias.name),
        })
    } else if let Some(newtype) = selected.declaration.newtype() {
        Some(ResolvedSignatureType {
            owner,
            visibility: newtype.vis.clone(),
            description: format!("newtype `{}`", newtype.name),
        })
    } else {
        selected
            .declaration
            .host_type()
            .map(|host| ResolvedSignatureType {
                owner,
                visibility: crate::ast::Visibility::Public,
                description: format!("host type `{}`", host.name),
            })
    }
}

fn require_signature_dependency_visibility(
    target: &ResolvedSignatureType,
    referrer_visibility: &crate::ast::Visibility,
    referrer_owner: &crate::ast::ModulePath,
    referrer: &str,
    span: crate::span::Span,
) -> Result<(), Error> {
    if crate::pass::resolve::visibility_covers(
        &target.visibility,
        &target.owner,
        referrer_visibility,
        referrer_owner,
    ) {
        return Ok(());
    }
    Err(Error::type_(
        span,
        format!(
            "{} must be at least as visible as {referrer}",
            target.description
        ),
    )
    .with_help(format!(
        "give {} visibility equal to or wider than {referrer}",
        target.description
    )))
}

#[cfg(all(test, feature = "surface"))]
fn check_signature_type_visibility<P>(
    ty: &crate::ast::Type<P>,
    bound: &HashSet<&str>,
    referrer_visibility: &crate::ast::Visibility,
    referrer_owner: &crate::ast::ModulePath,
    referrer: &str,
    env: &ModuleEnv<'_, P>,
) -> Result<(), Error>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    let provider = NominalProvider::new(Some(env.module), env.package);
    check_signature_type_visibility_with_provider(
        ty,
        bound,
        referrer_visibility,
        referrer_owner,
        referrer,
        env,
        &provider,
    )
}

fn check_signature_type_visibility_with_provider<P>(
    ty: &crate::ast::Type<P>,
    bound: &HashSet<&str>,
    referrer_visibility: &crate::ast::Visibility,
    referrer_owner: &crate::ast::ModulePath,
    referrer: &str,
    env: &ModuleEnv<'_, P>,
    provider: &NominalProvider<'_, P>,
) -> Result<(), Error>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    match ty {
        crate::ast::Type::Path {
            segments,
            args,
            meta,
        } => {
            if let Some(target) =
                resolved_signature_type_with_provider(segments, bound, env, provider)
            {
                require_signature_dependency_visibility(
                    &target,
                    referrer_visibility,
                    referrer_owner,
                    referrer,
                    meta.span,
                )?;
            }
            for arg in args {
                check_signature_type_visibility_with_provider(
                    arg,
                    bound,
                    referrer_visibility,
                    referrer_owner,
                    referrer,
                    env,
                    provider,
                )?;
            }
        }
        crate::ast::Type::Function { param, ret, .. } => {
            check_signature_type_visibility_with_provider(
                param,
                bound,
                referrer_visibility,
                referrer_owner,
                referrer,
                env,
                provider,
            )?;
            check_signature_type_visibility_with_provider(
                ret,
                bound,
                referrer_visibility,
                referrer_owner,
                referrer,
                env,
                provider,
            )?;
        }
        crate::ast::Type::Product { left, right, .. }
        | crate::ast::Type::Sum { left, right, .. } => {
            check_signature_type_visibility_with_provider(
                left,
                bound,
                referrer_visibility,
                referrer_owner,
                referrer,
                env,
                provider,
            )?;
            check_signature_type_visibility_with_provider(
                right,
                bound,
                referrer_visibility,
                referrer_owner,
                referrer,
                env,
                provider,
            )?;
        }
        crate::ast::Type::Forall { param, body, .. } => {
            let mut nested = bound.clone();
            nested.insert(param.name.as_str());
            check_signature_type_visibility_with_provider(
                body,
                &nested,
                referrer_visibility,
                referrer_owner,
                referrer,
                env,
                provider,
            )?;
        }
        crate::ast::Type::Unit { .. }
        | crate::ast::Type::Bottom { .. }
        | crate::ast::Type::Infer { .. } => {}
        crate::ast::Type::Goal { .. } => unreachable!(
            "an internal Type::Goal cannot occur in a declaration signature visibility check"
        ),
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
    }
    Ok(())
}

fn check_newtype_member_visibility_with_provider<P>(
    newtype: &crate::ast::Newtype<P>,
    member: &crate::ast::TypeMember<P>,
    member_kind: &str,
    bound: &HashSet<&str>,
    env: &ModuleEnv<'_, P>,
    provider: &NominalProvider<'_, P>,
) -> Result<(), Error>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    let referrer = format!("{member_kind} `{}.{}`", newtype.name, member.name);
    let effective_visibility = intersect_visibility(&newtype.vis, &member.vis);
    check_signature_type_visibility_with_provider(
        &newtype.payload,
        bound,
        &effective_visibility,
        &env.module.path,
        &referrer,
        env,
        provider,
    )
}

/// Enforce the host-type parameter contract shared by full Kio, Kio', and
/// frozen signature replay.
pub(crate) fn check_host_type_parameters<P: crate::ast::Phase>(
    host: &crate::ast::HostType<P>,
) -> Result<(), Error> {
    if let (Some(role), Some(first_param)) = (host.role, host.type_params.first()) {
        return Err(Error::type_(
            role.span,
            format!(
                "role-bearing host type `{}` cannot have type parameters",
                host.name
            ),
        )
        .with_secondary(
            first_param.span,
            "this type parameter makes the host type parameterized",
        )
        .with_help(format!(
            "remove the type parameters or remove `role({})`",
            role.role.as_str()
        ))
        .with_suggestion(role.span, ""));
    }

    if let Some(param) = host.type_params.iter().find(|param| param.kind.is_some()) {
        return Err(Error::type_(
            param.span,
            format!(
                "host type `{}` cannot have higher-kinded type parameter `{}`",
                host.name, param.name
            ),
        )
        .with_help(format!(
            "host type parameters must have kind `*` — write `[{}]`",
            param.name
        )));
    }

    Ok(())
}

struct ModuleCheckResult<E> {
    module_path: String,
    file_path: PathBuf,
    result: Result<E, Vec<Error>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypecheckExecution {
    AllowParallel,
    #[cfg(feature = "surface")]
    Sequential,
}

impl TypecheckExecution {
    #[cfg(any(feature = "surface", feature = "parallel"))]
    pub(crate) fn parallelizes(self, width: usize) -> bool {
        self == Self::AllowParallel && width > 1
    }
}

#[cfg(feature = "parallel")]
pub(crate) fn check_package_modules_with_typecheck_scope_collect_errors_with_execution<'m, P>(
    package: &'m Package<P>,
    elaborations: &mut P::Elaborations,
    typecheck_scope: std::sync::Arc<PackageTypecheckScope<P>>,
    execution: TypecheckExecution,
) -> Result<(), Vec<LocatedError>>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
    crate::ast::FnDef<P>: Sync,
    crate::ast::PackageFile<P>: Sync,
    crate::pass::resolve::ModuleEntry<P>: Sync,
    ModuleEnv<'m, P>: Sync,
    Package<P>: Sync,
    crate::ast::Type<P>: Send + Sync,
    P::Elaborations: Send,
{
    typecheck_scope.assert_package(package);
    let package_file = package.package_file().map(|e| &e.package_file);
    let package_name = package.package_file().map(|e| e.package_name.as_str());
    for level in package.value_import_topo_levels() {
        let mut results = if execution.parallelizes(level.len()) {
            crate::maybe_par_iter!(level)
                .map(|(module_path, entry)| {
                    check_one_module_in_package_with_typecheck_scope(
                        module_path,
                        entry,
                        package_file,
                        package_name,
                        Some(package),
                        typecheck_scope.clone(),
                        execution,
                    )
                })
                .collect::<Vec<_>>()
        } else {
            level
                .into_iter()
                .map(|(module_path, entry)| {
                    check_one_module_in_package_with_typecheck_scope(
                        module_path,
                        entry,
                        package_file,
                        package_name,
                        Some(package),
                        typecheck_scope.clone(),
                        execution,
                    )
                })
                .collect::<Vec<_>>()
        };
        results.sort_by(|a, b| a.module_path.cmp(&b.module_path));
        let errors = module_errors(&results);
        if !errors.is_empty() {
            return Err(errors);
        }
        for result in results {
            let local = result
                .result
                .expect("module errors returned before elaboration merge");
            P::merge_elaborations(elaborations, local);
        }
    }
    Ok(())
}

#[cfg(not(feature = "parallel"))]
pub(crate) fn check_package_modules_with_typecheck_scope_collect_errors_with_execution<P>(
    package: &Package<P>,
    elaborations: &mut P::Elaborations,
    typecheck_scope: std::sync::Arc<PackageTypecheckScope<P>>,
    execution: TypecheckExecution,
) -> Result<(), Vec<LocatedError>>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    typecheck_scope.assert_package(package);
    let package_file = package.package_file().map(|e| &e.package_file);
    let package_name = package.package_file().map(|e| e.package_name.as_str());
    for level in package.value_import_topo_levels() {
        let mut results = level
            .into_iter()
            .map(|(module_path, entry)| {
                check_one_module_in_package_with_typecheck_scope(
                    module_path,
                    entry,
                    package_file,
                    package_name,
                    Some(package),
                    typecheck_scope.clone(),
                    execution,
                )
            })
            .collect::<Vec<_>>();
        results.sort_by(|a, b| a.module_path.cmp(&b.module_path));
        let errors = module_errors(&results);
        if !errors.is_empty() {
            return Err(errors);
        }
        for result in results {
            let local = result
                .result
                .expect("module errors returned before elaboration merge");
            P::merge_elaborations(elaborations, local);
        }
    }
    Ok(())
}

#[cfg(feature = "parallel")]
fn check_one_module_in_package_with_typecheck_scope<'m, P>(
    module_path: &str,
    entry: &'m crate::pass::resolve::ModuleEntry<P>,
    package_file: Option<&'m crate::ast::PackageFile<P>>,
    package_name: Option<&str>,
    package: Option<&'m Package<P>>,
    typecheck_scope: std::sync::Arc<PackageTypecheckScope<P>>,
    execution: TypecheckExecution,
) -> ModuleCheckResult<P::Elaborations>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
    crate::ast::FnDef<P>: Sync,
    crate::pass::resolve::ModuleEntry<P>: Sync,
    ModuleEnv<'m, P>: Sync,
    P::Elaborations: Send,
{
    let mut local = P::Elaborations::default();
    let result = check_module_in_package_with_typecheck_scope_collect_errors_with_execution(
        &entry.module,
        package_file,
        package_name,
        package,
        &mut local,
        typecheck_scope,
        execution,
    )
    .map(|()| local);
    ModuleCheckResult {
        module_path: module_path.to_owned(),
        file_path: entry.file_path.clone(),
        result,
    }
}

#[cfg(not(feature = "parallel"))]
fn check_one_module_in_package_with_typecheck_scope<'m, P>(
    module_path: &str,
    entry: &'m crate::pass::resolve::ModuleEntry<P>,
    package_file: Option<&'m crate::ast::PackageFile<P>>,
    package_name: Option<&str>,
    package: Option<&'m Package<P>>,
    typecheck_scope: std::sync::Arc<PackageTypecheckScope<P>>,
    execution: TypecheckExecution,
) -> ModuleCheckResult<P::Elaborations>
where
    P: TyperPhase
        + crate::ast::Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemOp = crate::ast::Never,
        > + Clone,
{
    let mut local = P::Elaborations::default();
    let result = check_module_in_package_with_typecheck_scope_collect_errors_with_execution(
        &entry.module,
        package_file,
        package_name,
        package,
        &mut local,
        typecheck_scope,
        execution,
    )
    .map(|()| local);
    ModuleCheckResult {
        module_path: module_path.to_owned(),
        file_path: entry.file_path.clone(),
        result,
    }
}

fn module_errors<E>(results: &[ModuleCheckResult<E>]) -> Vec<LocatedError> {
    let mut errors: Vec<_> = results
        .iter()
        .flat_map(|result| {
            result.result.as_ref().err().into_iter().flat_map(|errors| {
                errors.iter().map(|err| {
                    let (span, _) = err.diag();
                    (
                        result.module_path.as_str(),
                        span.start,
                        span.end,
                        LocatedError::new(result.file_path.clone(), err.clone()),
                    )
                })
            })
        })
        .collect();
    errors.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    errors
        .into_iter()
        .map(|(_, _, _, located)| located)
        .collect()
}

/// Reject a `newtype` declaration whose existential binder run
/// includes a name that does not occur in the payload.
///
/// Each existential binder must appear at least once in the payload —
/// a binder that never occurs is information-free (the typer has
/// nothing to recover at constructor call sites and nothing to
/// reflect at projector call sites), so it adds nothing to the
/// type's meaning and is rejected at declaration time. See
/// `specs/language.md` § Existential type binders and
/// `specs/prime.md` § The newtype primitive.
fn check_no_phantom_existentials<P>(d: &crate::ast::Newtype<P>) -> Result<(), Error>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    if d.existential_params.is_empty() {
        return Ok(());
    }
    let mut payload_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    super::types::collect_free_type_vars(&d.payload, &mut payload_names);
    for binder in &d.existential_params {
        if !payload_names.contains(&binder.name) {
            return Err(Error::type_(
                binder.span,
                format!(
                    "phantom existential binder `<{}>`: the binder does not appear in \
                     the payload of `newtype {}`. Each existential binder must occur at \
                     least once in the payload — a phantom existential is \
                     information-free and rejected.",
                    binder.name, d.name,
                ),
            ));
        }
    }
    Ok(())
}

// =========================================================================
// Per-fn check
// =========================================================================

/// Type-check one `fn`: bind its parameters in source order, kind-
/// check the return type, then route the body through the
/// bidirectional `check_value_against` (via the [`Typer<P>`]
/// dispatch trait — Lowered handles its inhabited surface forms, while Prime
/// discharges those variants via `match *ext {}`).
///
/// Phase-polymorphic. This function records into the caller's phase-local
/// publication table; later Lowered substitution or Prime completion-baking
/// materializes the AST-directed entries before backend-facing Prime is
/// returned.
pub fn check_fn_def<'m, P>(
    d: &'m crate::ast::FnDef<P>,
    item_index: usize,
    env: &ModuleEnv<'m, P>,
    elaborations: &mut P::Elaborations,
) -> Result<(), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let mut tcx = TypeCtx::new(env, elaborations).at_item(item_index);
    check_fn_signature_with_ctx(d, &mut tcx)?;
    tcx.pure_context = d.purity.is_pure_fn();
    // The declaration's return type is the body's expected type.
    <P::Typer as Typer<P>>::check_value_against(&d.body, &d.ret, &mut tcx)
}

fn check_fn_signature<'m, P>(
    d: &'m crate::ast::FnDef<P>,
    env: &'m ModuleEnv<'m, P>,
    elaborations: &mut P::Elaborations,
) -> Result<(), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let mut tcx = TypeCtx::new(env, elaborations);
    check_fn_signature_with_ctx(d, &mut tcx)
}

fn check_fn_signature_with_ctx<'m, P>(
    d: &'m crate::ast::FnDef<P>,
    tcx: &mut TypeCtx<'m, '_, P>,
) -> Result<(), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    // Bind parameters in source order. Each value-param's annotation
    // is a type expression that may reference type-parameters
    // introduced earlier in the same parameter list.
    let mut bound = HashSet::new();
    let referrer = format!("function `{}`", d.name);
    let provider = NominalProvider::new(Some(tcx.env.module), tcx.env.package);
    for p in &d.sig.params {
        match p {
            crate::ast::SignatureParam::Type(tp) => {
                bound.insert(tp.name.as_str());
                tcx.push_type_param_kinded(&tp.name, tp.effective_kind(), tp.span);
                <P::Typer as Typer<P>>::record_binder_decl_at(
                    tp.span,
                    super::ResolvedBinderKind::TypeParam,
                    &tp.name,
                    tcx,
                );
            }
            crate::ast::SignatureParam::Value(vp) => {
                let ty = vp
                    .ty
                    .as_ref()
                    .expect("fn value-params carry mandatory type annotations");
                check_signature_type_visibility_with_provider(
                    ty,
                    &bound,
                    &d.vis,
                    &tcx.env.module.path,
                    &referrer,
                    tcx.env,
                    &provider,
                )?;
                check_well_formed_type(ty)?;
                super::types::check_kinds_in_type_with_provider(ty, tcx, &provider)?;
                tcx.push_value(vp.name.clone(), ty.clone());
                <P::Typer as Typer<P>>::record_binder_decl_at(
                    vp.meta.span,
                    super::ResolvedBinderKind::Local,
                    &vp.name,
                    tcx,
                );
            }
        }
    }
    check_signature_type_visibility_with_provider(
        &d.ret,
        &bound,
        &d.vis,
        &tcx.env.module.path,
        &referrer,
        tcx.env,
        &provider,
    )?;
    check_well_formed_type(&d.ret)?;
    super::types::check_kinds_in_type_with_provider(&d.ret, tcx, &provider)?;
    P::check_fn_capability_signature(d, tcx)
}

/// Type-check one `equiv`: bind its parameters in source order
/// (same scoping as `fn`), synth each `term` body, then require
/// all terms share a type. The bodies are independent expressions —
/// no `term` sees another's value, only the shared parameter scope.
///
/// Phase-polymorphic via the [`Typer<P>`] dispatch trait — body
/// synthesis routes through `<P::Typer as Typer<P>>::synth_expr`.
pub fn check_equiv<'m, P>(
    e: &'m crate::ast::Equiv<P>,
    item_index: usize,
    env: &'m ModuleEnv<'m, P>,
    elaborations: &mut P::Elaborations,
) -> Result<(), Error>
where
    P: TyperPhase + crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    let mut tcx = TypeCtx::new(env, elaborations).at_item(item_index);
    // Same parameter scoping as `check_fn_def`.
    for p in &e.sig.params {
        match p {
            crate::ast::SignatureParam::Type(tp) => {
                tcx.push_type_param_kinded(&tp.name, tp.effective_kind(), tp.span);
                <P::Typer as Typer<P>>::record_binder_decl_at(
                    tp.span,
                    super::ResolvedBinderKind::TypeParam,
                    &tp.name,
                    &mut tcx,
                );
            }
            crate::ast::SignatureParam::Value(vp) => {
                let ty = vp
                    .ty
                    .as_ref()
                    .expect("equiv value-params carry mandatory type annotations");
                check_well_formed_type(ty)?;
                super::types::check_kinds_in_type(ty, &mut tcx)?;
                tcx.push_value(vp.name.clone(), ty.clone());
                <P::Typer as Typer<P>>::record_binder_decl_at(
                    vp.meta.span,
                    super::ResolvedBinderKind::Local,
                    &vp.name,
                    &mut tcx,
                );
            }
        }
    }
    // Synth each `term` body and require they share a type. The
    // first term sets the expected type; later terms are required
    // to match via the existing definitional-equality routine.
    // Equiv arms are value positions. Monotypes and complete polymorphic
    // function schemes are therefore admitted; incomplete schemes retain the
    // shared value-boundary diagnostic.
    let mut iter = e.terms.iter();
    let first = iter
        .next()
        .expect("equiv has at least two terms (parser enforces N>=2)");
    let first_ty =
        super::synth::synth_value_type(<P::Typer as Typer<P>>::synth_expr(&first.body, &mut tcx)?)?;
    let binders = tcx.in_scope_type_param_binders();
    for term in iter {
        let term_ty = super::synth::synth_value_type(<P::Typer as Typer<P>>::synth_expr(
            &term.body, &mut tcx,
        )?)?;
        let alias_ctx = tcx.binder_alias_ctx(&binders);
        super::require_type_equiv_state(
            term_ty.as_type(),
            first_ty.as_type(),
            term.body.span(),
            &alias_ctx,
            term_ty.identity_is_canonical(),
            first_ty.identity_is_canonical(),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;

    #[cfg(feature = "prime")]
    fn prime_signature_package(sources: &[String]) -> Package<crate::ast::Prime> {
        let modules = sources
            .iter()
            .map(|source| {
                let parsed = crate::pass::parser::parse(source).expect("parse signature module");
                let module =
                    crate::prime::lower::lower_module(parsed).expect("lower signature module");
                let path = module
                    .path
                    .segments
                    .iter()
                    .map(|part| part.name.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                (PathBuf::from(format!("{path}.kio")), module)
            })
            .collect();
        let package = Package::build(std::path::Path::new("."), modules, None)
            .expect("build signature package");
        package
            .resolve_imports()
            .expect("resolve signature imports");
        package
            .check_in_body_resolution()
            .expect("resolve signature names");
        package
    }

    #[cfg(feature = "prime")]
    #[test]
    fn signature_package_builds_one_identity_alias_index_across_modules() {
        use crate::pass::resolve::{IdentityAliasIndexWork, take_identity_alias_index_work};

        for count in [64, 128] {
            let sources = (0..count)
                .map(|index| format!("module m{index}; pub type T = .;"))
                .collect::<Vec<_>>();
            let package = prime_signature_package(&sources);
            take_identity_alias_index_work();
            check_package_signatures(&package).expect("independent signatures");
            assert_eq!(
                take_identity_alias_index_work(),
                IdentityAliasIndexWork {
                    builds: 1,
                    items: count,
                }
            );
        }
    }

    #[cfg(feature = "prime")]
    #[test]
    fn signature_index_reuse_stays_within_one_package_check() {
        use crate::pass::resolve::{IdentityAliasIndexWork, take_identity_alias_index_work};

        let package = prime_signature_package(&[
            "module a; pub type T = .;".to_owned(),
            "module b; pub type T = !;".to_owned(),
        ]);
        for _ in 0..2 {
            take_identity_alias_index_work();
            check_package_signatures(&package).expect("fresh package invocation");
            assert_eq!(
                take_identity_alias_index_work(),
                IdentityAliasIndexWork {
                    builds: 1,
                    items: 2,
                }
            );
        }
        for (_, entry) in package.modules() {
            take_identity_alias_index_work();
            check_module_signatures(&package, entry).expect("fresh individual module check");
            assert_eq!(
                take_identity_alias_index_work(),
                IdentityAliasIndexWork {
                    builds: 1,
                    items: 2,
                }
            );
        }
        let empty = prime_signature_package(&[]);
        take_identity_alias_index_work();
        check_package_signatures(&empty).expect("empty package");
        assert_eq!(
            take_identity_alias_index_work(),
            IdentityAliasIndexWork::default()
        );
    }

    #[cfg(feature = "prime")]
    #[test]
    fn signature_package_scope_matches_fresh_module_checks() {
        let cases: &[(&[&str], bool)] = &[
            (
                &[
                    "module left; host type Token;",
                    "module right; host type Token;",
                    "module a; import left(Token); host fn take(value: Token) -> .;",
                    "module b; import right(Token); host fn take(value: Token) -> .;",
                ],
                true,
            ),
            (
                &[
                    "module left; host type Token; pub type Id[A] = A;",
                    "module a; import left as dep; pub newtype Wrapped : dep.Id(dep.Token) { pub constructor wrap; pub projector unwrap; };",
                    "module b; import a(Wrapped); pub type Copy = Wrapped;",
                ],
                true,
            ),
            (
                &[
                    "module a; pub type Higher[*F] = .;",
                    "module b; import a(Higher); pub type Bad = Higher(.);",
                ],
                false,
            ),
            (
                &[
                    "module a; pub type F[A] = A;",
                    "module b; import a(F); pub type Bad = F(., .);",
                ],
                false,
            ),
            (
                &[
                    "module left; pub type F[A] = A;",
                    "module right; pub type F[A, B] = B;",
                    "module a; import left(F); pub type T = F(.);",
                    "module b; import right(F); pub type T = F(., !);",
                ],
                true,
            ),
            (
                &[
                    "module left; pub type F[A] = A;",
                    "module right; pub type F[A, B] = B;",
                    "module a; import left(F); host fn take(value: F(.)) -> .;",
                    "module b; import right(F); host fn take(value: F(.)) -> .;",
                ],
                false,
            ),
        ];
        for (sources, accepted) in cases {
            let package = prime_signature_package(
                &sources
                    .iter()
                    .map(|source| (*source).to_owned())
                    .collect::<Vec<_>>(),
            );
            let shared = check_package_signatures(&package);
            let fresh = package
                .modules()
                .try_for_each(|(_, entry)| check_module_signatures(&package, entry));
            assert_eq!(
                fresh.is_ok(),
                *accepted,
                "sources: {sources:?}; fresh result: {fresh:?}"
            );
            assert_eq!(
                shared.map_err(|error| format!("{error:?}")),
                fresh.map_err(|error| format!("{error:?}"))
            );
        }
    }

    #[cfg(feature = "surface")]
    fn parse_module(src: &str) -> crate::ast::Module<crate::ast::Lowered> {
        let surface = crate::pass::parser::parse(src).expect("parse");
        let desugared = crate::pass::desugar::desugar_module(surface).expect("desugar");
        let (mut lowered, _) = crate::pass::label_elab::elaborate_package(
            vec![(PathBuf::from("test.kio"), desugared)],
            None,
        )
        .expect("label_elab");
        lowered.pop().expect("at least one module").1
    }

    #[cfg(feature = "surface")]
    fn check_named_signature(src: &str, name: &str) -> Result<(), Error> {
        let module = parse_module(src);
        let env = ModuleEnv::build(&module, None, None, None).expect("environment");
        let definition = module
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::FnDef(definition) if definition.name == name => Some(definition),
                _ => None,
            })
            .expect("named function");
        check_fn_signature(
            definition,
            &env,
            &mut crate::pass::typecheck_full::Elaborations::new(),
        )
    }

    fn module_error(path: &str, span: Span) -> ModuleCheckResult<()> {
        ModuleCheckResult {
            module_path: path.to_owned(),
            file_path: PathBuf::from(format!("{path}.kio")),
            result: Err(vec![Error::type_(span, path)]),
        }
    }

    #[test]
    fn module_errors_order_by_module_path_then_span() {
        let results = vec![
            module_error("pkg/z", Span::new(1, 2)),
            module_error("pkg/a", Span::new(5, 6)),
            module_error("pkg/a", Span::new(3, 4)),
        ];

        let errors = module_errors(&results);

        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].file_path, PathBuf::from("pkg/a.kio"));
        assert_eq!(errors[0].error.diag().0, Span::new(3, 4));
        assert_eq!(errors[1].file_path, PathBuf::from("pkg/a.kio"));
        assert_eq!(errors[1].error.diag().0, Span::new(5, 6));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn signature_visibility_reuses_the_declaration_provider() {
        const DECLARATIONS: usize = 64;
        const OCCURRENCES: usize = 48;

        let mut provider = String::from("module provider;");
        for index in 0..DECLARATIONS {
            provider.push_str(&format!(" pub host type Type{index};"));
        }
        let provider = parse_module(&provider);
        let consumer = parse_module("module consumer; import provider as p;");
        let provider_scope = crate::pass::resolve::TopLevelScope::build(&provider)
            .expect("provider declaration scope");
        let consumer_scope = crate::pass::resolve::TopLevelScope::build(&consumer)
            .expect("consumer declaration scope");
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
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");
        let leaf = || {
            crate::ast::Type::synth_path(
                vec!["p".to_owned(), format!("Type{}", DECLARATIONS - 1)],
                Vec::new(),
                Span::new(0, 0),
            )
        };
        let signature = (1..OCCURRENCES).fold(leaf(), |left, _| crate::ast::Type::Product {
            left: Box::new(left),
            right: Box::new(leaf()),
            meta: crate::ast::Meta::new(Span::new(0, 0)),
        });

        crate::pass::resolve::reset_nominal_provider_work();
        check_signature_type_visibility(
            &signature,
            &HashSet::new(),
            &crate::ast::Visibility::Public,
            &consumer.path,
            "test signature",
            &env,
        )
        .expect("public signature");
        let work = crate::pass::resolve::nominal_provider_work();
        assert!(
            work.checking_scope_builds == 0
                && work.declaration_items_indexed == 0
                && work.edge_target_lookups <= 1,
            "signature visibility must borrow the package declaration index and resolve one written edge for every repeated path, not rescan declarations: {work:?}; provider declarations={}",
            provider.items.len(),
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn declaration_visibility_and_kind_share_one_checking_provider() {
        const DECLARATIONS: usize = 64;
        const PARAMETERS: usize = 8;

        let mut source = String::from("module source;");
        for index in 0..DECLARATIONS {
            source.push_str(&format!(" type Unrelated{index} = .;"));
        }
        source.push_str(" host type Leaf; pub fn probe(");
        for index in 0..PARAMETERS {
            if index != 0 {
                source.push_str(", ");
            }
            source.push_str(&format!("value{index}: Leaf"));
        }
        source.push_str(") -> Leaf { value0 }");

        let module = parse_module(&source);
        let env = ModuleEnv::build(&module, None, None, None).expect("checking-root environment");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::FnDef(function) if function.name == "probe" => Some(function),
                _ => None,
            })
            .expect("probe function");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();

        super::super::types::reset_nominal_head_resolution_work();
        crate::pass::resolve::reset_nominal_provider_work();
        check_fn_signature(function, &env, &mut elaborations).expect("function signature");
        let work = crate::pass::resolve::nominal_provider_work();
        assert!(
            work.checking_scope_builds == 1 && work.declaration_items_indexed <= module.items.len(),
            "one declaration must share one checking-root provider across all parameter/result visibility and kind walks: {work:?}, declarations={}",
            module.items.len(),
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn compile_time_handles_require_direct_resolved_receivers() {
        check_named_signature(
            "module main; host type Comptime_bool; \
             pure fn ordinary(value: Comptime_bool) -> Comptime_bool { value }",
            "ordinary",
        )
        .expect("a same-spelled user type is not a compile-time handle");
        check_named_signature(
            "module main; import __comptime__; \
             type Proof = __Comptime__; type Fills = __Fill_ctx__; \
             pure fn valid(ct: Proof, fills: Fills, wrapped: __Type__ | .) -> Fills { fills }",
            "valid",
        )
        .expect("transparent aliases preserve exact direct capability identities");

        let return_only = check_named_signature(
            "module main; import __comptime__; pure fn bad() -> __Type__ { () }",
            "bad",
        )
        .expect_err("a return type cannot hide the required proof receiver");
        assert!(return_only.diag().1.contains("first value parameter"));

        let nested = check_named_signature(
            "module main; import __comptime__; \
             pure fn bad(packet: __Comptime__ & .) -> . { () }",
            "bad",
        )
        .expect_err("a nested proof is not the direct erasure receiver");
        assert!(nested.diag().1.contains("first value parameter"));

        let fill_late = check_named_signature(
            "module main; import __comptime__; \
             pure fn bad(ct: __Comptime__, value: ., fills: __Fill_ctx__) -> __Fill_ctx__ { fills }",
            "bad",
        )
        .expect_err("FillCtx must be the direct second value parameter");
        assert!(fill_late.diag().1.contains("second value parameter"));
    }

    #[cfg(all(feature = "parallel", feature = "surface"))]
    #[test]
    fn typecheck_execution_only_fans_out_when_allowed() {
        assert!(TypecheckExecution::AllowParallel.parallelizes(2));
        assert!(!TypecheckExecution::AllowParallel.parallelizes(1));
        assert!(!TypecheckExecution::Sequential.parallelizes(8));
    }
}
