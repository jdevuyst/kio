//! Operator-fold pass: Surface → Surface.
//!
//! Walks each module's expressions, looks up every `Expr::OpChain`
//! placeholder in the module's operator scope (built from the
//! module's own `Item::Op` entries plus the cross-module operator
//! bindings selected by full grammar in `import provider(op ...);`), and
//! replaces the placeholder with a concrete `Expr::Call`. The pass
//! runs between parsing and desugar; after it completes, no
//! `Expr::OpChain` nodes remain in the AST.
//!
//! Operator imports retain the callable's existing import identity. The fold
//! pass allocates collision-free aliases for module members and keeps builtin
//! values under their ordinary builtin import.
//!
// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use crate::ast::{
    CallArg, CallableSpec, ElaboratorCall, Equiv, Expr, FnDef, Import, ImportItem, ImportKind,
    Item, LabelValueLabel, Labels, LexicalCallablePath, Meta, Module, ModulePath, Op, OpBody,
    OpChainKind, OpPart, OperatorDispatchKey, PackageFile, PathSegment, Signature, SignatureParam,
    Surface, Type, TypeAlias, VariadicOperator, VariadicSpec, Visibility,
};
use crate::error::Error;
use crate::span::Span;

/// Run the operator-fold pass over every module in a package.
/// Builds the package-wide pub-op map first (one serial walk
/// over all modules, gathering each module's exported operator
/// bindings), then folds each module against the map in parallel.
/// The package-wide map build is serial because rayon adds no
/// value at the `O(items)` cost it pays; the per-module fold is
/// parallel because each module's fold reads the map immutably
/// and mutates only its own AST.
#[allow(clippy::type_complexity)] // returns the same Vec shape the FullPipeline wants.
pub fn fold_package(
    modules: Vec<(PathBuf, Module<Surface>)>,
) -> Result<Vec<(PathBuf, Module<Surface>)>, (PathBuf, Error)> {
    fold_package_named(modules, None)
}

/// As [`fold_package`], but accepts the named pipeline's package context.
/// Regular-module scope keys use declared module paths, independent of the
/// configured package storage namespace.
#[allow(clippy::type_complexity)]
pub fn fold_package_named(
    modules: Vec<(PathBuf, Module<Surface>)>,
    package_name: Option<&str>,
) -> Result<Vec<(PathBuf, Module<Surface>)>, (PathBuf, Error)> {
    fold_package_named_with_scope(modules, package_name).map(|(modules, _)| modules)
}

/// As [`fold_package_named`], retaining the exact package scope that drove
/// validation and folding for a caller that needs another projection of the
/// same resolution decisions.
#[allow(clippy::type_complexity)]
pub(crate) fn fold_package_named_with_scope(
    mut modules: Vec<(PathBuf, Module<Surface>)>,
    package_name: Option<&str>,
) -> Result<(Vec<(PathBuf, Module<Surface>)>, PackageOpScope), (PathBuf, Error)> {
    let package = PackageOpScope::from_modules_named(&modules, package_name)?;
    // Per-module fold via rayon. Each module's fold reads the
    // package-wide map by reference; no shared mutation.
    // Errors are collected into a per-module
    // Vec<Option<...>>, then ordered by module path and span so the
    // reported error is deterministic regardless of execution order.
    let errors: Vec<Option<ModuleFoldError>> = crate::maybe_par_iter_mut!(modules)
        .map(|(file_path, module)| {
            let module_path = module_path_str(&module.path);
            fold_module(module, &package)
                .err()
                .map(|error| ModuleFoldError::new(module_path, file_path.clone(), error))
        })
        .collect();
    if let Some(error) = first_module_fold_error(errors) {
        return Err((error.file_path, error.error));
    }
    Ok((modules, package))
}

struct ModuleFoldError {
    module_path: String,
    span: Span,
    file_path: PathBuf,
    error: Error,
}

impl ModuleFoldError {
    fn new(module_path: String, file_path: PathBuf, error: Error) -> Self {
        let (span, _) = error.diag();
        Self {
            module_path,
            span,
            file_path,
            error,
        }
    }
}

fn first_module_fold_error(errors: Vec<Option<ModuleFoldError>>) -> Option<ModuleFoldError> {
    errors.into_iter().flatten().min_by(|a, b| {
        (a.module_path.as_str(), a.span.start, a.span.end).cmp(&(
            b.module_path.as_str(),
            b.span.start,
            b.span.end,
        ))
    })
}

/// Run the operator-fold pass over a package's package file (if any).
/// The package file carries only the phase-independent `bridge` glob
/// list — it has no item bodies that could contain `Expr::OpChain` —
/// so this is a structural pass-through.
pub fn fold_package_file_in_package(
    package: PackageFile<Surface>,
    _package_scope: &PackageOpScope,
) -> Result<PackageFile<Surface>, Error> {
    Ok(package)
}

/// Per-module fold. Builds the consumer module's operator scope
/// (module-local ops + cross-module imports resolved against the
/// `package` map), then walks every item replacing `Expr::OpChain`
/// nodes with concrete `Expr::Call`. Injects synthetic
/// collision-free qualified imports for the source modules referenced by
/// operator-pattern imports, and strips the original
/// operator-pattern items from the surviving imports so the
/// post-fold AST contains only regular identifier imports plus the
/// synthetic qualified-alias clauses.
fn fold_module(module: &mut Module<Surface>, package: &PackageOpScope) -> Result<(), Error> {
    validate_callable_decls(module, package)?;
    let (scope, synthetic_imports) = build_module_scope(module, package)?;
    strip_import_op_patterns(&mut module.imports);
    module.imports.extend(synthetic_imports);
    for item in module.items.iter_mut() {
        fold_item(item, &scope)?;
    }
    Ok(())
}

/// Fold one module against a pre-built package operator scope.
///
/// This is the per-module half of [`fold_package_named`]. Batch
/// scheduling uses it after collecting the package's `pub op` summary
/// from lazy module headers, so a ready module can be folded without
/// forcing unrelated module bodies.
pub fn fold_module_in_package(
    module: &mut Module<Surface>,
    package: &PackageOpScope,
) -> Result<(), Error> {
    fold_module(module, package)
}

fn validate_callable_decls(
    module: &Module<Surface>,
    package: &PackageOpScope,
) -> Result<(), Error> {
    validate_callable_decls_observed(module, package, |_, _, _| {})
}

fn validate_callable_decls_observed(
    module: &Module<Surface>,
    package: &PackageOpScope,
    mut observe: impl FnMut(usize, &LexicalCallablePath, &ResolvedCallableTarget),
) -> Result<(), Error> {
    for (item_index, item) in module.items.iter().enumerate() {
        match item {
            Item::Op(d, _) => {
                let OpBody::Normal { pattern, function } = &d.body;
                let description = format!(
                    "operator `{}`",
                    crate::ast::OperatorGrammar::fixed(pattern).render()
                );
                let context = ImplValidationContext {
                    module,
                    visible_items: &module.items[..item_index],
                    package,
                    referrer_vis: &d.vis,
                    referrer: &description,
                };
                let target = validate_impl_path(&context, function)?;
                observe(item_index, function, &target.callable);
            }
            Item::VariadicOperator(d, _) => {
                let description = format!(
                    "operator `{}`",
                    crate::ast::OperatorGrammar::variadic(&d.open, &d.spec).render()
                );
                let context = ImplValidationContext {
                    module,
                    visible_items: &module.items[..item_index],
                    package,
                    referrer_vis: &d.vis,
                    referrer: &description,
                };
                let base = validate_callable(&context, &d.spec.initializer)?;
                observe(item_index, &d.spec.initializer.path, &base.callable);
                let step = validate_callable(&context, &d.spec.step)?;
                observe(item_index, &d.spec.step.path, &step.callable);
                if let Some(finalize) = &d.spec.finalize {
                    let target = validate_callable(&context, finalize)?;
                    observe(item_index, &finalize.path, &target.callable);
                }
            }
            Item::Elaborator(d, _) => {
                let description = format!("elaborator `{}`", d.name);
                let context = ImplValidationContext {
                    module,
                    visible_items: &module.items[..item_index],
                    package,
                    referrer_vis: &d.vis,
                    referrer: &description,
                };
                let target = require_resolved_impl_path(&context, &d.implementation)?;
                observe(item_index, &d.implementation, &target.callable);
            }
            _ => {}
        }
    }
    Ok(())
}

struct ImplValidationContext<'a> {
    module: &'a Module<Surface>,
    visible_items: &'a [Item<Surface>],
    package: &'a PackageOpScope,
    referrer_vis: &'a Visibility,
    referrer: &'a str,
}

struct ResolvedImplTarget {
    callable: ResolvedCallableTarget,
    visibilities: Vec<(Visibility, ModulePath, String)>,
}

impl ResolvedImplTarget {
    fn validate(&self, context: &ImplValidationContext<'_>, span: Span) -> Result<(), Error> {
        for (visibility, owner, description) in &self.visibilities {
            require_impl_visibility(visibility, owner, description, context, span)?;
        }
        Ok(())
    }
}

fn validate_callable(
    context: &ImplValidationContext<'_>,
    callable: &CallableSpec,
) -> Result<ResolvedImplTarget, Error> {
    validate_impl_path(context, &callable.path)
}

fn validate_impl_path(
    context: &ImplValidationContext<'_>,
    path: &LexicalCallablePath,
) -> Result<ResolvedImplTarget, Error> {
    let target = require_resolved_impl_path(context, path)?;
    target.validate(context, path.span())?;
    Ok(target)
}

fn require_resolved_impl_path(
    context: &ImplValidationContext<'_>,
    path: &LexicalCallablePath,
) -> Result<ResolvedImplTarget, Error> {
    let span = path.span();
    let Some(target) = resolve_impl_path(context, path) else {
        return Err(Error::name_res(
            span,
            format!(
                "{} implementation `{}` does not resolve to a function or newtype member",
                context.referrer,
                render_callable_path(path)
            ),
        )
        .with_help(
            "declare a same-module target before this declaration, or import a target from another module",
        ));
    };
    Ok(target)
}

fn resolve_impl_path(
    context: &ImplValidationContext<'_>,
    path: &LexicalCallablePath,
) -> Option<ResolvedImplTarget> {
    let head = path.first()?;
    match path.segments() {
        [_] => resolve_bare_impl_path(context, head),
        [head, member] => resolve_newtype_member_head(context, head, member)
            .or_else(|| resolve_qualified_impl_path(context, head, member)),
        [alias, type_name, member] => {
            resolve_qualified_newtype_impl_path(context, alias, type_name, member)
        }
        [_, _, _, ..] => None,
        [] => unreachable!("empty path rejected above"),
    }
}

fn render_callable_path(path: &LexicalCallablePath) -> String {
    path.iter()
        .map(PathSegment::as_str)
        .collect::<Vec<_>>()
        .join(".")
}

fn resolved_fn_target(
    visibility: &Visibility,
    owner: &ModulePath,
    name: &str,
) -> ResolvedImplTarget {
    ResolvedImplTarget {
        callable: ResolvedCallableTarget::Module {
            owner: owner.clone(),
            member: vec![name.to_owned()],
            #[cfg(feature = "lsp")]
            source_type: None,
        },
        visibilities: vec![(
            visibility.clone(),
            owner.clone(),
            format!("function `{name}`"),
        )],
    }
}

fn resolved_builtin_target(kind: ResolvedBuiltinKind, name: &str) -> ResolvedImplTarget {
    ResolvedImplTarget {
        callable: ResolvedCallableTarget::Builtin {
            kind,
            name: name.to_owned(),
        },
        visibilities: Vec::new(),
    }
}

fn resolved_newtype_target(
    target: &NewtypeTarget,
    owner: &ModulePath,
    type_name: &str,
    member: &str,
) -> Option<ResolvedImplTarget> {
    let source_kind = match &target.source_head {
        ResolvedCallableTypeHead::TypeAlias { .. } => "type alias",
        ResolvedCallableTypeHead::Newtype { .. } => "newtype",
    };
    let mut visibilities = vec![(
        target.vis.clone(),
        owner.clone(),
        format!("{source_kind} `{type_name}`"),
    )];
    if member == target.constructor_name {
        visibilities.push((
            target.constructor_vis.clone(),
            target.member_owner.clone(),
            format!("newtype member `{type_name}.{member}`"),
        ));
    } else if member == target.projector_name {
        visibilities.push((
            target.projector_vis.clone(),
            target.member_owner.clone(),
            format!("newtype member `{type_name}.{member}`"),
        ));
    } else {
        return None;
    }
    Some(ResolvedImplTarget {
        callable: ResolvedCallableTarget::Module {
            owner: target.member_owner.clone(),
            member: vec![target.member_type_name.clone(), member.to_owned()],
            #[cfg(feature = "lsp")]
            source_type: Some(target.source_head.clone()),
        },
        visibilities,
    })
}

fn resolve_bare_impl_path(
    context: &ImplValidationContext<'_>,
    name: &str,
) -> Option<ResolvedImplTarget> {
    if let Some(target) = resolve_local_module_fn(context, name) {
        return Some(target);
    }
    if module_imports_intrinsic(context.module, name) {
        return Some(resolved_builtin_target(
            ResolvedBuiltinKind::Intrinsic,
            name,
        ));
    }
    if module_imports_comptime_value(context.module, name) {
        return Some(resolved_builtin_target(ResolvedBuiltinKind::Comptime, name));
    }
    for u in &context.module.imports {
        let ImportKind::Selective { items, from } = &u.kind else {
            continue;
        };
        if !items
            .iter()
            .filter_map(ImportItem::as_name)
            .any(|imported| imported == name)
        {
            continue;
        }
        let from_path = module_path_str(from);
        if from_path == module_path_str(&context.module.path) {
            continue;
        }
        if let Some(target) = context.package.fn_target(&from_path, name) {
            return Some(resolved_fn_target(&target.vis, from, name));
        }
    }
    None
}

fn resolve_local_module_fn(
    context: &ImplValidationContext<'_>,
    name: &str,
) -> Option<ResolvedImplTarget> {
    if let Some(d) = context.visible_items.iter().find_map(|item| match item {
        Item::FnDef(d) if d.name == name => Some(d),
        Item::RecGroup(group, _) => group.members.iter().find(|d| d.name == name),
        _ => None,
    }) {
        return Some(resolved_fn_target(&d.vis, &context.module.path, name));
    }
    if context
        .visible_items
        .iter()
        .any(|item| matches!(item, Item::HostFn(host) if host.name == name))
    {
        return Some(resolved_fn_target(
            &Visibility::Public,
            &context.module.path,
            name,
        ));
    }
    None
}

fn resolve_newtype_member_head(
    context: &ImplValidationContext<'_>,
    type_name: &str,
    member: &str,
) -> Option<ResolvedImplTarget> {
    if let Some(target) = resolve_local_newtype_member(context, type_name, member) {
        return Some(target);
    }
    for u in &context.module.imports {
        let ImportKind::Selective { items, from } = &u.kind else {
            continue;
        };
        if !items
            .iter()
            .filter_map(ImportItem::as_name)
            .any(|imported| imported == type_name)
        {
            continue;
        }
        let from_path = module_path_str(from);
        if from_path == module_path_str(&context.module.path) {
            continue;
        }
        if let Some(target) = context.package.newtype_target(&from_path, type_name) {
            return resolved_newtype_target(target, from, type_name, member);
        }
    }
    None
}

fn resolve_local_newtype_member(
    context: &ImplValidationContext<'_>,
    type_name: &str,
    member: &str,
) -> Option<ResolvedImplTarget> {
    if let Some(d) = context
        .visible_items
        .iter()
        .find_map(|item| item_newtype(item, type_name))
    {
        let target = NewtypeTarget::from_newtype(d, &context.module.path);
        return resolved_newtype_target(&target, &context.module.path, type_name, member);
    }
    if let Some(target) = context
        .visible_items
        .iter()
        .find_map(|item| item_label_newtype_target(item, &context.module.path, type_name))
    {
        return resolved_newtype_target(&target, &context.module.path, type_name, member);
    }
    if context
        .visible_items
        .iter()
        .any(|item| item_has_type_alias(item, type_name))
        && let Some(target) = context
            .package
            .newtype_target(&module_path_str(&context.module.path), type_name)
    {
        return resolved_newtype_target(target, &context.module.path, type_name, member);
    }
    None
}

fn resolve_qualified_impl_path(
    context: &ImplValidationContext<'_>,
    alias: &str,
    member: &str,
) -> Option<ResolvedImplTarget> {
    for u in &context.module.imports {
        let ImportKind::Qualified {
            path,
            alias: import_alias,
            ..
        } = &u.kind
        else {
            continue;
        };
        if import_alias != alias {
            continue;
        }
        let from_path = module_path_str(path);
        if from_path == module_path_str(&context.module.path) {
            return resolve_local_module_fn(context, member);
        }
        if let Some(target) = context.package.fn_target(&from_path, member) {
            return Some(resolved_fn_target(&target.vis, path, member));
        }
    }
    None
}

fn resolve_qualified_newtype_impl_path(
    context: &ImplValidationContext<'_>,
    alias: &str,
    type_name: &str,
    member: &str,
) -> Option<ResolvedImplTarget> {
    for u in &context.module.imports {
        let ImportKind::Qualified {
            path,
            alias: import_alias,
            ..
        } = &u.kind
        else {
            continue;
        };
        if import_alias != alias {
            continue;
        }
        let from_path = module_path_str(path);
        if from_path == module_path_str(&context.module.path) {
            return resolve_local_newtype_member(context, type_name, member);
        }
        let target = context.package.newtype_target(&from_path, type_name)?;
        return resolved_newtype_target(target, path, type_name, member);
    }
    None
}

fn module_imports_intrinsic(module: &Module<Surface>, name: &str) -> bool {
    module
        .imports
        .iter()
        .any(|import_| matches!(import_.kind, ImportKind::Intrinsics))
        && crate::pass::resolve::PRIME_INTRINSICS.contains(&name)
}

fn module_imports_comptime_value(module: &Module<Surface>, name: &str) -> bool {
    module
        .imports
        .iter()
        .any(|import_| matches!(import_.kind, ImportKind::Comptime))
        && crate::comptime::ComptimeBuiltin::from_public_name(name)
            .is_some_and(crate::comptime::ComptimeBuiltin::is_value_name)
}

fn require_impl_visibility(
    target_vis: &Visibility,
    target_owner: &ModulePath,
    target: &str,
    context: &ImplValidationContext<'_>,
    span: Span,
) -> Result<(), Error> {
    if crate::pass::resolve::visibility_covers(
        target_vis,
        target_owner,
        context.referrer_vis,
        &context.module.path,
    ) {
        Ok(())
    } else {
        Err(Error::type_(
            span,
            format!(
                "{target} must be at least as visible as {}",
                context.referrer
            ),
        )
        .with_help(format!(
            "give {target} visibility equal to or wider than {}",
            context.referrer
        )))
    }
}

/// Walk each `Import` and drop any operator-pattern items from
/// selective imports. Import clauses whose item list becomes empty
/// after the strip are dropped entirely.
fn strip_import_op_patterns(imports: &mut Vec<Import>) {
    imports.retain_mut(|u| {
        let ImportKind::Selective { items, .. } = &mut u.kind else {
            return true;
        };
        items.retain(|item| matches!(item, ImportItem::Name { .. } | ImportItem::Label { .. }));
        !items.is_empty()
    });
}

/// A named operator callable after definition-site resolution. Ordinary
/// declarations retain their exact module/member identity; builtin module
/// values retain the existing import class that makes their bare name valid.
#[derive(Clone)]
pub(crate) enum ResolvedCallableTarget {
    Module {
        owner: ModulePath,
        member: Vec<String>,
        /// The source-visible type declaration that qualified a terminal
        /// newtype member. This differs from `owner`/`member` when the written
        /// head is an identity alias; navigation and source-order checks keep
        /// the alias identity while lowering routes the member to its nominal
        /// owner.
        #[cfg(feature = "lsp")]
        source_type: Option<ResolvedCallableTypeHead>,
    },
    Builtin {
        kind: ResolvedBuiltinKind,
        name: String,
    },
}

#[derive(Clone)]
pub(crate) enum ResolvedCallableTypeHead {
    TypeAlias {
        #[cfg(feature = "lsp")]
        owner: ModulePath,
        #[cfg(feature = "lsp")]
        name: String,
    },
    Newtype {
        #[cfg(feature = "lsp")]
        owner: ModulePath,
        #[cfg(feature = "lsp")]
        name: String,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum ResolvedBuiltinKind {
    Intrinsic,
    Comptime,
}

/// One source-written operator/fold callable path paired with the semantic
/// target selected by the ordinary declaration resolver. The LSP consumes
/// these facts after typechecking so declaration positions use their original
/// segment spans. `source_item_start` locates the declaration in source order
/// after surface-only declarations have been erased;
/// generated operator calls deliberately keep their call-site spans and are
/// not used as a source-position proxy.
#[derive(Clone)]
#[cfg(feature = "lsp")]
pub(crate) struct ResolvedCallableDeclaration {
    pub source_module: String,
    pub source_item_start: u32,
    pub source_path: LexicalCallablePath,
    pub target: ResolvedCallableTarget,
}

#[derive(Clone)]
struct ResolvedPubOpRecord {
    name: OperatorDispatchKey,
    body: ResolvedPubOpRecordBody,
}

#[derive(Clone)]
enum ResolvedPubOpRecordBody {
    Normal {
        pattern: Vec<OpPart>,
        target: ResolvedCallableTarget,
    },
    VariadicOperator {
        open: Vec<String>,
        spec: Box<VariadicSpec>,
        base_target: ResolvedCallableTarget,
        step_target: ResolvedCallableTarget,
        finalize_target: Option<Box<ResolvedCallableTarget>>,
    },
}

/// Package-wide pub-op catalogue. Keyed by source-module slash path. Named
/// callables retain their resolved definition-site module/member or builtin
/// import identities.
pub struct PackageOpScope {
    by_module: HashMap<String, Vec<ResolvedPubOpRecord>>,
    fn_targets_by_module: HashMap<String, HashMap<String, FnTarget>>,
    newtype_targets_by_module: HashMap<String, HashMap<String, NewtypeTarget>>,
    resolved_callable_targets_by_item: HashMap<(String, u32), Vec<ResolvedCallableTarget>>,
    #[cfg(feature = "lsp")]
    resolved_callable_declarations: Vec<ResolvedCallableDeclaration>,
    #[cfg(feature = "lsp")]
    resolved_callable_declaration_ranges: HashMap<String, std::ops::Range<usize>>,
}

struct FnTarget {
    vis: Visibility,
}

#[derive(Clone)]
struct NewtypeTarget {
    vis: Visibility,
    source_head: ResolvedCallableTypeHead,
    member_owner: ModulePath,
    member_type_name: String,
    constructor_name: String,
    constructor_vis: Visibility,
    projector_name: String,
    projector_vis: Visibility,
}

impl NewtypeTarget {
    fn from_newtype(d: &crate::ast::Newtype<Surface>, owner: &ModulePath) -> Self {
        Self {
            vis: d.vis.clone(),
            source_head: ResolvedCallableTypeHead::Newtype {
                #[cfg(feature = "lsp")]
                owner: owner.clone(),
                #[cfg(feature = "lsp")]
                name: d.name.clone(),
            },
            member_owner: owner.clone(),
            member_type_name: d.name.clone(),
            constructor_name: d.constructor.name.clone(),
            constructor_vis: d.constructor.vis.clone(),
            projector_name: d.projector.name.clone(),
            projector_vis: d.projector.vis.clone(),
        }
    }
}

fn label_newtype_target(
    labels: &Labels<Surface>,
    owner: &ModulePath,
    type_name: &str,
) -> Option<NewtypeTarget> {
    labels
        .entries
        .iter()
        .filter(|entry| !entry.is_reuse_marker())
        .find(|entry| crate::ast::mint_label_newtype_name(&entry.name) == type_name)
        .map(|_| label_newtype_shape(labels, owner, type_name))
}

fn label_newtype_shape(
    labels: &Labels<Surface>,
    owner: &ModulePath,
    type_name: &str,
) -> NewtypeTarget {
    NewtypeTarget {
        vis: labels.vis.clone(),
        source_head: ResolvedCallableTypeHead::Newtype {
            #[cfg(feature = "lsp")]
            owner: owner.clone(),
            #[cfg(feature = "lsp")]
            name: type_name.to_owned(),
        },
        member_owner: owner.clone(),
        member_type_name: type_name.to_owned(),
        constructor_name: "mk".to_owned(),
        constructor_vis: labels.vis.clone(),
        projector_name: "get".to_owned(),
        projector_vis: labels.vis.clone(),
    }
}

fn item_newtype<'a>(
    item: &'a Item<Surface>,
    type_name: &str,
) -> Option<&'a crate::ast::Newtype<Surface>> {
    match item {
        Item::Newtype(newtype) if newtype.name == type_name => Some(newtype),
        Item::TypeRecGroup(group) => group.members.iter().find_map(|member| match member {
            crate::ast::TypeRecMember::Newtype(newtype) if newtype.name == type_name => {
                Some(newtype)
            }
            _ => None,
        }),
        _ => None,
    }
}

fn item_type_alias<'a>(item: &'a Item<Surface>, type_name: &str) -> Option<&'a TypeAlias<Surface>> {
    match item {
        Item::TypeAlias(alias) if alias.name == type_name => Some(alias),
        Item::TypeRecGroup(group) => group.members.iter().find_map(|member| match member {
            crate::ast::TypeRecMember::TypeAlias(alias) if alias.name == type_name => Some(alias),
            _ => None,
        }),
        _ => None,
    }
}

fn item_has_type_alias(item: &Item<Surface>, type_name: &str) -> bool {
    if item_type_alias(item, type_name).is_some() {
        return true;
    }
    match item {
        Item::Labels(labels, _) => labels.type_alias_name.as_deref() == Some(type_name),
        Item::TypeRecGroup(group) => group.members.iter().any(|member| {
            matches!(member,
                crate::ast::TypeRecMember::Labels(labels, _)
                    if labels.type_alias_name.as_deref() == Some(type_name))
        }),
        _ => false,
    }
}

fn visit_item_identity_alias_heads(item: &Item<Surface>, mut visit: impl FnMut(&str, &Visibility)) {
    match item {
        Item::TypeAlias(alias) => visit(&alias.name, &alias.vis),
        Item::Labels(labels, _) => {
            if let Some(name) = &labels.type_alias_name {
                visit(name, &labels.vis);
            }
        }
        Item::TypeRecGroup(group) => {
            for member in &group.members {
                match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => {
                        visit(&alias.name, &alias.vis);
                    }
                    crate::ast::TypeRecMember::Labels(labels, _) => {
                        if let Some(name) = &labels.type_alias_name {
                            visit(name, &labels.vis);
                        }
                    }
                    crate::ast::TypeRecMember::Newtype(_) => {}
                }
            }
        }
        _ => {}
    }
}

fn item_label_newtype_target(
    item: &Item<Surface>,
    owner: &ModulePath,
    type_name: &str,
) -> Option<NewtypeTarget> {
    match item {
        Item::Labels(labels, _) => label_newtype_target(labels, owner, type_name),
        Item::TypeRecGroup(group) => group.members.iter().find_map(|member| match member {
            crate::ast::TypeRecMember::Labels(labels, _) => {
                label_newtype_target(labels, owner, type_name)
            }
            _ => None,
        }),
        _ => None,
    }
}

fn insert_item_newtype_targets(
    item: &Item<Surface>,
    owner: &ModulePath,
    targets: &mut HashMap<String, NewtypeTarget>,
) {
    match item {
        Item::Newtype(newtype) => {
            targets.insert(
                newtype.name.clone(),
                NewtypeTarget::from_newtype(newtype, owner),
            );
        }
        Item::Labels(labels, _) => insert_label_newtype_targets(labels, owner, targets),
        Item::TypeRecGroup(group) => {
            for member in &group.members {
                match member {
                    crate::ast::TypeRecMember::Newtype(newtype) => {
                        targets.insert(
                            newtype.name.clone(),
                            NewtypeTarget::from_newtype(newtype, owner),
                        );
                    }
                    crate::ast::TypeRecMember::Labels(labels, _) => {
                        insert_label_newtype_targets(labels, owner, targets);
                    }
                    crate::ast::TypeRecMember::TypeAlias(_) => {}
                }
            }
        }
        _ => {}
    }
}

fn insert_label_newtype_targets(
    labels: &Labels<Surface>,
    owner: &ModulePath,
    targets: &mut HashMap<String, NewtypeTarget>,
) {
    for entry in labels
        .entries
        .iter()
        .filter(|entry| !entry.is_reuse_marker())
    {
        let type_name = crate::ast::mint_label_newtype_name(&entry.name);
        targets.insert(
            type_name.clone(),
            label_newtype_shape(labels, owner, &type_name),
        );
    }
}

impl PackageOpScope {
    fn resolution_index(modules: &[(PathBuf, Module<Surface>)]) -> Self {
        let mut fn_targets_by_module: HashMap<String, HashMap<String, FnTarget>> = HashMap::new();
        let mut newtype_targets_by_module: HashMap<String, HashMap<String, NewtypeTarget>> =
            HashMap::new();
        for (_, module) in modules {
            let path = module_path_str(&module.path);
            let fn_entry = fn_targets_by_module.entry(path.clone()).or_default();
            let newtype_entry = newtype_targets_by_module.entry(path.clone()).or_default();
            for item in &module.items {
                match item {
                    Item::FnDef(d) => {
                        fn_entry.insert(d.name.clone(), FnTarget { vis: d.vis.clone() });
                    }
                    Item::RecGroup(group, _) => {
                        for d in &group.members {
                            fn_entry.insert(d.name.clone(), FnTarget { vis: d.vis.clone() });
                        }
                    }
                    Item::HostFn(host) => {
                        fn_entry.insert(
                            host.name.clone(),
                            FnTarget {
                                vis: Visibility::Public,
                            },
                        );
                    }
                    _ => insert_item_newtype_targets(item, &module.path, newtype_entry),
                }
            }
        }
        let identity_aliases =
            crate::pass::resolve::IdentityAliasNewtypeIndex::build_for_surface_modules(
                modules.iter().map(|(_, module)| module),
            );
        for (_, module) in modules {
            let source_module = module_path_str(&module.path);
            for item in &module.items {
                visit_item_identity_alias_heads(item, |alias_name, alias_vis| {
                    let Some((terminal_module, terminal_name)) =
                        identity_aliases.terminal_key(&source_module, alias_name)
                    else {
                        return;
                    };
                    let Some(mut target) = newtype_targets_by_module
                        .get(terminal_module)
                        .and_then(|targets| targets.get(terminal_name))
                        .cloned()
                    else {
                        return;
                    };
                    target.vis = alias_vis.clone();
                    target.source_head = ResolvedCallableTypeHead::TypeAlias {
                        #[cfg(feature = "lsp")]
                        owner: module.path.clone(),
                        #[cfg(feature = "lsp")]
                        name: alias_name.to_owned(),
                    };
                    newtype_targets_by_module
                        .entry(source_module.clone())
                        .or_default()
                        .insert(alias_name.to_owned(), target);
                });
            }
        }
        Self {
            by_module: HashMap::new(),
            fn_targets_by_module,
            newtype_targets_by_module,
            resolved_callable_targets_by_item: HashMap::new(),
            #[cfg(feature = "lsp")]
            resolved_callable_declarations: Vec::new(),
            #[cfg(feature = "lsp")]
            resolved_callable_declaration_ranges: HashMap::new(),
        }
    }

    /// Build the callable-resolution index, validate every declaration, then
    /// collect each exported operator with exact named-callable identities.
    pub fn from_modules_named(
        modules: &[(PathBuf, Module<Surface>)],
        _package_name: Option<&str>,
    ) -> Result<Self, (PathBuf, Error)> {
        let mut scope = Self::resolution_index(modules);
        let mut errors = Vec::new();
        let mut resolved_callable_targets_by_item = HashMap::new();
        #[cfg(feature = "lsp")]
        let mut resolved_callable_declarations = Vec::new();
        #[cfg(feature = "lsp")]
        let mut resolved_callable_declaration_ranges = HashMap::new();
        for (file_path, module) in modules {
            let source_module = module_path_str(&module.path);
            #[cfg(feature = "lsp")]
            let start = resolved_callable_declarations.len();
            let validation = validate_callable_decls_observed(
                module,
                &scope,
                |item_index, _source_path, target| {
                    resolved_callable_targets_by_item
                        .entry((source_module.clone(), module.items[item_index].span().start))
                        .or_insert_with(Vec::new)
                        .push(target.clone());
                    #[cfg(feature = "lsp")]
                    {
                        resolved_callable_declarations.push(ResolvedCallableDeclaration {
                            source_module: source_module.clone(),
                            source_item_start: module.items[item_index].span().start,
                            source_path: _source_path.clone(),
                            target: target.clone(),
                        });
                    }
                },
            );
            #[cfg(feature = "lsp")]
            {
                let end = resolved_callable_declarations.len();
                resolved_callable_declaration_ranges.insert(source_module, start..end);
            }
            errors.push(validation.err().map(|error| {
                ModuleFoldError::new(module_path_str(&module.path), file_path.clone(), error)
            }));
        }
        if let Some(error) = first_module_fold_error(errors) {
            return Err((error.file_path, error.error));
        }
        scope.resolved_callable_targets_by_item = resolved_callable_targets_by_item;
        #[cfg(feature = "lsp")]
        {
            scope.resolved_callable_declarations = resolved_callable_declarations;
            scope.resolved_callable_declaration_ranges = resolved_callable_declaration_ranges;
        }

        for (_, module) in modules {
            let mut records = Vec::new();
            for (item_index, item) in module.items.iter().enumerate() {
                match item {
                    Item::Op(d, _) if d.vis.is_pub() => {
                        let OpBody::Normal { pattern, .. } = &d.body;
                        let [target] = scope.resolved_callable_targets(module, item_index) else {
                            unreachable!("validated operator retains exactly one callable target")
                        };
                        records.push(ResolvedPubOpRecord {
                            name: OperatorDispatchKey::from_pattern(pattern),
                            body: ResolvedPubOpRecordBody::Normal {
                                pattern: pattern.clone(),
                                target: target.clone(),
                            },
                        });
                    }
                    Item::VariadicOperator(d, _) if d.vis.is_pub() => {
                        let targets = scope.resolved_callable_targets(module, item_index);
                        let (base_target, step_target, finalize_target) = match targets {
                            [base, step] => (base.clone(), step.clone(), None),
                            [base, step, finalize] => {
                                (base.clone(), step.clone(), Some(Box::new(finalize.clone())))
                            }
                            _ => unreachable!(
                                "validated fold retains base, step, and optional finalize targets"
                            ),
                        };
                        records.push(ResolvedPubOpRecord {
                            name: OperatorDispatchKey::from_variadic(d),
                            body: ResolvedPubOpRecordBody::VariadicOperator {
                                open: d.open.clone(),
                                spec: d.spec.clone(),
                                base_target,
                                step_target,
                                finalize_target,
                            },
                        });
                    }
                    _ => {}
                }
            }
            scope
                .by_module
                .insert(module_path_str(&module.path), records);
        }
        Ok(scope)
    }

    fn fn_target(&self, module_path: &str, name: &str) -> Option<&FnTarget> {
        self.fn_targets_by_module
            .get(module_path)
            .and_then(|module| module.get(name))
    }

    fn newtype_target(&self, module_path: &str, name: &str) -> Option<&NewtypeTarget> {
        self.newtype_targets_by_module
            .get(module_path)
            .and_then(|module| module.get(name))
    }

    fn resolved_callable_targets(
        &self,
        module: &Module<Surface>,
        item_index: usize,
    ) -> &[ResolvedCallableTarget] {
        self.resolved_callable_targets_by_item
            .get(&(
                module_path_str(&module.path),
                module.items[item_index].span().start,
            ))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Project source-written operator/fold callable declarations through this
    /// already-validated package scope. This is deliberately not a second
    /// package-scope construction or validation pass: LSP source positions
    /// must observe the exact resolution environment used by lowering.
    #[cfg(feature = "lsp")]
    pub(crate) fn try_resolved_callable_declarations(
        &self,
        only_source_module: Option<&str>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Option<Vec<ResolvedCallableDeclaration>> {
        let selected = match only_source_module {
            Some(module) => self
                .resolved_callable_declaration_ranges
                .get(module)
                .map(|range| &self.resolved_callable_declarations[range.clone()])
                .unwrap_or_default(),
            None => self.resolved_callable_declarations.as_slice(),
        };
        let mut declarations = Vec::with_capacity(selected.len());
        for declaration in selected {
            if is_cancelled() {
                return None;
            }
            declarations.push(declaration.clone());
        }
        Some(declarations)
    }
}

/// In-scope operator bindings for one module. Keyed structurally
/// per the prefix-forbidden conflict rule: `(leading_op_run,
/// is_prefix)` for normal ops, and the OPEN-token sequence for
/// folds.
struct OpScope {
    normal: HashMap<(Vec<String>, bool), NormalBinding>,
    variadic: HashMap<Vec<String>, VariadicBinding>,
}

#[derive(Clone)]
struct NormalBinding {
    /// Path naming the underlying function after imports have been routed.
    function: LexicalCallablePath,
    pattern: Vec<OpPart>,
}

#[derive(Clone)]
struct VariadicBinding {
    spec: VariadicSpec,
}

impl OpScope {
    fn new() -> Self {
        Self {
            normal: HashMap::new(),
            variadic: HashMap::new(),
        }
    }

    fn insert_local_op(&mut self, d: &Op<Surface>, function: LexicalCallablePath) {
        match &d.body {
            OpBody::Normal { pattern, .. } => {
                let is_prefix = matches!(pattern.first(), Some(OpPart::Token { .. }));
                let leading = leading_op_run(pattern);
                self.normal.insert(
                    (leading, is_prefix),
                    NormalBinding {
                        function,
                        pattern: pattern.clone(),
                    },
                );
            }
        }
    }

    fn insert_local_variadic(&mut self, d: &VariadicOperator<Surface>, spec: VariadicSpec) {
        self.variadic
            .insert(d.open.clone(), VariadicBinding { spec });
    }

    fn insert_imported_normal(
        &mut self,
        source_pattern: &[OpPart],
        function: LexicalCallablePath,
        import_span: Span,
    ) -> Result<(), Error> {
        let key = (
            leading_op_run(source_pattern),
            is_prefix_shape(source_pattern),
        );
        if self.normal.contains_key(&key) {
            return Err(Error::name_res(
                import_span,
                format!(
                    "duplicate operator import `{}`",
                    crate::ast::OperatorGrammar::fixed(source_pattern).render()
                ),
            )
            .with_help("remove the repeated operator import"));
        }
        let binding = NormalBinding {
            function,
            pattern: source_pattern.to_vec(),
        };
        self.normal.insert(key, binding);
        Ok(())
    }

    /// Cross-module variadic-bracket import: register the source module's
    /// already-routed variadic spec under the consumer's import key.
    fn insert_imported_variadic(
        &mut self,
        open: Vec<String>,
        source_spec: VariadicSpec,
        span: Span,
    ) -> Result<(), Error> {
        if self.variadic.contains_key(&open) {
            return Err(Error::name_res(
                span,
                format!(
                    "duplicate operator import `{}`",
                    crate::ast::OperatorGrammar::variadic(&open, &source_spec).render()
                ),
            )
            .with_help("remove the repeated operator import"));
        }
        self.variadic
            .insert(open, VariadicBinding { spec: source_spec });
        Ok(())
    }
}

/// Compute the leading op-token run from a op pattern — the
/// sequence of `Token` parts from the start up to the first slot
/// (or all of them, for prefix patterns).
fn leading_op_run(pattern: &[OpPart]) -> Vec<String> {
    let start = if matches!(
        pattern.first(),
        Some(OpPart::SlotPlain { .. })
            | Some(OpPart::SlotRecursive { .. })
            | Some(OpPart::SlotGreedy { .. })
    ) {
        1
    } else {
        0
    };
    pattern[start..]
        .iter()
        .take_while(|p| matches!(p, OpPart::Token { .. }))
        .filter_map(|p| match p {
            OpPart::Token { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

fn is_prefix_shape(pattern: &[OpPart]) -> bool {
    matches!(pattern.first(), Some(OpPart::Token { .. }))
}

fn module_path_str(p: &ModulePath) -> String {
    p.segments.join("/")
}

/// Build the consumer module's operator scope from module-local
/// ops plus explicit operator-grammar imports. Returns the scope
/// alongside the synthetic qualified imports the fold
/// pass needs to inject so the resolver and typer can resolve the
/// emitted qualified calls.
fn build_module_scope(
    module: &Module<Surface>,
    package: &PackageOpScope,
) -> Result<(OpScope, Vec<Import>), Error> {
    let mut scope = OpScope::new();
    let mut occupied = module_binding_names(module);
    let mut builtin_imports = RoutedBuiltinImports::from_module(module);
    // Keep one deterministic source-module path to synthetic-alias map for
    // declaration targets and imported operators alike.
    let mut aliases: BTreeMap<String, (String, Span)> = BTreeMap::new();
    for (item_index, item) in module.items.iter().enumerate() {
        match item {
            Item::Op(d, _) => {
                let OpBody::Normal { function, .. } = &d.body;
                let [target] = package.resolved_callable_targets(module, item_index) else {
                    unreachable!("validated operator retains exactly one callable target")
                };
                let function = routed_declaration_callable_path(
                    target,
                    &module.path,
                    &mut aliases,
                    &mut occupied,
                    &mut builtin_imports,
                    function.span(),
                );
                scope.insert_local_op(d, function);
            }
            Item::VariadicOperator(d, _) => {
                let targets = package.resolved_callable_targets(module, item_index);
                let (base_target, step_target, finalize_target) = match targets {
                    [base, step] => (base, step, None),
                    [base, step, finalize] => (base, step, Some(finalize)),
                    _ => unreachable!(
                        "validated fold retains base, step, and optional finalize targets"
                    ),
                };
                let mut spec = d.spec.as_ref().clone();
                route_declaration_callable(
                    &mut spec.initializer,
                    base_target,
                    &module.path,
                    &mut aliases,
                    &mut occupied,
                    &mut builtin_imports,
                );
                route_declaration_callable(
                    &mut spec.step,
                    step_target,
                    &module.path,
                    &mut aliases,
                    &mut occupied,
                    &mut builtin_imports,
                );
                if let Some(finalize) = spec.finalize.as_mut() {
                    let target = finalize_target.expect("finalize target accompanies callable");
                    route_declaration_callable(
                        finalize,
                        target,
                        &module.path,
                        &mut aliases,
                        &mut occupied,
                        &mut builtin_imports,
                    );
                }
                scope.insert_local_variadic(d, spec);
            }
            _ => {}
        }
    }
    // Walk imports and extend the same stable routing map.
    for u in &module.imports {
        let ImportKind::Selective { items, from } = &u.kind else {
            continue;
        };
        let from_path = module_path_str(from);
        let Some(source_ops) = package.by_module.get(&from_path).map(Vec::as_slice) else {
            if items
                .iter()
                .any(|item| matches!(item, ImportItem::OperatorPattern { .. }))
            {
                return Err(Error::import(
                    from.span,
                    format!("module `{from_path}` is not in this package"),
                ));
            }
            continue;
        };
        for item in items {
            let ImportItem::OperatorPattern { grammar, span, .. } = item else {
                continue;
            };
            // The explicit provider and dispatch key select at most one
            // declaration; its full projection must then equal the import.
            let dispatch_key = grammar.dispatch_key();
            let matched = source_ops.iter().find(|d| d.name == dispatch_key);
            let Some(matched) = matched else {
                return Err(Error::import(
                    *span,
                    format!(
                        "module `{from_path}` exports no operator matching `{}`",
                        grammar.render(),
                    ),
                ));
            };
            let provided_grammar = match &matched.body {
                ResolvedPubOpRecordBody::Normal { pattern, .. } => {
                    crate::ast::OperatorGrammar::fixed(pattern)
                }
                ResolvedPubOpRecordBody::VariadicOperator { open, spec, .. } => {
                    crate::ast::OperatorGrammar::variadic(open, spec)
                }
            };
            if grammar != &provided_grammar {
                return Err(Error::import(
                    *span,
                    format!(
                        "imported operator grammar does not match its provider: `{}`",
                        grammar.render()
                    ),
                )
                .with_help(format!(
                    "import {from_path}({});",
                    provided_grammar.render()
                )));
            }
            match &matched.body {
                ResolvedPubOpRecordBody::VariadicOperator {
                    open: source_open,
                    spec: source_spec,
                    base_target,
                    step_target,
                    finalize_target,
                } => {
                    let mut routed_spec = (**source_spec).clone();
                    route_imported_callable(
                        &mut routed_spec.initializer,
                        base_target,
                        &mut aliases,
                        &mut occupied,
                        &mut builtin_imports,
                        *span,
                    );
                    route_imported_callable(
                        &mut routed_spec.step,
                        step_target,
                        &mut aliases,
                        &mut occupied,
                        &mut builtin_imports,
                        *span,
                    );
                    if let Some(finalize) = routed_spec.finalize.as_mut() {
                        let finalize_target = finalize_target
                            .as_ref()
                            .expect("resolved finalize target accompanies named callable");
                        route_imported_callable(
                            finalize,
                            finalize_target,
                            &mut aliases,
                            &mut occupied,
                            &mut builtin_imports,
                            *span,
                        );
                    }
                    scope.insert_imported_variadic(source_open.clone(), routed_spec, *span)?;
                }
                ResolvedPubOpRecordBody::Normal { pattern, target } => {
                    let function = routed_callable_path(
                        target,
                        &mut aliases,
                        &mut occupied,
                        &mut builtin_imports,
                        *span,
                    );
                    scope.insert_imported_normal(pattern, function, *span)?;
                }
            }
        }
    }
    // Builtin imports have a fixed order; qualified imports then follow in
    // module-path order from the BTreeMap.
    let mut synthetic: Vec<Import> = Vec::with_capacity(aliases.len() + 2);
    builtin_imports.append_synthetic(&mut synthetic);
    for (from_path, (alias, span)) in &aliases {
        synthetic.push(synthetic_qualified_import(from_path, alias, *span));
    }
    Ok((scope, synthetic))
}

fn route_imported_callable(
    callable: &mut CallableSpec,
    target: &ResolvedCallableTarget,
    aliases: &mut BTreeMap<String, (String, Span)>,
    occupied: &mut BTreeSet<String>,
    builtin_imports: &mut RoutedBuiltinImports,
    import_span: Span,
) {
    callable.path = routed_callable_path(target, aliases, occupied, builtin_imports, import_span);
}

fn route_declaration_callable(
    callable: &mut CallableSpec,
    target: &ResolvedCallableTarget,
    current_module: &ModulePath,
    aliases: &mut BTreeMap<String, (String, Span)>,
    occupied: &mut BTreeSet<String>,
    builtin_imports: &mut RoutedBuiltinImports,
) {
    callable.path = routed_declaration_callable_path(
        target,
        current_module,
        aliases,
        occupied,
        builtin_imports,
        callable.path.span(),
    );
}

/// Spell a declaration-site-resolved callable for later ordinary resolution.
/// Cross-module identities receive the same collision-free qualified route as
/// imported operators. A same-module identity stays in its ordinary local
/// spelling; the checker-boundary alpha normalizer protects these synthesized
/// call heads from value binders at the use site.
fn routed_declaration_callable_path(
    target: &ResolvedCallableTarget,
    current_module: &ModulePath,
    aliases: &mut BTreeMap<String, (String, Span)>,
    occupied: &mut BTreeSet<String>,
    builtin_imports: &mut RoutedBuiltinImports,
    span: Span,
) -> LexicalCallablePath {
    match target {
        ResolvedCallableTarget::Module { owner, member, .. }
            if module_path_str(owner) == module_path_str(current_module) =>
        {
            LexicalCallablePath::synth(member.clone(), span)
        }
        _ => routed_callable_path(target, aliases, occupied, builtin_imports, span),
    }
}

fn routed_callable_path(
    target: &ResolvedCallableTarget,
    aliases: &mut BTreeMap<String, (String, Span)>,
    occupied: &mut BTreeSet<String>,
    builtin_imports: &mut RoutedBuiltinImports,
    import_span: Span,
) -> LexicalCallablePath {
    match target {
        ResolvedCallableTarget::Module { owner, member, .. } => {
            let owner = module_path_str(owner);
            let alias = aliases
                .entry(owner.clone())
                .or_insert_with(|| {
                    let alias = allocate_synthetic_alias(&owner, occupied);
                    (alias, import_span)
                })
                .0
                .clone();
            let mut path = Vec::with_capacity(member.len() + 1);
            path.push(alias);
            path.extend(member.iter().cloned());
            LexicalCallablePath::synth(path, import_span)
        }
        ResolvedCallableTarget::Builtin { kind, name } => {
            builtin_imports.request(*kind, import_span);
            LexicalCallablePath::synth([name.clone()], import_span)
        }
    }
}

struct RoutedBuiltinImports {
    has_intrinsics: bool,
    has_comptime: bool,
    intrinsic_span: Option<Span>,
    comptime_span: Option<Span>,
}

impl RoutedBuiltinImports {
    fn from_module(module: &Module<Surface>) -> Self {
        Self {
            has_intrinsics: module
                .imports
                .iter()
                .any(|import_| matches!(import_.kind, ImportKind::Intrinsics)),
            has_comptime: module
                .imports
                .iter()
                .any(|import_| matches!(import_.kind, ImportKind::Comptime)),
            intrinsic_span: None,
            comptime_span: None,
        }
    }

    fn request(&mut self, kind: ResolvedBuiltinKind, span: Span) {
        let requested = match kind {
            ResolvedBuiltinKind::Intrinsic => &mut self.intrinsic_span,
            ResolvedBuiltinKind::Comptime => &mut self.comptime_span,
        };
        requested.get_or_insert(span);
    }

    fn append_synthetic(&self, imports: &mut Vec<Import>) {
        if !self.has_intrinsics
            && let Some(span) = self.intrinsic_span
        {
            imports.push(synthetic_builtin_import(
                ResolvedBuiltinKind::Intrinsic,
                span,
            ));
        }
        if !self.has_comptime
            && let Some(span) = self.comptime_span
        {
            imports.push(synthetic_builtin_import(
                ResolvedBuiltinKind::Comptime,
                span,
            ));
        }
    }
}

fn module_binding_names(module: &Module<Surface>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for import_ in &module.imports {
        match &import_.kind {
            ImportKind::Qualified { alias, .. } => {
                names.insert(alias.clone());
            }
            ImportKind::Selective { items, .. } => {
                names.extend(
                    items
                        .iter()
                        .filter_map(ImportItem::as_name)
                        .map(str::to_owned),
                );
            }
            ImportKind::Intrinsics | ImportKind::Comptime => {}
        }
    }
    for item in &module.items {
        match item {
            Item::FnDef(def) => {
                names.insert(def.name.clone());
            }
            Item::RecGroup(group, _) => {
                names.extend(group.members.iter().map(|def| def.name.clone()));
            }
            Item::TypeRecGroup(group) => {
                for member in &group.members {
                    match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => {
                            names.insert(alias.name.clone());
                        }
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            names.insert(newtype.name.clone());
                            names.insert(newtype.constructor.name.clone());
                            names.insert(newtype.projector.name.clone());
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            names.extend(labels.type_alias_name.iter().cloned());
                            names.extend(labels.entries.iter().map(|entry| entry.name.clone()));
                            if let Some(arms) = &labels.type_alias_arms {
                                names.extend(
                                    arms.iter()
                                        .flat_map(|arm| arm.entries.iter())
                                        .map(|entry| entry.name.clone()),
                                );
                            }
                        }
                    }
                }
            }
            Item::TypeAlias(alias) => {
                names.insert(alias.name.clone());
            }
            Item::LiteralAlias(alias, _) => {
                names.insert(alias.name.clone());
            }
            Item::Newtype(newtype) => {
                names.insert(newtype.name.clone());
                names.insert(newtype.constructor.name.clone());
                names.insert(newtype.projector.name.clone());
            }
            Item::Labels(labels, _) => {
                names.extend(labels.type_alias_name.iter().cloned());
                names.extend(labels.entries.iter().map(|entry| entry.name.clone()));
                if let Some(arms) = &labels.type_alias_arms {
                    names.extend(
                        arms.iter()
                            .flat_map(|arm| arm.entries.iter())
                            .map(|entry| entry.name.clone()),
                    );
                }
            }
            Item::Equiv(equiv, _) => {
                names.insert(equiv.name.clone());
            }
            Item::Elaborator(elaborator, _) => {
                names.insert(elaborator.name.clone());
            }
            Item::HostType(host) => {
                names.insert(host.name.clone());
            }
            Item::HostFn(host) => {
                names.insert(host.name.clone());
            }
            Item::Op(_, _) | Item::VariadicOperator(_, _) | Item::LabelForward(_, _) => {}
        }
    }
    names
}

/// Allocate an injective, parser-legal alias for one source module. Letter
/// encoding distinguishes every path byte, while the deterministic suffix
/// avoids every consumer-written binding and every alias allocated earlier.
fn allocate_synthetic_alias(from_path: &str, occupied: &mut BTreeSet<String>) -> String {
    let encoded = crate::naming::encode_name_component(from_path);
    let base = format!("_op_{encoded}");
    for suffix in 1usize.. {
        let candidate = if suffix == 1 {
            format!("{base}__")
        } else {
            format!("{base}_n{suffix}__")
        };
        if occupied.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unbounded operator-alias suffix space is never exhausted")
}

/// Synthesize an ordinary qualified import for one operator provider. The
/// generated import retains the source operator import's span so any conflict
/// points at the declaration that induced it.
fn synthetic_qualified_import(from_path: &str, alias: &str, span: Span) -> Import {
    let segments: Vec<PathSegment> = from_path
        .split('/')
        .map(|s| PathSegment {
            name: s.to_owned(),
            span,
        })
        .collect();
    Import {
        trailing_trivia: Vec::new(),
        kind: ImportKind::Qualified {
            path: ModulePath { segments, span },
            alias: alias.to_owned(),
        },
        span,
        leading_trivia: Vec::new(),
    }
}

fn synthetic_builtin_import(kind: ResolvedBuiltinKind, span: Span) -> Import {
    Import {
        trailing_trivia: Vec::new(),
        kind: match kind {
            ResolvedBuiltinKind::Intrinsic => ImportKind::Intrinsics,
            ResolvedBuiltinKind::Comptime => ImportKind::Comptime,
        },
        span,
        leading_trivia: Vec::new(),
    }
}

fn fold_item(item: &mut Item<Surface>, scope: &OpScope) -> Result<(), Error> {
    match item {
        Item::FnDef(d) => fold_fn_def(d, scope),
        Item::TypeAlias(a) => fold_type_alias(a, scope),
        Item::LiteralAlias(_, _) => Ok(()),
        Item::Newtype(_) => Ok(()),
        Item::Labels(d, _) => fold_labels(d, scope),
        Item::LabelForward(_, _) => Ok(()),
        Item::Equiv(e, _) => fold_equiv(e, scope),
        Item::Elaborator(s, _) => {
            fold_type(&mut s.call_ty, scope)?;
            Ok(())
        }
        Item::RecGroup(g, _) => {
            for member in g.members.iter_mut() {
                fold_fn_def(member, scope)?;
            }
            Ok(())
        }
        Item::TypeRecGroup(group) => {
            for member in &mut group.members {
                match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => {
                        fold_type_alias(alias, scope)?;
                    }
                    crate::ast::TypeRecMember::Newtype(_) => {}
                    crate::ast::TypeRecMember::Labels(labels, _) => {
                        fold_labels(labels, scope)?;
                    }
                }
            }
            Ok(())
        }
        Item::Op(d, _) => fold_op(d, scope),
        Item::VariadicOperator(d, _) => fold_variadic_declaration(d, scope),
        // Host items are signature-only — no value expressions, so no
        // operator chains to fold.
        Item::HostType(_) | Item::HostFn(_) => Ok(()),
    }
}

fn fold_fn_def(d: &mut FnDef<Surface>, scope: &OpScope) -> Result<(), Error> {
    fold_signature(&mut d.sig, scope)?;
    fold_type(&mut d.ret, scope)?;
    fold_expr(&mut d.body, scope)
}

fn fold_type_alias(a: &mut TypeAlias<Surface>, scope: &OpScope) -> Result<(), Error> {
    fold_type(&mut a.body, scope)
}

fn fold_labels(d: &mut Labels<Surface>, scope: &OpScope) -> Result<(), Error> {
    for entry in d.entries.iter_mut() {
        fold_type(&mut entry.payload, scope)?;
    }
    Ok(())
}

fn fold_equiv(e: &mut Equiv<Surface>, scope: &OpScope) -> Result<(), Error> {
    fold_signature(&mut e.sig, scope)?;
    for term in e.terms.iter_mut() {
        fold_expr(&mut term.body, scope)?;
    }
    Ok(())
}

fn fold_op(_d: &mut Op<Surface>, _scope: &OpScope) -> Result<(), Error> {
    Ok(())
}

fn fold_variadic_declaration(
    _d: &mut VariadicOperator<Surface>,
    _scope: &OpScope,
) -> Result<(), Error> {
    Ok(())
}

fn fold_signature(sig: &mut Signature<Surface>, scope: &OpScope) -> Result<(), Error> {
    for p in sig.params.iter_mut() {
        if let SignatureParam::Value(v) = p
            && let Some(ty) = v.ty.as_mut()
        {
            fold_type(ty, scope)?;
        }
    }
    Ok(())
}

fn fold_type(ty: &mut Type<Surface>, scope: &OpScope) -> Result<(), Error> {
    // Types don't contain operator usages — operator-fold operates
    // on value-position expressions only.
    let _ = (ty, scope);
    Ok(())
}

fn fold_expr(expr: &mut Expr<Surface>, scope: &OpScope) -> Result<(), Error> {
    match expr {
        Expr::BlockCall { prefix, blocks, .. } => {
            for value in prefix {
                fold_expr(value, scope)?;
            }
            for block in blocks {
                for item in &mut block.items {
                    let value = match item {
                        crate::ast::NeutralItem::Expression { value, .. }
                        | crate::ast::NeutralItem::Binding { value, .. }
                        | crate::ast::NeutralItem::RowBinding { value, .. }
                        | crate::ast::NeutralItem::ExistentialBinding { value, .. } => value,
                    };
                    fold_expr(value, scope)?;
                }
            }
        }
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::StrLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::Call { callee, args, .. } => {
            fold_expr(callee, scope)?;
            for arg in args.iter_mut() {
                if let CallArg::Value(v) = arg {
                    fold_expr(v, scope)?;
                }
            }
        }
        Expr::RecCall { args, .. } => {
            for arg in args.iter_mut() {
                if let CallArg::Value(v) = arg {
                    fold_expr(v, scope)?;
                }
            }
        }
        Expr::FnExpr {
            sig, ret_ty, body, ..
        } => {
            fold_signature(sig, scope)?;
            if let Some(ty) = ret_ty.as_mut() {
                fold_type(ty, scope)?;
            }
            fold_expr(body, scope)?;
        }
        Expr::FnPlaceholder {
            stem,
            state,
            body,
            meta,
            ..
        } => {
            crate::pass::placeholder::prepare(stem, state, body, meta.span)?;
            fold_expr(body, scope)?;
        }
        Expr::Let { value, body, .. } => {
            fold_expr(value, scope)?;
            fold_expr(body, scope)?;
        }
        Expr::RowLet { value, body, .. } => {
            fold_expr(value, scope)?;
            fold_expr(body, scope)?;
        }
        Expr::Seq { value, body, .. } => {
            fold_expr(value, scope)?;
            fold_expr(body, scope)?;
        }
        Expr::Tuple { items, .. } => {
            for item in items.iter_mut() {
                fold_expr(item, scope)?;
            }
        }
        Expr::LabelValue { labels, .. } => {
            for LabelValueLabel { value, .. } in labels.iter_mut() {
                fold_expr(value, scope)?;
            }
        }
        Expr::Elaborator { call, .. } => match call {
            ElaboratorCall::FieldAccess { receiver, .. } => {
                fold_expr(receiver, scope)?;
            }
            ElaboratorCall::FieldUpdate { receiver, updates } => {
                fold_expr(receiver, scope)?;
                for update in updates.iter_mut() {
                    fold_expr(&mut update.value, scope)?;
                }
            }
        },
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { args, .. } => {
            for arg in args {
                match arg {
                    CallArg::Value(value) => fold_expr(value, scope)?,
                    CallArg::Type(ty) => fold_type(ty, scope)?,
                }
            }
        }
        Expr::Ufcs { receiver, args, .. } => {
            fold_expr(receiver, scope)?;
            for arg in args.iter_mut() {
                if let CallArg::Value(v) = arg {
                    fold_expr(v, scope)?;
                }
            }
        }
        Expr::OpChain { .. } => {
            // Recurse into the chain's sub-expressions before
            // resolving the chain itself.
            if let Expr::OpChain { kind, .. } = expr {
                match kind {
                    OpChainKind::Normal { slots, .. } => {
                        for s in slots.iter_mut() {
                            fold_expr(s, scope)?;
                        }
                    }
                    OpChainKind::Variadic { elements, .. } => {
                        for el in elements.iter_mut() {
                            fold_expr(el, scope)?;
                        }
                    }
                }
            }
            *expr = resolve_op_chain(std::mem::replace(expr, placeholder_unit()), scope)?;
        }
        // Statically uninhabited at `Surface` — the enriched
        // structural variants are produced post-typecheck.
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. } => match *ext {},
        Expr::LowHostCall { ext, .. }
        | Expr::LowModuleCall { ext, .. }
        | Expr::LowQualifiedModuleCall { ext, .. }
        | Expr::LowQualifiedNewtypeMember { ext, .. }
        | Expr::LowNewtypeCtor { ext, .. }
        | Expr::LowNewtypeProj { ext, .. }
        | Expr::LowClosureCall { ext, .. }
        | Expr::LowIndirectCall { ext, .. }
        | Expr::LowTypeApplication { ext, .. }
        | Expr::LowAbsurdCall { ext, .. }
        | Expr::LowCpsProjectorApply { ext, .. }
        | Expr::LowBoundRef { ext, .. }
        | Expr::LowHostFnValueRef { ext, .. }
        | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
    }
    Ok(())
}

fn placeholder_unit() -> Expr<Surface> {
    Expr::Unit {
        occurrence: Default::default(),
        meta: Meta::new(Span::new(0, 0)),
    }
}

fn resolve_op_chain(expr: Expr<Surface>, scope: &OpScope) -> Result<Expr<Surface>, Error> {
    let Expr::OpChain {
        occurrence: _,
        kind,
        meta,
        ext: _,
    } = expr
    else {
        unreachable!("resolve_op_chain called with non-OpChain expression");
    };
    match kind {
        OpChainKind::Normal { pattern, slots } => {
            let prefix = matches!(pattern.first(), Some(OpPart::Token { .. }));
            let tokens = leading_op_run(&pattern);
            let Some(binding) = scope.normal.get(&(tokens.clone(), prefix)) else {
                return Err(Error::parse(
                    meta.span,
                    format!(
                        "operator `{}` is not in scope at this position",
                        crate::ast::OperatorGrammar::fixed(&pattern).render()
                    ),
                ));
            };
            let pattern_slot_count = binding
                .pattern
                .iter()
                .filter(|p| {
                    matches!(
                        p,
                        OpPart::SlotPlain { .. }
                            | OpPart::SlotRecursive { .. }
                            | OpPart::SlotGreedy { .. }
                    )
                })
                .count();
            if slots.len() != pattern_slot_count {
                return Err(Error::parse(
                    meta.span,
                    format!(
                        "operator `{}` expects {pattern_slot_count} operand(s); got {}",
                        crate::ast::OperatorGrammar::fixed(&pattern).render(),
                        slots.len()
                    ),
                ));
            }
            let callee = path_to_expr(&binding.function, meta.span);
            let args: Vec<CallArg<Surface>> = slots.into_iter().map(CallArg::Value).collect();
            Ok(Expr::Call {
                occurrence: Default::default(),
                callee: Box::new(callee),
                args,
                meta,
                ext: (),
            })
        }
        OpChainKind::Variadic {
            open_tokens,
            close_tokens,
            elements,
            ..
        } => {
            let Some(binding) = scope.variadic.get(&open_tokens) else {
                return Err(Error::parse(
                    meta.span,
                    format!(
                        "operator `{}` is not in scope at this position",
                        crate::ast::OperatorGrammar::Variadic {
                            open: open_tokens,
                            close: close_tokens,
                        }
                        .render()
                    ),
                ));
            };
            build_variadic_fold(&binding.spec, elements, meta.span)
        }
    }
}

fn build_variadic_fold(
    spec: &VariadicSpec,
    mut elements: Vec<Expr<Surface>>,
    span: Span,
) -> Result<Expr<Surface>, Error> {
    // Inferred call arguments are keyed by source span and slot. A fold's
    // opening-token position is unique in the module, while the element
    // ends within that fold are distinct, so prefix spans keep synthesized
    // calls disjoint even when a recursively folded element has inherited
    // an inner call's end. The identity depends only on use-site tokens, not
    // declarations.
    let base_span = Span::new(span.start, span.start);
    let requires_element = spec.mode.requires_element();
    if elements.is_empty() {
        if requires_element {
            return Err(Error::parse(
                span,
                format!(
                    "`{}` requires at least one complete element",
                    spec.mode.keyword()
                ),
            ));
        }
        let call = callable_spec_to_call(&spec.initializer, vec![], base_span);
        return Ok(finalize_variadic(spec, call, span));
    }

    let (base_args, base_span) = if !requires_element {
        (Vec::new(), base_span)
    } else {
        let seed = if spec.mode.is_right() {
            elements
                .pop()
                .unwrap_or_else(|| unreachable!("non-empty right fold has a final element"))
        } else {
            elements.remove(0)
        };
        let seed_span = Span::new(span.start, seed.span().end);
        (vec![seed], seed_span)
    };
    let base_call = callable_spec_to_call(&spec.initializer, base_args, base_span);
    let mut acc = base_call;
    let iter: Box<dyn Iterator<Item = Expr<Surface>>> = if spec.mode.is_right() {
        Box::new(elements.into_iter().rev())
    } else {
        Box::new(elements.into_iter())
    };
    for element in iter {
        let step_span = Span::new(span.start, element.span().end);
        let step_args = if spec.mode.is_right() {
            vec![element, acc]
        } else {
            vec![acc, element]
        };
        acc = callable_spec_to_call(&spec.step, step_args, step_span);
    }
    Ok(finalize_variadic(spec, acc, span))
}

fn finalize_variadic(spec: &VariadicSpec, body: Expr<Surface>, span: Span) -> Expr<Surface> {
    match &spec.finalize {
        None => body,
        Some(fin) => callable_spec_to_call(fin, vec![body], span),
    }
}

fn callable_spec_to_call(
    spec: &CallableSpec,
    args: Vec<Expr<Surface>>,
    span: Span,
) -> Expr<Surface> {
    let callee = path_to_expr(&spec.path, span);
    let call_args: Vec<CallArg<Surface>> = args.into_iter().map(CallArg::Value).collect();
    Expr::Call {
        occurrence: Default::default(),
        callee: Box::new(callee),
        args: call_args,
        meta: Meta::new(span),
        ext: (),
    }
}

fn path_to_expr(function: &LexicalCallablePath, span: Span) -> Expr<Surface> {
    Expr::Path {
        occurrence: Default::default(),
        segments: function
            .segments()
            .iter()
            .map(|segment| PathSegment::synth(segment.name.clone(), span))
            .collect(),
        meta: Meta::new(span),
        ext: (),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn import_grammar_four_modes_ordinary_pair_nesting() {
        fn call_tree(expr: &Expr<Surface>, aliases: &BTreeMap<String, String>) -> String {
            match expr {
                Expr::Path { segments, .. } if segments.len() == 1 => {
                    let name = &segments[0].name;
                    assert!(
                        ["a", "b", "c", "d", "e", "f"].contains(&name.as_str()),
                        "bare callable must not replace a resolved owner"
                    );
                    name.clone()
                }
                Expr::Path { segments, .. } => {
                    let owner = aliases
                        .get(&segments[0].name)
                        .expect("ordinary qualified import owns the callable path");
                    format!(
                        "{owner}.{}",
                        segments[1..]
                            .iter()
                            .map(|part| part.name.as_str())
                            .collect::<Vec<_>>()
                            .join(".")
                    )
                }
                Expr::Call { callee, args, .. } => format!(
                    "{}({})",
                    call_tree(callee, aliases),
                    args.iter()
                        .map(|arg| call_tree(value_arg(arg), aliases))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                other => panic!("expected only named calls/slots, got {other:?}"),
            }
        }
        let packets = [("a", "b"), ("c", "d"), ("e", "f")];
        let mut admitted = 0;
        let mut rejected_empty = 0;
        for mode in ["foldl", "foldr", "foldl1", "foldr1"] {
            let right = mode.starts_with("foldr");
            let seeded = mode.ends_with('1');
            for count in [0, 1, 3] {
                for finalize in [false, true] {
                    let initializer = if seeded { "seed" } else { "base" };
                    let finalizer = if finalize { " finalize finish;" } else { "" };
                    let provider = p(&format!(
                        "module syntax;
                        pub fn base() -> . & . {{ ((), ()) }}
                        pub fn seed(entry: . & .) -> . & . {{ entry }}
                        pub fn step(a: . & ., b: . & .) -> . & . {{ a }}
                        pub fn finish(value: . & .) -> (. & .) & . & . {{ (value, value) }}
                        pub fn entry(key: ., value: .) -> . & . {{ (key, value) }}
                        pub varop [% %] {{ {mode} step {initializer};{finalizer} }};
                        pub op _ => _ {{ impl entry; }};"
                    ));
                    let literal = packets[..count]
                        .iter()
                        .map(|(key, value)| format!("{key} => {value}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let mut consumer = p(&format!(
                        "module consumer;
                        import syntax(varop [% %], op _ => _);
                        fn run(a: ., b: ., c: ., d: ., e: ., f: .) -> {} {{ [% {literal} %] }}",
                        if finalize { "(. & .) & . & ." } else { ". & ." }
                    ));
                    let modules = [
                        (PathBuf::from("syntax.kio"), provider),
                        (PathBuf::from("consumer.kio"), consumer.clone()),
                    ];
                    let scope = PackageOpScope::from_modules_named(&modules, None)
                        .expect("ordinary resolved callables");
                    let ResolvedPubOpRecordBody::VariadicOperator {
                        base_target,
                        step_target,
                        finalize_target,
                        ..
                    } = &scope.by_module["syntax"][0].body
                    else {
                        panic!("resolved variadic export")
                    };
                    for (target, member) in [(base_target, initializer), (step_target, "step")] {
                        let ResolvedCallableTarget::Module {
                            owner,
                            member: resolved,
                            ..
                        } = target
                        else {
                            panic!("ordinary module callable")
                        };
                        assert_eq!(module_path_str(owner), "syntax");
                        assert_eq!(resolved, &[member]);
                    }
                    if finalize {
                        let ResolvedCallableTarget::Module { owner, member, .. } =
                            finalize_target.as_deref().unwrap()
                        else {
                            panic!("ordinary finalizer")
                        };
                        assert_eq!(module_path_str(owner), "syntax");
                        assert_eq!(member, &["finish"]);
                    } else {
                        assert!(finalize_target.is_none());
                    }
                    let result = fold_module(&mut consumer, &scope);
                    if seeded && count == 0 {
                        assert!(result.is_err(), "{mode} requires an element to seed");
                        rejected_empty += 1;
                        continue;
                    }
                    result.expect("fold attached after syntax");
                    let mut indices: Vec<usize> = (0..count).collect();
                    let mut expected = if seeded {
                        let index = if right {
                            indices.pop().unwrap()
                        } else {
                            indices.remove(0)
                        };
                        let (key, value) = packets[index];
                        format!("syntax.seed(syntax.entry({key},{value}))")
                    } else {
                        "syntax.base()".into()
                    };
                    if right {
                        indices.reverse();
                    }
                    for index in indices {
                        let (key, value) = packets[index];
                        expected = if right {
                            format!("syntax.step(syntax.entry({key},{value}),{expected})")
                        } else {
                            format!("syntax.step({expected},syntax.entry({key},{value}))")
                        };
                    }
                    if finalize {
                        expected = format!("syntax.finish({expected})");
                    }
                    let Item::FnDef(run) = &consumer.items[0] else {
                        panic!("run")
                    };
                    let aliases: BTreeMap<String, String> = consumer
                        .imports
                        .iter()
                        .filter_map(|import| match &import.kind {
                            ImportKind::Qualified { path, alias } => {
                                Some((alias.clone(), module_path_str(path)))
                            }
                            _ => None,
                        })
                        .collect();
                    assert_eq!(aliases.len(), 1);
                    let actual = call_tree(&run.body, &aliases);
                    assert_eq!(
                        actual, expected,
                        "mode={mode} count={count} finalize={finalize}"
                    );
                    assert_eq!(actual.matches("finish(").count(), usize::from(finalize));
                    admitted += 1;
                }
            }
        }
        assert_eq!((admitted, rejected_empty), (20, 4));
        eprintln!(
            "fold attachment: cells=24 admitted=20 empty_rejected=4; runtime typing/execution not claimed"
        );
    }
    use super::*;
    use crate::error::Diagnostic;
    use crate::pass::parser::{parse, parse_module_file};

    fn p(src: &str) -> Module<Surface> {
        parse(src).expect("parse")
    }

    fn pf(src: &str) -> Module<Surface> {
        parse_module_file(src).expect("parse module file").module
    }

    fn fold(module: Module<Surface>) -> Module<Surface> {
        let modules = [(PathBuf::from("module.kio"), module.clone())];
        let pkg = PackageOpScope::from_modules_named(&modules, None)
            .expect("operator declarations validate");
        let mut m = module;
        fold_module(&mut m, &pkg).expect("fold_module");
        m
    }

    #[test]
    fn synthetic_alias_is_collision_safe_and_injective() {
        let mut occupied = BTreeSet::new();
        let slash = allocate_synthetic_alias("a/b", &mut occupied);
        let underscore = allocate_synthetic_alias("a_b", &mut occupied);
        assert_eq!(slash, "_op_gbcpgc__");
        assert_eq!(underscore, "_op_gbfpgc__");
        assert_ne!(slash, underscore);

        occupied.insert("_op_gn__".to_owned());
        assert_eq!(allocate_synthetic_alias("m", &mut occupied), "_op_gn_n2__");
    }

    fn fold_error(src: &str) -> String {
        let mut module = p(src);
        let modules = [(PathBuf::from("module.kio"), module.clone())];
        let package = match PackageOpScope::from_modules_named(&modules, None) {
            Ok(package) => package,
            Err((_, Error::Type(Diagnostic { message, .. }))) => return message,
            Err((_, other)) => panic!("expected type error, got {other:?}"),
        };
        match fold_module(&mut module, &package).expect_err("operator declaration must fail") {
            Error::Type(Diagnostic { message, .. }) => message,
            other => panic!("expected type error, got {other:?}"),
        }
    }

    fn validate_against_modules(
        module: &Module<Surface>,
        providers: &[Module<Surface>],
    ) -> Result<(), Error> {
        let modules = providers
            .iter()
            .chain(std::iter::once(module))
            .cloned()
            .enumerate()
            .map(|(index, module)| (PathBuf::from(format!("module-{index}.kio")), module))
            .collect::<Vec<_>>();
        let package = PackageOpScope::resolution_index(&modules);
        validate_callable_decls(module, &package)
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn package_scope_reuses_validated_callable_declaration_facts() {
        let module = p("module x; \
             fn add[A](left: A, right: A) -> A { left } \
             op _ + _ { impl add; };");
        let scope = PackageOpScope::from_modules_named(&[(PathBuf::from("x.kio"), module)], None)
            .expect("callable declarations validate");

        assert!(
            scope
                .try_resolved_callable_declarations(None, || true)
                .is_none(),
            "fact projection must remain cancellable"
        );
        assert!(
            scope
                .try_resolved_callable_declarations(Some("other"), || false)
                .expect("uncancelled projection")
                .is_empty()
        );
        let facts = scope
            .try_resolved_callable_declarations(Some("x"), || false)
            .expect("uncancelled projection");
        assert_eq!(facts.len(), 1);
        assert_eq!(render_callable_path(&facts[0].source_path), "add");
        assert!(matches!(
            &facts[0].target,
            ResolvedCallableTarget::Module { owner, member, .. }
                if module_path_str(owner) == "x" && member == &["add".to_owned()]
        ));
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn alias_member_callable_facts_keep_source_head_and_terminal_member_identities() {
        let origin = p("module origin; \
             pub newtype Terminal : . { pub constructor make; pub projector open; };");
        let relay = p("module relay; import origin as source; \
             pub type Alias = source.Terminal;");
        let consumer = p("module consumer; import relay as r; \
             op + _ { impl r.Alias.make; }; \
             varop [* *] { foldr r.Alias.open r.Alias.make; }; \
             elab demo : . -> . { impl r.Alias.make; };");
        let scope = PackageOpScope::from_modules_named(
            &[
                (PathBuf::from("origin.kio"), origin),
                (PathBuf::from("relay.kio"), relay),
                (PathBuf::from("consumer.kio"), consumer),
            ],
            None,
        )
        .expect("all declaration callable paths resolve through the identity alias");
        let facts = scope
            .try_resolved_callable_declarations(Some("consumer"), || false)
            .expect("fact projection");
        assert_eq!(facts.len(), 4);
        for fact in facts {
            assert!(matches!(
                fact.target,
                ResolvedCallableTarget::Module {
                    owner,
                    member,
                    source_type: Some(ResolvedCallableTypeHead::TypeAlias {
                        owner: source_owner,
                        name,
                    }),
                } if module_path_str(&owner) == "origin"
                    && member.first().is_some_and(|head| head == "Terminal")
                    && module_path_str(&source_owner) == "relay"
                    && name == "Alias"
            ));
        }
    }

    #[test]
    fn operator_target_routing_leaves_elaborator_schedules_and_paths_unchanged() {
        let mut module = p("module x; \
             pure fn apply(left: ., right: .) -> . { left } \
             elab late : . -> . { impl apply; }; \
             elab fills : . -> . { impl(fills) apply; }; \
             op _ + _ { impl apply; }; \
             fn run(apply: .) -> . { () + () }");
        let package =
            PackageOpScope::from_modules_named(&[(PathBuf::from("x.kio"), module.clone())], None)
                .expect("declaration callables validate");
        fold_module(&mut module, &package).expect("operator lowers");

        let elaborators = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Elaborator(elaborator, _) => Some(elaborator),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(elaborators.len(), 2);
        assert_eq!(
            elaborators[0].schedule,
            crate::ast::ElaboratorSchedule::Late
        );
        assert_eq!(
            elaborators[1].schedule,
            crate::ast::ElaboratorSchedule::Fills
        );
        for elaborator in elaborators {
            assert_eq!(render_callable_path(&elaborator.implementation), "apply");
        }
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn focused_callable_projection_visits_only_the_selected_module_bucket() {
        let mut modules = (0..32)
            .map(|index| {
                let name = format!("unrelated{index}");
                let source = format!(
                    "module {name}; fn add(left: ., right: .) -> . {{ left }} \
                     op _ + _ {{ impl add; }};"
                );
                (PathBuf::from(format!("{name}.kio")), p(&source))
            })
            .collect::<Vec<_>>();
        modules.push((
            PathBuf::from("target.kio"),
            p("module target; fn add(left: ., right: .) -> . { left } \
               op _ + _ { impl add; };"),
        ));
        let scope = PackageOpScope::from_modules_named(&modules, None)
            .expect("callable declarations validate");
        let polls = std::cell::Cell::new(0usize);
        let facts = scope
            .try_resolved_callable_declarations(Some("target"), || {
                polls.set(polls.get() + 1);
                false
            })
            .expect("focused projection is not cancelled");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].source_module, "target");
        assert_eq!(
            polls.get(),
            1,
            "focused projection must not visit unrelated package-wide facts"
        );
    }

    fn validation_message(error: Error) -> String {
        match error {
            Error::Type(Diagnostic { message, .. }) => message,
            other => panic!("expected type error, got {other:?}"),
        }
    }

    fn expect_name_error(result: Result<(), Error>) -> String {
        match result.expect_err("validation must fail") {
            Error::NameRes(Diagnostic { message, .. }) => message,
            other => panic!("expected name-resolution error, got {other:?}"),
        }
    }

    #[test]
    fn public_operator_rejects_private_function_implementation() {
        let message = fold_error(
            "module x; \
             fn add(a: A, b: A) -> A { a } \
             pub op _ + _ { impl add; };",
        );
        assert!(
            message.contains("function `add` must be at least as visible as operator `op _ + _`"),
            "got: {message}"
        );
    }

    #[test]
    fn private_operator_accepts_private_function_implementation() {
        fold(p("module x; \
             fn add(a: A, b: A) -> A { a } \
             op _ + _ { impl add; };"));
    }

    #[test]
    fn public_operator_rejects_private_newtype_member_implementation() {
        let message = fold_error(
            "module x; \
             pub newtype Box[A] : A { constructor mk; pub projector get; }; \
             pub op + _ { impl Box.mk; };",
        );
        assert!(
            message.contains(
                "newtype member `Box.mk` must be at least as visible as operator `op + _`"
            ),
            "got: {message}"
        );
    }

    #[test]
    fn declaration_callable_visibility_names_alias_heads_separately_from_newtypes() {
        let provider = p("module origin; \
             pub newtype Token : . { pub constructor make; pub projector open; };");
        for declaration in [
            "pub op + _ { impl Tag.make; };",
            "pub varop [* *] { foldr Tag.open Tag.make; };",
        ] {
            let consumer = p(&format!(
                "module consumer; import origin as imported; \
                 type Tag = imported.Token; {declaration}"
            ));
            let message = validation_message(
                validate_against_modules(&consumer, std::slice::from_ref(&provider))
                    .expect_err("a private alias cannot support a public callable declaration"),
            );
            assert!(
                message.contains("type alias `Tag` must be at least as visible"),
                "the source visibility edge must name the written alias: {message}"
            );
        }

        let literal = fold_error(
            "module literal; \
             newtype Token : . { pub constructor make; pub projector open; }; \
             pub op + _ { impl Token.make; };",
        );
        assert!(
            literal.contains("newtype `Token` must be at least as visible"),
            "a literal newtype head keeps its existing diagnostic vocabulary: {literal}"
        );
    }

    #[test]
    fn public_fold_checks_every_named_callable_visibility() {
        let message = fold_error(
            "module x; \
             pub fn nil() -> A { let r = (); r } \
             pub fn step(x: A, acc: A) -> A { x } \
             fn finish(x: A) -> A { x } \
             pub varop [* *] { foldr step nil; finalize finish; };",
        );
        assert!(
            message.contains(
                "function `finish` must be at least as visible as operator `varop [* *]`"
            ),
            "got: {message}"
        );
    }

    #[test]
    fn qualified_alias_checks_the_actual_function_leaf() {
        let provider = p("module a/b; \
             fn step(a: A, b: A) -> A { a }");
        let consumer = p("module x; \
             import a/b as provider; \
             pub op _ + _ { impl provider.step; };");
        let message = validation_message(
            validate_against_modules(&consumer, &[provider])
                .expect_err("private alias-qualified endpoint must fail visibility validation"),
        );
        assert!(
            message.contains("function `step` must be at least as visible as operator `op _ + _`"),
            "got: {message}"
        );
    }

    #[test]
    fn qualified_alias_accepts_visible_function() {
        let provider = p("module a/b; \
             pub fn step(a: A, b: A) -> A { a }");
        let consumer = p("module x; \
             import a/b as provider; \
             pub op _ + _ { impl provider.step; };");
        validate_against_modules(&consumer, &[provider]).expect("visible alias-qualified endpoint");
    }

    #[test]
    fn qualified_alias_function_is_validated_at_its_target() {
        let provider = p("module a/b; \
             fn step(a: A, b: A) -> A { a }");
        let consumer = p("module x; \
             import a/b as h; \
             pub op _ + _ { impl h.step; };");
        let message = validation_message(
            validate_against_modules(&consumer, &[provider])
                .expect_err("private alias-qualified endpoint must fail"),
        );
        assert!(message.contains("function `step`"), "got: {message}");
    }

    #[test]
    fn selective_function_import_is_validated_at_its_target() {
        let provider = p("module a/b; \
             fn step(a: A, b: A) -> A { a }");
        let consumer = p("module x; \
             import a/b(step); \
             pub op _ + _ { impl step; };");
        let message = validation_message(
            validate_against_modules(&consumer, &[provider])
                .expect_err("private selectively imported endpoint must fail"),
        );
        assert!(message.contains("function `step`"), "got: {message}");
    }

    #[test]
    fn every_declaration_callable_position_accepts_each_lexical_path_shape() {
        for (label, provider, declarations, target) in [
            (
                "bare local",
                None,
                "fn target(a: A, b: A) -> A { a }",
                "target",
            ),
            (
                "selectively imported",
                Some("module provider; pub fn target(a: A, b: A) -> A { a }"),
                "import provider(target);",
                "target",
            ),
            (
                "module alias",
                Some("module provider; pub fn target(a: A, b: A) -> A { a }"),
                "import provider as helpers;",
                "helpers.target",
            ),
            (
                "local newtype member",
                None,
                "newtype Box[A] : A { constructor mk; projector get; };",
                "Box.mk",
            ),
            (
                "alias-qualified newtype member",
                Some(
                    "module provider; \
                     pub newtype Box[A] : A { pub constructor mk; pub projector get; };",
                ),
                "import provider as helpers;",
                "helpers.Box.mk",
            ),
        ] {
            let consumer_source = format!(
                "module consumer; \
                 {declarations} \
                 op _ + _ {{ impl {target}; }}; \
                 varop [* *] {{ \
                   foldr {target} {target}; finalize {target}; \
                 }};"
            );
            let consumer = p(&consumer_source);
            let providers = provider.into_iter().map(p).collect::<Vec<_>>();
            validate_against_modules(&consumer, &providers).unwrap_or_else(|error| {
                panic!("{label} should resolve in every position: {error:?}")
            });
        }
    }

    #[test]
    fn callable_resolution_does_not_search_unimported_modules() {
        let coincidental = p("module provider; pub fn target(a: A, b: A) -> A { a }");
        let consumer = p("module consumer; op _ + _ { impl target; };");
        let message = expect_name_error(validate_against_modules(&consumer, &[coincidental]));
        assert!(
            message.contains("target") && message.contains("does not resolve"),
            "unimported declarations must not become ambient candidates: {message}"
        );
    }

    #[test]
    fn unknown_operator_implementation_endpoint_is_rejected() {
        let provider = p("module a/b; pub fn other(a: A) -> A { a }");
        let consumer = p("module x; import a/b as provider; op + _ { impl provider.missing; };");
        let message = expect_name_error(validate_against_modules(&consumer, &[provider]));
        assert!(
            message.contains(
                "operator `op + _` implementation `provider.missing` does not resolve to a function or newtype member"
            ),
            "got: {message}"
        );
    }

    #[test]
    fn unknown_newtype_member_is_rejected() {
        let consumer = p("module x; \
             newtype Box[A] : A { pub constructor mk; pub projector get; }; \
             op + _ { impl Box.missing; };");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`Box.missing` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn operator_rejects_later_function_implementation() {
        let consumer = p("module x; \
             op + _ { impl later; }; \
             fn later(a: A) -> A { a }");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`later` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn operator_rejects_unimported_dotted_head_even_when_it_matches_the_module_name() {
        let consumer = p("module x; \
             op + _ { impl x.later; }; \
             fn later(a: A) -> A { a }");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`x.later` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn operator_rejects_later_self_alias_qualified_function_implementation() {
        let consumer = p("module x; \
             import x as self_module; \
             op + _ { impl self_module.later; }; \
             fn later(a: A) -> A { a }");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`self_module.later` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn fold_rejects_later_rec_member_implementation() {
        let consumer = p("module x; \
             varop [* *] { foldr cons nil; }; \
             rec(loop) { \
               fn nil() -> A { () }; \
               fn cons(a: A, b: A) -> A { a } \
             }");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(message.contains("`nil` does not resolve"), "got: {message}");
    }

    #[test]
    fn operator_rejects_later_newtype_member_implementation() {
        let consumer = p("module x; \
             op + _ { impl Box.mk; }; \
             newtype Box[A] : A { constructor mk; projector get; };");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`Box.mk` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn fold_rejects_later_newtype_member_implementation() {
        let consumer = p("module x; \
             varop [* *] { foldr Box.get Box.mk; }; \
             newtype Box[A] : A { constructor mk; projector get; };");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`Box.mk` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn elaborator_rejects_later_newtype_member_implementation() {
        let consumer = p("module x; \
             elab demo : . -> . { impl Later.mk; }; \
             newtype Later : . { constructor mk; projector get; };");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`Later.mk` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn operator_accepts_earlier_self_alias_qualified_newtype_member() {
        let consumer = p("module x; \
             import x as self_module; \
             newtype Box[A] : A { constructor mk; projector get; }; \
             op + _ { impl self_module.Box.mk; };");
        validate_against_modules(&consumer, &[])
            .expect("self-alias qualification preserves an earlier local newtype member");
    }

    #[test]
    fn operator_rejects_later_self_alias_qualified_newtype_member() {
        let consumer = p("module x; \
             import x as self_module; \
             op + _ { impl self_module.Later.mk; }; \
             newtype Later[A] : A { constructor mk; projector get; };");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`self_module.Later.mk` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn operator_rejects_later_self_selectively_imported_newtype_member() {
        let consumer = p("module x; \
             import x(Later); \
             op + _ { impl Later.mk; }; \
             newtype Later[A] : A { constructor mk; projector get; };");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`Later.mk` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn fold_accepts_earlier_self_alias_qualified_label_member() {
        let consumer = p("module x; \
             import x as self_module; \
             labels { box: A }; \
             varop [* *] { \
               foldr self_module.Box.mk self_module.Box.mk; \
             };");
        validate_against_modules(&consumer, &[])
            .expect("self-alias qualification preserves an earlier labels-generated member");
    }

    #[test]
    fn fold_rejects_later_self_alias_qualified_label_member() {
        let consumer = p("module x; \
             import x as self_module; \
             varop [* *] { \
               foldr self_module.Box.mk self_module.Box.mk; \
             }; \
             labels { box: A };");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`self_module.Box.mk` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn impure_operator_accepts_earlier_local_host_function() {
        let mut consumer = p("module x; \
             host fn host_add(a: A) -> A; \
             op + _ { impl host_add; };");
        let modules = [(PathBuf::from("x.kio"), consumer.clone())];
        let package = PackageOpScope::from_modules_named(&modules, None)
            .expect("operator declaration validates");
        fold_module(&mut consumer, &package).expect("earlier local host fn is a callable target");
    }

    #[test]
    fn impure_operator_rejects_later_local_host_function() {
        let consumer = p("module x; \
             op + _ { impl host_add; }; \
             host fn host_add(a: A) -> A;");
        let modules = [(PathBuf::from("x.kio"), consumer)];
        let (_, error) = PackageOpScope::from_modules_named(&modules, None)
            .err()
            .expect("the local host implementation must precede the operator");
        assert!(
            matches!(error, Error::NameRes(ref diagnostic)
            if diagnostic.message.contains("implementation `host_add` does not resolve")),
            "{error:?}"
        );
    }

    #[test]
    fn host_source_order_callable_rejects_later_host_in_every_slot() {
        for declaration in [
            "op + _ { impl target; };",
            "varop [* *] { foldr earlier target; };",
            "varop [* *] { foldr target earlier; };",
            "varop [* *] { foldr earlier earlier; finalize target; };",
            "elab demo : . -> . { impl target; };",
            "elab demo : . -> . { impl(fills) target; };",
        ] {
            let source =
                format!("module x; host fn earlier() -> .; {declaration} host fn target() -> .;");
            let consumer = p(&source);
            let error = validate_against_modules(&consumer, &[])
                .expect_err("a callable's local host target must precede it");
            let Error::NameRes(diagnostic) = error else {
                panic!("expected name resolution for {declaration}, got {error:?}");
            };
            assert!(
                diagnostic
                    .message
                    .contains("implementation `target` does not resolve")
            );
            let start = u32::try_from(source.find("target").unwrap()).unwrap();
            assert_eq!(diagnostic.span, Span::new(start, start + 6));
        }
    }

    #[test]
    fn host_source_order_callable_preserves_prior_and_imported_hosts() {
        let provider = p("module provider; host fn target() -> .;");
        for declaration in [
            "op + _ { impl target; };",
            "varop [* *] { foldr target target; finalize target; };",
            "elab demo : . -> . { impl target; };",
            "elab demo : . -> . { impl(fills) target; };",
        ] {
            let prior = p(&format!("module x; host fn target() -> .; {declaration}"));
            validate_against_modules(&prior, &[]).expect("preceding host target resolves");
            let imported = p(&format!("module x; import provider(target); {declaration}"));
            validate_against_modules(&imported, std::slice::from_ref(&provider))
                .expect("a selective import retains its host target");
            let qualified = p(&format!(
                "module x; import provider as p; {}",
                declaration.replace("target", "p.target")
            ));
            validate_against_modules(&qualified, std::slice::from_ref(&provider))
                .expect("a qualified import retains its host target");
        }
    }

    #[test]
    fn operator_accepts_private_member_of_earlier_multi_rec_group() {
        let consumer = p("module x; \
             host fn loop[S][R](step: S -> S | R, state: S) -> R; \
             rec(loop) { \
               fn hidden(a: A) -> A { rec other(a) }; \
               fn other(a: A) -> A { rec hidden(a) } \
             } \
             op + _ { impl hidden; };");
        validate_against_modules(&consumer, &[])
            .expect("private rec member remains an ordinary module item");
    }

    #[test]
    fn operator_accepts_public_member_of_earlier_multi_rec_group() {
        let consumer = p("module x; \
             host fn loop[S][R](step: S -> S | R, state: S) -> R; \
             rec(loop) { \
               pub fn visible(a: A) -> A { rec hidden(a) }; \
               fn hidden(a: A) -> A { rec visible(a) } \
             } \
             op + _ { impl visible; };");
        validate_against_modules(&consumer, &[]).expect("public rec member is a callable target");
    }

    #[test]
    fn operator_accepts_private_singleton_rec_function() {
        let consumer = p("module x; \
             host fn loop[S][R](step: S -> S | R, state: S) -> R; \
             rec(loop) fn recursive(a: A) -> A { a } \
             op + _ { impl recursive; };");
        validate_against_modules(&consumer, &[])
            .expect("private singleton rec function remains an ordinary module item");
    }

    #[test]
    fn impure_operator_accepts_imported_host_function() {
        let provider = p("module host; host fn host_add(a: A) -> A;");
        let consumer = p("module x; \
             import host(host_add); \
             op + _ { impl host_add; };");
        validate_against_modules(&consumer, &[provider]).expect("imported host fn target");
    }

    #[test]
    fn operator_accepts_local_label_generated_member() {
        let mut consumer = p("module x; \
             labels { box: A }; \
             op + _ { impl Box.mk; };");
        let modules = [(PathBuf::from("x.kio"), consumer.clone())];
        let package = PackageOpScope::from_modules_named(&modules, None)
            .expect("operator declaration validates");
        fold_module(&mut consumer, &package).expect("labels-generated local member");
    }

    #[test]
    fn operator_rejects_later_local_label_generated_member() {
        let consumer = p("module x; \
             op + _ { impl Box.mk; }; \
             labels { box: A };");
        let message = expect_name_error(validate_against_modules(&consumer, &[]));
        assert!(
            message.contains("`Box.mk` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn operator_accepts_local_recursive_group_newtype_member() {
        let consumer = p("module x; \
             rec { \
               newtype A : (. | B) { constructor mk_a; projector un_a; }; \
               newtype B : (. | A) { constructor mk_b; projector un_b; }; \
             } \
             op + _ { impl A.mk_a; };");
        validate_against_modules(&consumer, &[])
            .expect("an earlier recursive-group newtype member is a callable target");
    }

    #[test]
    fn operator_accepts_imported_recursive_group_newtype_member() {
        let provider = p("module types; \
             rec { \
               pub newtype A : (. | B) { pub constructor mk_a; pub projector un_a; }; \
               pub newtype B : (. | A) { pub constructor mk_b; pub projector un_b; }; \
             }");
        let consumer = p("module x; \
             import types(A); \
             pub op + _ { impl A.mk_a; };");
        validate_against_modules(&consumer, &[provider])
            .expect("an imported recursive-group newtype member is a callable target");
    }

    #[test]
    fn operator_accepts_local_recursive_labels_generated_member() {
        let consumer = p("module x; \
             rec { \
               labels A = { to_b: B }; \
               newtype B : A { constructor mk_b; projector un_b; }; \
             } \
             op + _ { impl To_b.mk; };");
        validate_against_modules(&consumer, &[])
            .expect("a recursive labels entry's generated newtype member is a callable target");
    }

    #[test]
    fn declaration_callables_accept_named_labels_identity_alias_members() {
        let local = p("module local; \
             import local as self_module; \
             labels Tree[A][B] = { branch[A][B]: A & B }; \
             op + _ { impl Tree.mk; }; \
             varop [* *] { foldr Tree.get self_module.Tree.mk; }; \
             elab demo : . -> . { impl self_module.Tree.mk; };");
        validate_against_modules(&local, &[])
            .expect("a named one-arm labels alias exposes its generated nominal member");

        let recursive = p("module recursive; \
             rec { \
               pub labels Tree = { branch: Bar }; \
               pub newtype Bar : Tree { pub constructor mk_bar; pub projector get_bar; }; \
             } \
             pub op + _ { impl Tree.mk; };");
        validate_against_modules(&recursive, &[])
            .expect("a named labels alias inside a recursive type group exposes its member");

        let provider = p("module provider; pub labels Tree = { branch: . };");
        let consumer = p("module consumer; \
             import provider(Tree); \
             import provider as p; \
             pub op + _ { impl Tree.mk; }; \
             pub varop [* *] { foldr Tree.get p.Tree.mk; };");
        validate_against_modules(&consumer, &[provider])
            .expect("selective and qualified imports preserve the named labels alias member");
    }

    #[test]
    fn declaration_callables_reject_nonidentity_named_labels_alias_members() {
        let standalone = p("module standalone; \
             labels Reordered[A][B] = { swapped[B][A]: A & B }; \
             labels Renamed[A] = { renamed_item[B]: B }; \
             labels Partial[A] = { partial_item: . }; \
             op + _ { impl Reordered.mk; };");
        let message = expect_name_error(validate_against_modules(&standalone, &[]));
        assert!(
            message.contains("`Reordered.mk` does not resolve"),
            "got: {message}"
        );

        for (alias, declaration) in [
            ("Renamed", "labels Renamed[A] = { renamed_item[B]: B };"),
            ("Partial", "labels Partial[A] = { partial_item: . };"),
        ] {
            let consumer = p(&format!(
                "module negative; {declaration} op + _ {{ impl {alias}.mk; }};"
            ));
            let message = expect_name_error(validate_against_modules(&consumer, &[]));
            assert!(
                message.contains(&format!("`{alias}.mk` does not resolve")),
                "got: {message}"
            );
        }

        let grouped = p("module grouped; \
             rec { \
               labels Reordered[A][B] = { swapped[B][A]: Peer(B, A) }; \
               newtype Peer[A][B] : Reordered(A, B) { constructor make; projector open; }; \
             } \
             varop [* *] { foldr Reordered.get Reordered.mk; };");
        let message = expect_name_error(validate_against_modules(&grouped, &[]));
        assert!(
            message.contains("`Reordered.mk` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn declaration_callables_reject_later_named_labels_identity_alias_members() {
        let local = p("module local; \
             import local as self_module; \
             op + _ { impl Tree.mk; }; \
             labels Tree = { branch: . };");
        let message = expect_name_error(validate_against_modules(&local, &[]));
        assert!(
            message.contains("`Tree.mk` does not resolve"),
            "got: {message}"
        );

        let qualified = p("module local; \
             import local as self_module; \
             op + _ { impl self_module.Tree.mk; }; \
             labels Tree = { branch: . };");
        let message = expect_name_error(validate_against_modules(&qualified, &[]));
        assert!(
            message.contains("`self_module.Tree.mk` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn operator_accepts_imported_label_generated_member() {
        let provider = p("module labels; pub labels { box: A };");
        let consumer = p("module x; \
             import labels(Box); \
             op + _ { impl Box.mk; };");
        validate_against_modules(&consumer, &[provider]).expect("labels-generated imported member");
    }

    #[test]
    fn operator_and_fold_accept_identity_alias_newtype_members() {
        let origin = p("module origin; \
             pub newtype Box[A] : A { pub constructor make; pub projector open; };");
        let relay = p("module relay; \
             import origin as imported; \
             pub type Box[A] = imported.Box(A);");
        let consumer = p("module x; \
             import relay(Box); \
             op + _ { impl Box.make; }; \
             varop [* *] { foldr Box.open Box.make; };");
        validate_against_modules(&consumer, &[origin, relay])
            .expect("op and fold callables resolve through an imported identity alias");

        let local = p("module local; \
             import origin as imported; \
             import local as self_module; \
             type Box[A] = imported.Box(A); \
             op + _ { impl Box.make; }; \
             op - _ { impl self_module.Box.make; }; \
             elab demo : . -> . { impl self_module.Box.make; };");
        validate_against_modules(
            &local,
            &[p("module origin; \
             pub newtype Box[A] : A { pub constructor make; pub projector open; };")],
        )
        .expect("an earlier local identity alias exposes the terminal member");
    }

    #[test]
    fn declaration_callable_identity_alias_requires_a_written_import_edge() {
        let provider = p("module provider; \
             pub newtype Tag : . { pub constructor make; pub projector open; };");
        let consumer = p("module consumer; \
             type Alias = provider.Tag; \
             op + _ { impl Alias.make; };");
        let message = expect_name_error(validate_against_modules(&consumer, &[provider]));
        assert!(
            message.contains("`Alias.make` does not resolve"),
            "adding an unrelated same-spelling module must not create an import edge: {message}"
        );
    }

    #[test]
    fn operator_rejects_later_or_nonidentity_alias_member_heads() {
        let origin = p("module origin; \
             pub newtype Pair[A][B] : A & B { pub constructor make; pub projector open; };");
        let later = p("module x; \
             import origin as imported; \
             op + _ { impl Later.make; }; \
             type Later[A][B] = imported.Pair(A, B);");
        let message = expect_name_error(validate_against_modules(
            &later,
            std::slice::from_ref(&origin),
        ));
        assert!(
            message.contains("`Later.make` does not resolve"),
            "got: {message}"
        );
        let later_qualified = p("module x; \
             import origin as imported; \
             import x as self_module; \
             op + _ { impl self_module.Later.make; }; \
             type Later[A][B] = imported.Pair(A, B);");
        let message = expect_name_error(validate_against_modules(
            &later_qualified,
            std::slice::from_ref(&origin),
        ));
        assert!(
            message.contains("`self_module.Later.make` does not resolve"),
            "got: {message}"
        );

        let alias_body_qualified = p("module x; \
             import x as self_module; \
             type Alias = self_module.Later; \
             op + _ { impl Alias.make; }; \
             newtype Later : . { constructor make; projector open; };");
        let message = expect_name_error(validate_against_modules(&alias_body_qualified, &[]));
        assert!(
            message.contains("`Alias.make` does not resolve"),
            "a self-qualified alias body cannot import its later local target: {message}"
        );

        let alias_body_selective = p("module x; \
             import x(Later); \
             type Alias = Later; \
             op + _ { impl Alias.make; }; \
             newtype Later : . { constructor make; projector open; };");
        let message = expect_name_error(validate_against_modules(&alias_body_selective, &[]));
        assert!(
            message.contains("`Alias.make` does not resolve"),
            "a selectively self-imported alias body cannot import its later local target: {message}"
        );

        let alias_body_in_scope = p("module x; \
             import x as self_module; \
             newtype Earlier : . { constructor make; projector open; }; \
             type Alias = self_module.Earlier; \
             op + _ { impl Alias.make; };");
        validate_against_modules(&alias_body_in_scope, &[])
            .expect("a self-qualified alias body retains an earlier local target");

        for (name, body) in [
            ("structural", "A & imported.Pair(A, B)"),
            ("reordered", "imported.Pair(B, A)"),
            ("partial", "imported.Pair(A)"),
        ] {
            let consumer = p(&format!(
                "module x; import origin as imported; \
                 type Alias[A][B] = {body}; \
                 op + _ {{ impl Alias.make; }};"
            ));
            let message = expect_name_error(validate_against_modules(
                &consumer,
                std::slice::from_ref(&origin),
            ));
            assert!(
                message.contains("`Alias.make` does not resolve"),
                "{name}: {message}"
            );
        }
    }

    #[test]
    fn qualified_alias_missing_member_does_not_fall_back_to_direct_module() {
        let aliased = p("module a/b; pub fn other(a: A) -> A { a }");
        let coincidental = p("module h; pub fn step(a: A) -> A { a }");
        let consumer = p("module x; \
             import a/b as h; \
             op + _ { impl h.step; };");
        let message = expect_name_error(validate_against_modules(
            &consumer,
            &[aliased, coincidental],
        ));
        assert!(
            message.contains("`h.step` does not resolve"),
            "got: {message}"
        );
    }

    #[test]
    fn package_fold_error_orders_by_module_path_then_span() {
        let errors = vec![
            Some(ModuleFoldError::new(
                "pkg/z".to_owned(),
                PathBuf::from("pkg/z.kio"),
                Error::type_(Span::new(1, 2), "z"),
            )),
            Some(ModuleFoldError::new(
                "pkg/a".to_owned(),
                PathBuf::from("pkg/a.kio"),
                Error::type_(Span::new(5, 6), "a late"),
            )),
            Some(ModuleFoldError::new(
                "pkg/a".to_owned(),
                PathBuf::from("pkg/a.kio"),
                Error::type_(Span::new(3, 4), "a early"),
            )),
        ];

        let err = first_module_fold_error(errors).expect("first fold error");

        assert_eq!(err.file_path, PathBuf::from("pkg/a.kio"));
        assert_eq!(err.span, Span::new(3, 4));
    }

    fn folded_fn_def_body(src: &str, idx: usize) -> Expr<Surface> {
        let m = fold(p(src));
        match &m.items[idx] {
            Item::FnDef(d) => d.body.clone(),
            other => panic!("expected FnDef at index {idx}, got {other:?}"),
        }
    }

    fn call_parts<'a>(
        expr: &'a Expr<Surface>,
        expected_callee: &str,
    ) -> (&'a [CallArg<Surface>], Span) {
        let Expr::Call {
            callee, args, meta, ..
        } = expr
        else {
            panic!("expected call to `{expected_callee}`, got {expr:?}");
        };
        let Expr::Path { segments, .. } = callee.as_ref() else {
            panic!("expected path callee for `{expected_callee}`, got {callee:?}");
        };
        assert_eq!(
            segments.last().map(|segment| segment.name.as_str()),
            Some(expected_callee)
        );
        (args, meta.span)
    }

    fn value_arg(arg: &CallArg<Surface>) -> &Expr<Surface> {
        let CallArg::Value(value) = arg else {
            panic!("expected value argument, got {arg:?}");
        };
        value
    }

    fn assert_path(expr: &Expr<Surface>, expected: &str) {
        let Expr::Path { segments, .. } = expr else {
            panic!("expected path `{expected}`, got {expr:?}");
        };
        assert_eq!(
            segments.last().map(|segment| segment.name.as_str()),
            Some(expected),
        );
    }

    fn variadic_elements(expr: &Expr<Surface>) -> &[Expr<Surface>] {
        let Expr::OpChain {
            kind: OpChainKind::Variadic { elements, .. },
            ..
        } = expr
        else {
            panic!("expected variadic fold literal, got {expr:?}");
        };
        elements
    }

    fn assert_distinct_call_spans(spans: &[Span]) {
        for (index, left) in spans.iter().enumerate() {
            for right in &spans[index + 1..] {
                assert_ne!(left, right, "synthesized calls must have distinct spans");
            }
        }
    }

    #[test]
    fn synthesized_callable_path_segments_use_the_generated_call_span() {
        let declaration_span = Span::new(3, 9);
        let call_span = Span::new(40, 47);
        let path = LexicalCallablePath::new(vec![
            PathSegment::new("helpers", declaration_span),
            PathSegment::new("add", declaration_span),
        ]);

        let Expr::Path { segments, meta, .. } = path_to_expr(&path, call_span) else {
            panic!("expected synthesized callable path")
        };
        assert_eq!(meta.span, call_span);
        assert!(segments.iter().all(|segment| segment.span == call_span));
    }

    #[test]
    fn fold_callable_path_segments_use_the_generated_call_span() {
        let declaration_span = Span::new(3, 9);
        let call_span = Span::new(40, 47);
        let spec = CallableSpec {
            path: LexicalCallablePath::new(vec![
                PathSegment::new("helpers", declaration_span),
                PathSegment::new("add", declaration_span),
            ]),
        };

        let Expr::Call { callee, meta, .. } = callable_spec_to_call(&spec, Vec::new(), call_span)
        else {
            panic!("expected synthesized callable call")
        };
        let Expr::Path {
            segments,
            meta: callee_meta,
            ..
        } = callee.as_ref()
        else {
            panic!("expected synthesized callable path")
        };
        assert_eq!(meta.span, call_span);
        assert_eq!(callee_meta.span, call_span);
        assert!(segments.iter().all(|segment| segment.span == call_span));
    }

    #[test]
    fn fold_binary_op_to_call() {
        let body = folded_fn_def_body(
            "module x; \
             fn add(a: A, b: A) -> A { a } \
             op _ + _ { impl add; }; \
             fn use_op(x: A, y: A) -> A { x + y }",
            2,
        );
        match body {
            Expr::Call { callee, args, .. } => {
                if let Expr::Path { segments, .. } = callee.as_ref() {
                    assert_eq!(segments.len(), 1);
                    assert_eq!(segments[0].name, "add");
                } else {
                    panic!("expected Path callee");
                }
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn fold_prefix_op_to_call() {
        let body = folded_fn_def_body(
            "module x; \
             fn neg(a: A) -> A { a } \
             op - __ { impl neg; }; \
             fn use_op(x: A) -> A { -x }",
            2,
        );
        match body {
            Expr::Call { args, .. } => assert_eq!(args.len(), 1),
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn fold_prefix_then_binary_composes() {
        // Prefix-unary then binary parses without parens — `- a + b`
        // folds to `add(neg(a), b)` (per `specs/language.md` §
        // Operators — Nesting summary). The outer call is `add`
        // with two args; the first arg is the inner prefix call
        // `neg(a)`.
        let body = folded_fn_def_body(
            "module x; \
             fn neg(a: A) -> A { a } \
             fn add(a: A, b: A) -> A { a } \
             op - __ { impl neg; }; \
             op _ + __ { impl add; }; \
             fn use_op(a: A, b: A) -> A { - a + b }",
            4,
        );
        match body {
            Expr::Call { callee, args, .. } => {
                if let Expr::Path { segments, .. } = callee.as_ref() {
                    assert_eq!(segments[0].name, "add");
                } else {
                    panic!("expected `add` Path callee");
                }
                assert_eq!(args.len(), 2);
                let CallArg::Value(Expr::Call {
                    callee: inner_callee,
                    args: inner_args,
                    ..
                }) = &args[0]
                else {
                    panic!("expected nested `neg` call in first arg, got {:?}", args[0]);
                };
                if let Expr::Path { segments, .. } = inner_callee.as_ref() {
                    assert_eq!(segments[0].name, "neg");
                } else {
                    panic!("expected `neg` Path callee");
                }
                assert_eq!(inner_args.len(), 1);
            }
            other => panic!("expected outer Call, got {other:?}"),
        }
    }

    #[test]
    fn fold_ternary_to_call() {
        let body = folded_fn_def_body(
            "module x; \
             fn cond(a: A, b: A, c: A) -> A { a } \
             op _ ? _ : __ { impl cond; }; \
             fn use_op(p: A, t: A, e: A) -> A { p ? t : e }",
            2,
        );
        match body {
            Expr::Call { args, .. } => assert_eq!(args.len(), 3),
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn fold_variadic_to_nested_calls() {
        let body = folded_fn_def_body(
            "module x; \
             fn nil() -> A { let r = (); r } \
             fn cat(x: A, acc: A) -> A { x } \
             varop [* *] { foldr cat nil; }; \
             fn use_op(a: A, b: A) -> A { [* a, b *] }",
            3,
        );
        match body {
            Expr::Call { callee, args, .. } => {
                if let Expr::Path { segments, .. } = callee.as_ref() {
                    assert_eq!(segments[0].name, "cat");
                } else {
                    panic!("expected Path callee");
                }
                assert_eq!(args.len(), 2);
                if let CallArg::Value(Expr::Call {
                    callee: inner_callee,
                    args: inner_args,
                    ..
                }) = &args[1]
                {
                    if let Expr::Path { segments, .. } = inner_callee.as_ref() {
                        assert_eq!(segments[0].name, "cat");
                    } else {
                        panic!("expected nested Path callee");
                    }
                    assert_eq!(inner_args.len(), 2);
                } else {
                    panic!("expected nested cat call");
                }
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn foldr1_consumes_the_last_element() {
        let body = folded_fn_def_body(
            "module x; \
             fn seed(value: A) -> A { value } \
             fn keep_seed(value: A, acc: A) -> A { acc } \
             varop [* *] { foldr1 keep_seed seed; }; \
             fn use_op(a: A, b: A, c: A) -> A { [* a, b, c *] }",
            3,
        );

        let (outer_args, _) = call_parts(&body, "keep_seed");
        let (inner_args, _) = call_parts(value_arg(&outer_args[1]), "keep_seed");
        let (seed_args, _) = call_parts(value_arg(&inner_args[1]), "seed");
        assert_path(value_arg(&outer_args[0]), "a");
        assert_path(value_arg(&inner_args[0]), "b");
        assert_path(value_arg(&seed_args[0]), "c");
    }

    #[test]
    fn foldl1_consumes_the_first_element() {
        let body = folded_fn_def_body(
            "module x; \
             fn seed(value: A) -> A { value } \
             fn keep_seed(acc: A, value: A) -> A { acc } \
             varop [* *] { foldl1 keep_seed seed; }; \
             fn use_op(a: A, b: A, c: A) -> A { [* a, b, c *] }",
            3,
        );

        let (outer_args, _) = call_parts(&body, "keep_seed");
        let (inner_args, _) = call_parts(value_arg(&outer_args[0]), "keep_seed");
        let (seed_args, _) = call_parts(value_arg(&inner_args[0]), "seed");
        assert_path(value_arg(&seed_args[0]), "a");
        assert_path(value_arg(&inner_args[1]), "b");
        assert_path(value_arg(&outer_args[1]), "c");
    }

    #[test]
    fn foldr1_initializer_consumes_one_complete_terminal_pair_expression() {
        let source = "module x; \
             fn seed(entry: A & B) -> A & B { entry } \
             fn keep_seed(entry: A & B, acc: A & B) -> A & B { acc } \
             fn entry(key: A, value: B) -> A & B { (key, value) } \
             op _ => _ { impl entry; }; \
             varop [% %] { foldr1 keep_seed seed; }; \
             fn use_op(a: A, b: B, c: A, d: B) -> A & B { [% a => b, c => d %] }";
        let parsed = p(source);
        let Item::FnDef(use_op) = &parsed.items[5] else {
            panic!("expected use_op function");
        };
        let literal_span = use_op.body.span();
        let terminal_element_end = variadic_elements(&use_op.body)[1].span().end;
        let body = folded_fn_def_body(source, 5);

        let (step_args, step_span) = call_parts(&body, "keep_seed");
        let (seed_args, seed_span) = call_parts(value_arg(&step_args[1]), "seed");
        let (first_pair, first_pair_span) = call_parts(value_arg(&step_args[0]), "entry");
        let (last_pair, last_pair_span) = call_parts(value_arg(&seed_args[0]), "entry");
        assert_eq!(step_args.len(), 2);
        assert_eq!(seed_args.len(), 1);
        assert_path(value_arg(&first_pair[0]), "a");
        assert_path(value_arg(&first_pair[1]), "b");
        assert_path(value_arg(&last_pair[0]), "c");
        assert_path(value_arg(&last_pair[1]), "d");
        assert_eq!(
            seed_span,
            Span::new(literal_span.start, terminal_element_end)
        );
        assert_distinct_call_spans(&[seed_span, step_span, first_pair_span, last_pair_span]);
    }

    #[test]
    fn one_element_seeded_fold_skips_step_and_finalizes_the_base() {
        let source = "module x; \
             fn seed(value: A) -> A { value } \
             fn step(value: A, acc: A) -> A { value } \
             fn finish(acc: A) -> A { acc } \
             varop [* *] { foldr1 step seed; finalize finish; }; \
             fn use_op(a: A) -> A { [* a *] }";
        let parsed = p(source);
        let Item::FnDef(use_op) = &parsed.items[4] else {
            panic!("expected use_op function");
        };
        let literal_span = use_op.body.span();
        let element_end = variadic_elements(&use_op.body)[0].span().end;
        let body = folded_fn_def_body(source, 4);

        let (finish_args, finish_span) = call_parts(&body, "finish");
        let (seed_args, seed_span) = call_parts(value_arg(&finish_args[0]), "seed");
        assert_eq!(finish_args.len(), 1);
        assert_eq!(seed_args.len(), 1);
        assert_path(value_arg(&seed_args[0]), "a");
        assert_eq!(seed_span, Span::new(literal_span.start, element_end));
        assert_eq!(finish_span, literal_span);
        assert_distinct_call_spans(&[seed_span, finish_span]);
    }

    #[test]
    fn right_fold_call_spans_anchor_every_pair_step_to_its_element_end() {
        let src = "module x; \
             fn base() -> C { () } \
             fn step(_entry: A & B, acc: C) -> C { acc } \
             fn finish(acc: C) -> C { acc } \
             fn entry(key: A, value: B) -> A & B { (key, value) } \
             op _ => _ { impl entry; }; \
             varop [% %] { foldr step base; finalize finish; }; \
             fn use_op(a: A, b: B, c: A, d: B) -> C { [% a => b, c => d %] }";
        let parsed = p(src);
        let Item::FnDef(use_op) = &parsed.items[6] else {
            panic!("expected use_op function");
        };
        let literal_span = use_op.body.span();
        let body = folded_fn_def_body(src, 6);

        let (finalize_args, finalize_span) = call_parts(&body, "finish");
        let (outer_step_args, outer_step_span) = call_parts(value_arg(&finalize_args[0]), "step");
        let (inner_step_args, inner_step_span) = call_parts(value_arg(&outer_step_args[1]), "step");
        let (base_args, base_span) = call_parts(value_arg(&inner_step_args[1]), "base");
        let (outer_pair, outer_pair_span) = call_parts(value_arg(&outer_step_args[0]), "entry");
        let (inner_pair, inner_pair_span) = call_parts(value_arg(&inner_step_args[0]), "entry");
        let outer_final_slot_end = value_arg(&outer_pair[1]).span().end;
        let inner_final_slot_end = value_arg(&inner_pair[1]).span().end;

        assert!(base_args.is_empty());
        assert_eq!(base_span, Span::new(literal_span.start, literal_span.start));
        assert_eq!(
            outer_step_span,
            Span::new(literal_span.start, outer_final_slot_end)
        );
        assert_eq!(
            inner_step_span,
            Span::new(literal_span.start, inner_final_slot_end)
        );
        assert_eq!(finalize_span, literal_span);
        assert_distinct_call_spans(&[
            base_span,
            inner_step_span,
            outer_step_span,
            finalize_span,
            outer_pair_span,
            inner_pair_span,
        ]);
    }

    #[test]
    fn left_fold_call_spans_anchor_every_step_to_its_slot() {
        let src = "module x; \
             fn base() -> C { () } \
             fn step(acc: C, _value: A) -> C { acc } \
             fn finish(acc: C) -> C { acc } \
             varop [! !] { foldl step base; finalize finish; }; \
             fn use_op(a: A, b: A) -> C { [! a, b !] }";
        let parsed = p(src);
        let Item::FnDef(use_op) = &parsed.items[4] else {
            panic!("expected use_op function");
        };
        let literal_span = use_op.body.span();
        let body = folded_fn_def_body(src, 4);

        let (finalize_args, finalize_span) = call_parts(&body, "finish");
        let (outer_step_args, outer_step_span) = call_parts(value_arg(&finalize_args[0]), "step");
        let (inner_step_args, inner_step_span) = call_parts(value_arg(&outer_step_args[0]), "step");
        let (base_args, base_span) = call_parts(value_arg(&inner_step_args[0]), "base");
        let outer_final_slot_end = value_arg(&outer_step_args[1]).span().end;
        let inner_final_slot_end = value_arg(&inner_step_args[1]).span().end;

        assert!(base_args.is_empty());
        assert_eq!(base_span, Span::new(literal_span.start, literal_span.start));
        assert_eq!(
            outer_step_span,
            Span::new(literal_span.start, outer_final_slot_end)
        );
        assert_eq!(
            inner_step_span,
            Span::new(literal_span.start, inner_final_slot_end)
        );
        assert_eq!(finalize_span, literal_span);
        assert_distinct_call_spans(&[base_span, inner_step_span, outer_step_span, finalize_span]);
    }

    #[test]
    fn nested_element_uses_the_enclosing_fold_opening_for_identity() {
        let src = "module x; \
             fn inner_base() -> C { () } \
             fn inner_step(_value: A, acc: C) -> C { acc } \
             varop [* *] { foldr inner_step inner_base; }; \
             fn outer_base() -> D { () } \
             fn outer_step(_value: C, acc: D) -> D { acc } \
             varop [% %] { foldr outer_step outer_base; }; \
             fn use_op(a: A) -> D { [% [* a *] %] }";
        let parsed = p(src);
        let Item::FnDef(use_op) = &parsed.items[6] else {
            panic!("expected use_op function");
        };
        let outer_literal_span = use_op.body.span();
        let nested_literal = &variadic_elements(&use_op.body)[0];
        let nested_literal_span = nested_literal.span();
        let nested_value_end = variadic_elements(nested_literal)[0].span().end;

        let body = folded_fn_def_body(src, 6);
        let (outer_step_args, outer_step_span) = call_parts(&body, "outer_step");
        let (inner_step_args, inner_step_span) =
            call_parts(value_arg(&outer_step_args[0]), "inner_step");
        let (inner_base_args, inner_base_span) =
            call_parts(value_arg(&inner_step_args[1]), "inner_base");
        let (outer_base_args, outer_base_span) =
            call_parts(value_arg(&outer_step_args[1]), "outer_base");

        assert!(inner_base_args.is_empty());
        assert!(outer_base_args.is_empty());
        assert_eq!(
            inner_step_span,
            Span::new(nested_literal_span.start, nested_value_end)
        );
        assert_eq!(
            outer_step_span,
            Span::new(outer_literal_span.start, nested_value_end)
        );
        assert_ne!(inner_step_span.start, outer_step_span.start);
        assert_eq!(inner_step_span.end, outer_step_span.end);
        assert_distinct_call_spans(&[
            inner_base_span,
            inner_step_span,
            outer_base_span,
            outer_step_span,
        ]);
    }

    #[test]
    fn finalizing_nested_element_keeps_inner_and_enclosing_calls_distinct() {
        let src = "module x; \
             fn inner_base() -> C { () } \
             fn inner_step(_value: A, acc: C) -> C { acc } \
             fn inner_finish(acc: C) -> C { acc } \
             varop [+ +] { \
               foldr inner_step inner_base; finalize inner_finish; \
             }; \
             fn outer_base() -> D { () } \
             fn outer_step(_value: C, acc: D) -> D { acc } \
             varop [% %] { foldr outer_step outer_base; }; \
             fn use_op(a: A) -> D { [% [+ a +] %] }";
        let parsed = p(src);
        let Item::FnDef(use_op) = &parsed.items[7] else {
            panic!("expected use_op function");
        };
        let outer_literal_span = use_op.body.span();
        let nested_literal = &variadic_elements(&use_op.body)[0];
        let nested_literal_span = nested_literal.span();
        let nested_value_end = variadic_elements(nested_literal)[0].span().end;

        let body = folded_fn_def_body(src, 7);
        let (outer_step_args, outer_step_span) = call_parts(&body, "outer_step");
        let (inner_finish_args, inner_finish_span) =
            call_parts(value_arg(&outer_step_args[0]), "inner_finish");
        let (inner_step_args, inner_step_span) =
            call_parts(value_arg(&inner_finish_args[0]), "inner_step");
        let (_, inner_base_span) = call_parts(value_arg(&inner_step_args[1]), "inner_base");
        let (_, outer_base_span) = call_parts(value_arg(&outer_step_args[1]), "outer_base");

        assert_eq!(inner_finish_span, nested_literal_span);
        assert_eq!(
            inner_step_span,
            Span::new(nested_literal_span.start, nested_value_end)
        );
        assert_eq!(
            outer_step_span,
            Span::new(outer_literal_span.start, nested_literal_span.end)
        );
        assert_eq!(inner_finish_span.end, outer_step_span.end);
        assert_ne!(inner_finish_span.start, outer_step_span.start);
        assert_distinct_call_spans(&[
            inner_base_span,
            inner_step_span,
            inner_finish_span,
            outer_base_span,
            outer_step_span,
        ]);
    }

    #[test]
    fn empty_nested_element_keeps_inner_base_and_enclosing_step_distinct() {
        let src = "module x; \
             fn inner_base() -> C { () } \
             fn inner_step(_value: A, acc: C) -> C { acc } \
             varop [* *] { foldr inner_step inner_base; }; \
             fn outer_base() -> D { () } \
             fn outer_step(_value: C, acc: D) -> D { acc } \
             varop [% %] { foldr outer_step outer_base; }; \
             fn use_op() -> D { [% [* *] %] }";
        let parsed = p(src);
        let Item::FnDef(use_op) = &parsed.items[6] else {
            panic!("expected use_op function");
        };
        let outer_literal_span = use_op.body.span();
        let nested_literal_span = variadic_elements(&use_op.body)[0].span();

        let body = folded_fn_def_body(src, 6);
        let (outer_step_args, outer_step_span) = call_parts(&body, "outer_step");
        let (inner_base_args, inner_base_span) =
            call_parts(value_arg(&outer_step_args[0]), "inner_base");
        let (_, outer_base_span) = call_parts(value_arg(&outer_step_args[1]), "outer_base");

        assert!(inner_base_args.is_empty());
        assert_eq!(
            inner_base_span,
            Span::new(nested_literal_span.start, nested_literal_span.start)
        );
        assert_eq!(
            outer_step_span,
            Span::new(outer_literal_span.start, nested_literal_span.start)
        );
        assert_eq!(inner_base_span.end, outer_step_span.end);
        assert_ne!(inner_base_span.start, outer_step_span.start);
        assert_distinct_call_spans(&[inner_base_span, outer_base_span, outer_step_span]);
    }

    #[test]
    fn variadic_fold_commas_do_not_create_phantom_elements() {
        let source = "module x; \
             fn base() -> . { () } \
             fn step(_value: ., acc: .) -> . { acc } \
             varop [* *] { foldr step base; }; \
             fn use_op() -> . { [* ,,, *] }";
        let parsed = p(source);
        let Item::FnDef(use_op) = &parsed.items[3] else {
            panic!("expected use_op function");
        };
        assert!(variadic_elements(&use_op.body).is_empty());
        let literal_span = use_op.body.span();
        let body = folded_fn_def_body(source, 3);
        let (base_args, base_span) = call_parts(&body, "base");
        assert!(base_args.is_empty());
        assert_eq!(base_span, Span::new(literal_span.start, literal_span.start));
    }

    #[test]
    fn foldr1_rejects_an_empty_literal() {
        let m = p("module x; \
             fn singleton(x: A) -> A { x } \
             fn cat(x: A, acc: A) -> A { x } \
             varop [* *] { foldr1 cat singleton; }; \
             fn use_op(x: A) -> A { [* *] }");
        let modules = [(PathBuf::from("x.kio"), m.clone())];
        let pkg =
            PackageOpScope::from_modules_named(&modules, None).expect("fold declaration validates");
        let mut m = m;
        let err = fold_module(&mut m, &pkg).expect_err("rejects empty");
        match err {
            Error::Parse(Diagnostic { message, .. }) => {
                assert!(
                    message.contains("`foldr1` requires at least one complete element"),
                    "got: {message}"
                );
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn lowering_rejects_same_key_projection_mismatches() {
        for (declared, selected, body) in [
            ("_ ? _ : _", "op _ ? _", "a ? b"),
            ("_ ? __", "op _ ? _", "a ? b"),
            ("(_ ? _)", "op _ ? _", "a ? b"),
        ] {
            let provider = p(&format!(
                "module syntax;
                 pub fn target(a: ., b: ., c: .) -> . {{ a }}
                 pub fn empty() -> . {{ () }}
                 pub op {declared} {{ impl target; }};"
            ));
            let consumer_source = format!(
                "module consumer; import syntax({selected});
                 fn run(a: ., b: .) -> . {{ {body} }}"
            );
            let consumer = p(&consumer_source);
            let ImportKind::Selective { items, .. } = &consumer.imports[0].kind else {
                panic!("selective import");
            };
            let ImportItem::OperatorPattern { grammar, span, .. } = &items[0] else {
                panic!("operator projection");
            };
            let provided = match &provider.items[2] {
                Item::Op(declaration, _) => {
                    let OpBody::Normal { pattern, .. } = &declaration.body;
                    crate::ast::OperatorGrammar::fixed(pattern)
                }
                Item::VariadicOperator(declaration, _) => {
                    crate::ast::OperatorGrammar::variadic(&declaration.open, &declaration.spec)
                }
                _ => panic!("operator declaration"),
            };
            assert_eq!(grammar.dispatch_key(), provided.dispatch_key());
            assert_ne!(grammar, &provided);
            let selected_span = *span;
            let error = fold_package(vec![
                (PathBuf::from("syntax.kio"), provider),
                (PathBuf::from("consumer.kio"), consumer),
            ])
            .expect_err("a matching dispatch key cannot erase a grammar mismatch")
            .1;
            let Error::Import(diagnostic) = error else {
                panic!("import error: {error:?}")
            };
            assert_eq!(diagnostic.span, selected_span);
            assert!(
                diagnostic.message.contains("grammar does not match"),
                "{diagnostic:?}"
            );
            assert!(diagnostic.message.contains(selected), "{diagnostic:?}");
            assert!(diagnostic.help().unwrap().contains(&provided.render()));
        }
    }

    #[test]
    fn variadic_projection_mismatches_are_rejected_before_lowering() {
        for selected in [
            "op variadic <% _ => _ ; ... %>",
            "op variadic <% _ -> _ , ... %>",
            "op variadic <% _ => _ , ... !>",
            "varop [% _ => _ , ... %]",
            "varop [% !]",
        ] {
            let source = format!("module consumer; import syntax({selected});");
            assert!(parse(&source).is_err(), "{selected}");
            assert!(
                crate::pass::parser::parse_lazy(&source).is_err(),
                "{selected}"
            );
        }
    }

    #[test]
    fn fold_cross_module_binary_op() {
        let source = p("module m; \
             pub fn add(a: A, b: A) -> A { a } \
             pub op _ + _ { impl add; };");
        let consumer = p("module x; \
             import m(op _ + _); \
             fn use_op(x: A, y: A) -> A { x + y }");
        let modules = vec![
            (PathBuf::from("x.kio"), consumer),
            (PathBuf::from("m.kio"), source),
        ];
        let folded = fold_package(modules).expect("fold_package");
        let consumer = &folded[0].1;
        // The synthetic path-byte alias was injected.
        let synthetic = consumer
            .imports
            .iter()
            .find(|u| matches!(&u.kind, ImportKind::Qualified { alias, .. } if alias == "_op_gn__"))
            .expect("synthetic alias clause");
        if let ImportKind::Qualified { path, .. } = &synthetic.kind {
            assert_eq!(path.segments, vec!["m"]);
        }
        // The original `import m(op _ + _);` clause was stripped from
        // `module.imports` after op-fold consumed its operator-pattern
        // item.
        let stale = consumer.imports.iter().find(|u| {
            matches!(
                &u.kind,
                ImportKind::Selective { items, .. }
                    if items.iter().any(|i| matches!(i, ImportItem::OperatorPattern { .. }))
            )
        });
        assert!(stale.is_none(), "operator-pattern items should be stripped");
        // The use-site `x + y` folded through that exact alias.
        if let Item::FnDef(d) = &consumer.items[0]
            && let Expr::Call { callee, args, .. } = &d.body
        {
            if let Expr::Path { segments, .. } = callee.as_ref() {
                assert_eq!(
                    segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                    vec!["_op_gn__", "add"]
                );
            } else {
                panic!("expected qualified Path callee");
            }
            assert_eq!(args.len(), 2);
        } else {
            panic!("expected folded Call body");
        }
    }

    #[test]
    fn imported_named_callables_keep_their_resolved_owner() {
        let definitions = p("module d; \
             pub fn combine(_a: ., _b: .) -> . { () } \
             pub fn begin() -> . { () } \
             pub fn accumulate(_a: ., _b: .) -> . { () } \
             pub fn finish(_a: .) -> . { () }");
        let syntax = p("module p; \
             import d(combine, finish); \
             import d as definitions; \
             pub op _ %+ _ { impl combine; }; \
             pub varop [* *] { \
               foldr definitions.accumulate definitions.begin; \
               finalize finish; \
             };");
        let consumer = p("module x; \
             import p(op _ %+ _, varop [* *]); \
             fn use_op(a: ., b: .) -> . { a %+ b } \
             fn use_fold(a: ., b: .) -> . { [* a, b *] }");
        let modules = vec![
            (PathBuf::from("x.kio"), consumer),
            (PathBuf::from("p.kio"), syntax),
            (PathBuf::from("d.kio"), definitions),
        ];
        let folded = fold_package(modules).expect("fold package");
        let consumer = &folded[0].1;

        let imported_modules = consumer
            .imports
            .iter()
            .filter_map(|import_| match &import_.kind {
                ImportKind::Qualified { path, alias } if alias.starts_with("_op_") => {
                    Some(module_path_str(path))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(imported_modules, vec!["d"]);

        let Item::FnDef(use_op) = &consumer.items[0] else {
            panic!("expected operator consumer");
        };
        let Expr::Call { callee, .. } = &use_op.body else {
            panic!("expected imported operator call");
        };
        let Expr::Path { segments, .. } = callee.as_ref() else {
            panic!("expected imported operator path");
        };
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>(),
            vec!["_op_ge__", "combine"]
        );

        let Item::FnDef(use_fold) = &consumer.items[1] else {
            panic!("expected fold consumer");
        };
        let (finish_args, _) = call_parts(&use_fold.body, "finish");
        let (outer_step_args, _) = call_parts(value_arg(&finish_args[0]), "accumulate");
        let (inner_step_args, _) = call_parts(value_arg(&outer_step_args[1]), "accumulate");
        call_parts(value_arg(&inner_step_args[1]), "begin");
        for call in [
            &use_fold.body,
            value_arg(&finish_args[0]),
            value_arg(&outer_step_args[1]),
            value_arg(&inner_step_args[1]),
        ] {
            let Expr::Call { callee, .. } = call else {
                panic!("expected routed callable");
            };
            let Expr::Path { segments, .. } = callee.as_ref() else {
                panic!("expected routed callable path");
            };
            assert_eq!(segments[0].name, "_op_ge__");
        }
    }

    #[test]
    fn imported_builtin_callables_keep_their_import_class_and_bare_name() {
        let syntax = p("module p; \
             import __intrinsics__; \
             import __comptime__; \
             pub op _ ** _ { impl __pair__; }; \
             pub varop [+ +] { \
               foldr __term_call__ __term_unit__; \
               finalize __term_type__; \
             };");
        let consumer = p("module x; \
             import __intrinsics__; \
             import p(op _ ** _, varop [+ +]); \
             fn use_op(a: ., b: .) -> . { a ** b } \
             fn use_fold(a: ., b: .) -> . { [+ a, b +] }");
        let modules = vec![
            (PathBuf::from("x.kio"), consumer),
            (PathBuf::from("p.kio"), syntax),
        ];
        let folded = fold_package(modules).expect("fold package");
        let consumer = &folded[0].1;

        assert_eq!(
            consumer
                .imports
                .iter()
                .filter(|import_| matches!(import_.kind, ImportKind::Intrinsics))
                .count(),
            1
        );
        assert_eq!(
            consumer
                .imports
                .iter()
                .filter(|import_| matches!(import_.kind, ImportKind::Comptime))
                .count(),
            1
        );
        assert!(
            consumer
                .imports
                .iter()
                .all(|import_| !matches!(import_.kind, ImportKind::Qualified { .. }))
        );

        let Item::FnDef(use_op) = &consumer.items[0] else {
            panic!("expected operator consumer");
        };
        assert_bare_call(&use_op.body, "__pair__");

        let Item::FnDef(use_fold) = &consumer.items[1] else {
            panic!("expected fold consumer");
        };
        let finalize_args = assert_bare_call(&use_fold.body, "__term_type__");
        let outer_step_args = assert_bare_call(value_arg(&finalize_args[0]), "__term_call__");
        let inner_step_args = assert_bare_call(value_arg(&outer_step_args[1]), "__term_call__");
        assert_bare_call(value_arg(&inner_step_args[1]), "__term_unit__");
    }

    fn assert_bare_call<'a>(
        expr: &'a Expr<Surface>,
        expected_callee: &str,
    ) -> &'a [CallArg<Surface>] {
        let Expr::Call { callee, args, .. } = expr else {
            panic!("expected call to `{expected_callee}`, got {expr:?}");
        };
        let Expr::Path { segments, .. } = callee.as_ref() else {
            panic!("expected path callee for `{expected_callee}`, got {callee:?}");
        };
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>(),
            vec![expected_callee]
        );
        args
    }

    #[test]
    fn fold_cross_module_prefix_op() {
        // Cross-module operator import keyed by the prefix keyspace
        // (per `specs/language.md` § Operators). The consumer
        // imports `+ _` from `m`; the source declares a prefix-shaped
        // `op + _ { impl pos; };` and the consumer's use-site `+ x` folds
        // into the synthetic qualified call through the same alias.
        let source = p("module m; \
             pub fn pos(a: A) -> A { a } \
             pub op + _ { impl pos; };");
        let consumer = p("module x; \
             import m(op + _); \
             fn use_op(x: A) -> A { + x }");
        let modules = vec![
            (PathBuf::from("x.kio"), consumer),
            (PathBuf::from("m.kio"), source),
        ];
        let folded = fold_package(modules).expect("fold_package");
        let consumer = &folded[0].1;
        if let Item::FnDef(d) = &consumer.items[0]
            && let Expr::Call { callee, args, .. } = &d.body
        {
            if let Expr::Path { segments, .. } = callee.as_ref() {
                assert_eq!(
                    segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                    vec!["_op_gn__", "pos"]
                );
            } else {
                panic!("expected qualified Path callee");
            }
            assert_eq!(args.len(), 1);
        } else {
            panic!("expected folded Call body");
        }
    }

    #[test]
    fn fold_imported_binary_op() {
        let arith = pf("module arith; \
             pub fn add(a: A, b: A) -> A { a } \
             pub op _ + _ { impl add; };");
        let consumer = p("module x; \
             import arith(op _ + _); \
             fn use_op(x: A, y: A) -> A { x + y }");

        let modules = vec![
            (PathBuf::from("x.kio"), consumer),
            (PathBuf::from("arith.kio"), arith),
        ];
        let folded = fold_package(modules).expect("fold_package");
        let consumer = &folded[0].1;

        // The synthetic path-byte alias for `arith` was injected.
        let synthetic = consumer
            .imports
            .iter()
            .find(|u| matches!(&u.kind, ImportKind::Qualified { alias, .. } if alias == "_op_gbhcgjhegi__"))
            .expect("synthetic alias clause");
        if let ImportKind::Qualified { path, .. } = &synthetic.kind {
            assert_eq!(path.segments, vec!["arith"]);
        }

        // The original `import arith(op _ + _);` operator-pattern item
        // was stripped after op-fold consumed it.
        let stale = consumer.imports.iter().find(|u| {
            matches!(
                &u.kind,
                ImportKind::Selective { items, .. }
                    if items.iter().any(|i| matches!(i, ImportItem::OperatorPattern { .. }))
            )
        });
        assert!(stale.is_none(), "operator-pattern items should be stripped");

        // The use-site `x + y` folded through that exact alias.
        if let Item::FnDef(d) = &consumer.items[0]
            && let Expr::Call { callee, args, .. } = &d.body
        {
            if let Expr::Path { segments, .. } = callee.as_ref() {
                assert_eq!(
                    segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                    vec!["_op_gbhcgjhegi__", "add"]
                );
            } else {
                panic!("expected qualified Path callee");
            }
            assert_eq!(args.len(), 2);
        } else {
            panic!("expected folded Call body");
        }
    }

    #[test]
    fn fold_imported_no_matching_op_errors() {
        let consumer = p("module x; \
             import arith(op _ + _); \
             fn use_op(x: A, y: A) -> A { x + y }");
        let arith = pf("module arith; pub fn add(a: A, b: A) -> A { a }");

        let modules = vec![
            (PathBuf::from("x.kio"), consumer),
            (PathBuf::from("arith.kio"), arith),
        ];
        let err = fold_package(modules).expect_err("no matching op").1;
        match err {
            Error::Import(Diagnostic { message, .. }) => {
                assert!(
                    message.contains("module `arith`")
                        && message.contains("exports no operator matching"),
                    "got: {message}"
                );
            }
            other => panic!("expected Import error, got {other:?}"),
        }
    }
}
