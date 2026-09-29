//! Fresh-artifact validation for replayed signature histories.
//!
//! A signature declaration keeps the imports from its own version-local
//! origin. We therefore cannot rebuild one module by unioning imports from
//! several historical declarations: a later import collision could rebind an
//! older type head. Instead, each newly introduced origin is resolved against
//! a name-only view of its candidate epoch under only its own imports. The
//! resulting declaration is fully qualified. A complete epoch with ordinary
//! imports erased can then run through the package resolver and shared
//! signature typer; an origin that uses a public `__comptime__` type retains
//! that explicit capability import because the builtin has no ordinary FQN.

use super::QualifiedName;
use crate::ast::{
    HostFn, HostFnParam, Import, Item, Meta, Module, Prime, SigExportFn, SigItem, SigModuleSection,
    Surface, Type, TypeRecMember, Visibility,
};
use crate::error::Error;
use crate::pass::resolve::{NominalProvider, NominalSelection, Package};
use crate::span::Span;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct CanonicalDeclaration {
    pub(crate) item: SigItem<Prime>,
    pub(crate) recursive_context: Option<Arc<SigModuleSection<Prime>>>,
    pub(crate) comptime_import: Option<Import>,
}

pub(crate) type CanonicalEpoch = BTreeMap<QualifiedName, CanonicalDeclaration>;

#[derive(Debug, Clone)]
pub(crate) struct PendingDeclaration {
    pub(crate) item: SigItem<Surface>,
    pub(crate) imports: Vec<Import>,
    pub(crate) recursive_context: Option<Arc<SigModuleSection<Surface>>>,
}

#[derive(Debug, Clone)]
struct PendingPrimeDeclaration {
    item: SigItem<Prime>,
    imports: Vec<Import>,
    recursive_context: Option<Arc<SigModuleSection<Prime>>>,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct CanonicalizationWork {
    pub(super) package_builds: usize,
    pub(super) package_items: usize,
}

#[cfg(test)]
thread_local! {
    static ACTIVE_CANONICALIZATION_WORK: std::cell::RefCell<Option<CanonicalizationWork>> =
        const { std::cell::RefCell::new(None) };
    static LAST_CANONICALIZATION_WORK: std::cell::RefCell<Option<CanonicalizationWork>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn take_last_canonicalization_work() -> CanonicalizationWork {
    LAST_CANONICALIZATION_WORK.with(|slot| {
        slot.borrow_mut()
            .take()
            .expect("signature replay recorded canonicalization work")
    })
}

#[cfg(test)]
struct CanonicalizationWorkGuard;

#[cfg(test)]
impl CanonicalizationWorkGuard {
    fn start() -> Self {
        ACTIVE_CANONICALIZATION_WORK.with(|slot| {
            assert!(
                slot.borrow_mut()
                    .replace(CanonicalizationWork::default())
                    .is_none()
            );
        });
        Self
    }
}

#[cfg(test)]
impl Drop for CanonicalizationWorkGuard {
    fn drop(&mut self) {
        let work = ACTIVE_CANONICALIZATION_WORK.with(|slot| slot.borrow_mut().take());
        LAST_CANONICALIZATION_WORK.with(|slot| *slot.borrow_mut() = work);
    }
}

/// Resolve and fully qualify every declaration newly originating in this
/// version, while retaining already-canonical declarations from earlier
/// origins unchanged.
pub(crate) fn canonicalize_pending(
    mut epoch: CanonicalEpoch,
    pending: &BTreeMap<QualifiedName, PendingDeclaration>,
) -> Result<CanonicalEpoch, Error> {
    #[cfg(test)]
    let _work = CanonicalizationWorkGuard::start();
    let lowered = lower_pending(pending)?;
    let mut candidate = epoch.clone();
    for (name, declaration) in &lowered {
        candidate.insert(
            name.clone(),
            CanonicalDeclaration {
                item: declaration.item.clone(),
                recursive_context: declaration.recursive_context.clone(),
                comptime_import: comptime_import(&declaration.imports),
            },
        );
    }
    let candidate_modules = candidate
        .keys()
        .map(|name| name.module_path.clone())
        .collect::<BTreeSet<_>>();

    let mut completed_groups = BTreeSet::new();
    for (name, declaration) in &lowered {
        if let Some(context) = &declaration.recursive_context {
            let key = Arc::as_ptr(context) as usize;
            if !completed_groups.insert(key) {
                continue;
            }
            let canonical = canonicalize_section(
                context,
                &declaration.imports,
                &candidate,
                &candidate_modules,
            )?;
            install_context(&mut epoch, &canonical);
        } else {
            let section = SigModuleSection {
                leading_trivia: Vec::new(),
                trailing_trivia: Vec::new(),
                path: module_path(name),
                imports: declaration.imports.clone(),
                items: vec![declaration.item.clone()],
                span: item_span(&declaration.item),
            };
            let canonical = canonicalize_section(
                &section,
                &declaration.imports,
                &candidate,
                &candidate_modules,
            )?;
            let item = canonical
                .items
                .into_iter()
                .next()
                .expect("a one-declaration validation section stays non-empty");
            epoch.insert(
                name.clone(),
                CanonicalDeclaration {
                    item,
                    recursive_context: None,
                    comptime_import: canonical.imports.first().cloned(),
                },
            );
        }
    }
    Ok(epoch)
}

/// Validate one complete, fully-qualified interface epoch through the same
/// package resolver and signature checker used for ordinary source packages.
pub(crate) fn validate_epoch(epoch: &CanonicalEpoch) -> Result<(), Error> {
    if epoch.is_empty() {
        return Ok(());
    }
    let package = build_package(epoch, None, &[])?;
    for (_, entry) in package.modules() {
        for item in &entry.module.items {
            if let Item::TypeRecGroup(group) = item {
                crate::pass::resolve::validate_canonical_type_rec_group(group, &entry.module.path)?;
            }
        }
    }
    crate::pass::typecheck_core::check_no_alias_only_cycles(&package)
        .map_err(|error| error.error)?;
    crate::pass::typecheck_core::check_package_signatures(&package).map_err(|error| error.error)
}

/// Verify that every live canonical recursive component is represented by
/// exactly one complete version-context identity. Validating each written
/// `with` declaration independently is insufficient: two individually valid
/// singleton contexts can acquire cross-edges and jointly form one SCC.
pub(crate) fn validate_recursive_context_partition(epoch: &CanonicalEpoch) -> Result<(), Error> {
    struct EpochType {
        name: QualifiedName,
        member: TypeRecMember<Prime>,
        context: Option<usize>,
    }

    // CanonicalEpoch is ordered by (module path, leaf), so collect contiguous
    // module runs without rebuilding another ordered index. Context identity
    // lookup below is hash-indexed; together these keep partition validation
    // linear in the live type graph on the expected HashMap cost model.
    let mut modules = Vec::<(String, Vec<EpochType>)>::new();
    for (name, declaration) in epoch {
        let member = match &declaration.item {
            SigItem::TypeAlias(item) => TypeRecMember::TypeAlias(item.clone()),
            SigItem::Newtype(item) => TypeRecMember::Newtype(item.clone()),
            SigItem::HostType(_)
            | SigItem::HostFn(_)
            | SigItem::ExportFn(_)
            | SigItem::TypeRecGroup(_) => continue,
        };
        if modules.last().map(|(module, _)| module.as_str()) != Some(name.module_path.as_str()) {
            modules.push((name.module_path.clone(), Vec::new()));
        }
        modules
            .last_mut()
            .expect("the current module run was just established")
            .1
            .push(EpochType {
                name: name.clone(),
                member,
                context: declaration
                    .recursive_context
                    .as_ref()
                    .map(|context| Arc::as_ptr(context) as usize),
            });
    }

    for (module, declarations) in modules {
        let members = declarations
            .iter()
            .map(|declaration| declaration.member.clone())
            .collect::<Vec<_>>();
        let owner = module_path_from_string(&module);
        let cyclic_components =
            crate::pass::resolve::canonical_type_rec_cyclic_components(&members, &owner);
        let mut context_members = HashMap::<usize, Vec<usize>>::new();
        for (index, declaration) in declarations.iter().enumerate() {
            if let Some(context) = declaration.context {
                context_members.entry(context).or_default().push(index);
            }
        }
        let mut matched_context_members = vec![false; declarations.len()];
        for component in &cyclic_components {
            let first = component[0];
            let Some(context) = declarations[first].context else {
                return Err(Error::bridge(
                    declarations[first].member.meta().span,
                    format!(
                        "recursive signature component containing `{}` has no complete `with` context",
                        declarations[first].name
                    ),
                )
                .with_help(
                    "put every member of this maximal recursive component in one `with` declaration",
                ));
            };
            for &member in &component[1..] {
                if declarations[member].context != Some(context) {
                    return Err(Error::bridge(
                        declarations[member].member.meta().span,
                        format!(
                            "recursive signature component containing `{}` is split across `with` contexts",
                            declarations[member].name
                        ),
                    )
                    .with_secondary(
                        declarations[first].member.meta().span,
                        format!(
                            "`{}` belongs to the same maximal recursive component",
                            declarations[first].name
                        ),
                    )
                    .with_help(
                        "put every member of this maximal recursive component in one complete recursive context",
                    ));
                }
            }
            let retained = context_members
                .get(&context)
                .expect("a declaration context indexes its member");
            if retained != component {
                let extra = first_extra_sorted_member(retained, component).unwrap_or(first);
                return Err(Error::bridge(
                    declarations[extra].member.meta().span,
                    format!(
                        "the `with` context containing `{}` does not match one complete recursive component",
                        declarations[extra].name
                    ),
                )
                .with_secondary(
                    declarations[first].member.meta().span,
                    "this maximal recursive component determines the required context",
                )
                .with_help(
                    "keep exactly the members of one maximal recursive component in each recursive context",
                ));
            }
            for &member in component {
                matched_context_members[member] = true;
            }
        }

        if let Some((_, declaration)) =
            declarations
                .iter()
                .enumerate()
                .find(|(index, declaration)| {
                    declaration.context.is_some() && !matched_context_members[*index]
                })
        {
            return Err(Error::bridge(
                declaration.member.meta().span,
                format!(
                    "the `with` context containing `{}` is not a recursive type component",
                    declaration.name
                ),
            )
            .with_help(
                "write acyclic declarations inline, or keep exactly one genuine recursive component in `with`",
            ));
        }
    }
    Ok(())
}

fn first_extra_sorted_member(retained: &[usize], component: &[usize]) -> Option<usize> {
    let mut retained_index = 0;
    let mut component_index = 0;
    while retained_index < retained.len() && component_index < component.len() {
        #[cfg(test)]
        RECURSIVE_CONTEXT_MEMBER_COMPARISONS
            .with(|comparisons| comparisons.set(comparisons.get() + 1));
        match retained[retained_index].cmp(&component[component_index]) {
            std::cmp::Ordering::Less => return Some(retained[retained_index]),
            std::cmp::Ordering::Equal => {
                retained_index += 1;
                component_index += 1;
            }
            std::cmp::Ordering::Greater => component_index += 1,
        }
    }
    retained.get(retained_index).copied()
}

#[cfg(test)]
thread_local! {
    static RECURSIVE_CONTEXT_MEMBER_COMPARISONS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
fn reset_recursive_context_member_comparisons() {
    RECURSIVE_CONTEXT_MEMBER_COMPARISONS.with(|comparisons| comparisons.set(0));
}

#[cfg(test)]
fn recursive_context_member_comparisons() -> usize {
    RECURSIVE_CONTEXT_MEMBER_COMPARISONS.with(std::cell::Cell::get)
}

fn lower_pending(
    pending: &BTreeMap<QualifiedName, PendingDeclaration>,
) -> Result<BTreeMap<QualifiedName, PendingPrimeDeclaration>, Error> {
    let mut lowered = BTreeMap::new();
    let mut groups = BTreeMap::<usize, Arc<SigModuleSection<Prime>>>::new();
    for (name, declaration) in pending {
        if let Some(context) = &declaration.recursive_context {
            let key = Arc::as_ptr(context) as usize;
            let context = match groups.get(&key) {
                Some(context) => context.clone(),
                None => {
                    let context = Arc::new(lower_section(context)?);
                    groups.insert(key, context.clone());
                    context
                }
            };
            let item = context_item(&context, name).ok_or_else(|| {
                Error::bridge(
                    item_span(&declaration.item),
                    format!("recursive signature context does not declare `{name}`"),
                )
            })?;
            lowered.insert(
                name.clone(),
                PendingPrimeDeclaration {
                    item,
                    imports: declaration.imports.clone(),
                    recursive_context: Some(context),
                },
            );
        } else {
            lowered.insert(
                name.clone(),
                PendingPrimeDeclaration {
                    item: lower_item(&module_path(name), &declaration.imports, &declaration.item)?,
                    imports: declaration.imports.clone(),
                    recursive_context: None,
                },
            );
        }
    }
    Ok(lowered)
}

fn lower_section(section: &SigModuleSection<Surface>) -> Result<SigModuleSection<Prime>, Error> {
    let items = section
        .items
        .iter()
        .map(sig_item_as_module_item)
        .collect::<Vec<_>>();
    let module = Module {
        path: section.path.clone(),
        imports: section.imports.clone(),
        items,
        meta: Meta::new(section.span),
        doc: None,
    };
    let lowered = crate::prime::lower::lower_module(module)?;
    let items = section
        .items
        .iter()
        .zip(lowered.items)
        .map(|(source, item)| lowered_sig_item(source, item))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SigModuleSection {
        leading_trivia: section.leading_trivia.clone(),
        trailing_trivia: section.trailing_trivia.clone(),
        path: section.path.clone(),
        imports: section.imports.clone(),
        items,
        span: section.span,
    })
}

fn lower_item(
    path: &crate::ast::ModulePath,
    imports: &[Import],
    item: &SigItem<Surface>,
) -> Result<SigItem<Prime>, Error> {
    let section = SigModuleSection {
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
        path: path.clone(),
        imports: imports.to_vec(),
        items: vec![item.clone()],
        span: item_span(item),
    };
    Ok(lower_section(&section)?
        .items
        .into_iter()
        .next()
        .expect("a one-declaration lowering stays non-empty"))
}

fn sig_item_as_module_item(item: &SigItem<Surface>) -> Item<Surface> {
    match item {
        SigItem::HostType(item) => Item::HostType(item.clone()),
        SigItem::HostFn(item) => Item::HostFn(item.clone()),
        SigItem::TypeAlias(item) => {
            let mut item = item.clone();
            item.vis = Visibility::Public;
            Item::TypeAlias(item)
        }
        SigItem::Newtype(item) => Item::Newtype(item.clone()),
        SigItem::TypeRecGroup(item) => Item::TypeRecGroup(item.clone()),
        SigItem::ExportFn(item) => Item::HostFn(item.function.clone()),
    }
}

fn lowered_sig_item(
    source: &SigItem<Surface>,
    lowered: Item<Prime>,
) -> Result<SigItem<Prime>, Error> {
    match (source, lowered) {
        (SigItem::HostType(_), Item::HostType(item)) => Ok(SigItem::HostType(item)),
        (SigItem::HostFn(_), Item::HostFn(item)) => Ok(SigItem::HostFn(item)),
        (SigItem::TypeAlias(_), Item::TypeAlias(item)) => Ok(SigItem::TypeAlias(item)),
        (SigItem::Newtype(_), Item::Newtype(item)) => Ok(SigItem::Newtype(item)),
        (SigItem::TypeRecGroup(_), Item::TypeRecGroup(item)) => Ok(SigItem::TypeRecGroup(item)),
        (SigItem::ExportFn(source), Item::HostFn(function)) => Ok(SigItem::ExportFn(SigExportFn {
            purity: source.purity,
            function,
        })),
        (_, item) => Err(Error::bridge(
            item.meta().span,
            "signature Kio' lowering changed a declaration's kind",
        )),
    }
}

fn canonicalize_section(
    section: &SigModuleSection<Prime>,
    imports: &[Import],
    candidate: &CanonicalEpoch,
    candidate_modules: &BTreeSet<String>,
) -> Result<SigModuleSection<Prime>, Error> {
    let package = validate_origin_section(section, candidate, candidate_modules)?;
    let path = path_string(&section.path);
    let owner = package
        .module(&path)
        .expect("the candidate epoch contains the declaration's owner module");
    let provider = NominalProvider::new(Some(&owner.module), Some(&package));
    let scope = provider.root();
    let mut canonical = section.clone();
    canonical.imports = comptime_import(imports).into_iter().collect();
    for item in &mut canonical.items {
        canonicalize_sig_item(item, &provider, scope)?;
    }
    Ok(canonical)
}

/// Run ordinary in-body resolution for one immutable declaration origin.
///
/// Earlier origins in the candidate epoch are already identity-qualified, so
/// replaying all of their bodies under this origin's imports would be both
/// invalid Kio syntax and historically wrong. The resolver only needs their
/// declaration heads here: body-free stand-ins retain referenced heads and
/// import-collision peers, preserving whether each name is a type or value.
/// The real origin then goes through the ordinary resolver,
/// including the shared recursive-group shape checks. Full kinds, variance,
/// positivity, and alias expansion run later over the canonical epoch.
fn validate_origin_section(
    section: &SigModuleSection<Prime>,
    candidate: &CanonicalEpoch,
    candidate_modules: &BTreeSet<String>,
) -> Result<Package<Prime>, Error> {
    let modules = resolution_modules(section, candidate, candidate_modules);
    let slice_result = build_package_from_modules(modules).and_then(|package| {
        package
            .check_in_body_resolution()
            .map_err(|error| error.error)?;
        Ok(package)
    });
    match slice_result {
        Ok(package) => Ok(package),
        Err(slice_error) => {
            // The full candidate is an error-only oracle. Valid histories must
            // stay on the indexed slice path; an accepted full package here
            // means the dependency collector missed a semantic edge.
            let full_modules = full_resolution_modules(section, candidate);
            let package = build_package_from_modules(full_modules)?;
            match package.check_in_body_resolution() {
                Err(error) => Err(error.error),
                Ok(()) => unreachable!(
                    "signature origin dependency slice rejected a section accepted by the full candidate: {slice_error:?}"
                ),
            }
        }
    }
}

fn resolution_modules(
    section: &SigModuleSection<Prime>,
    candidate: &CanonicalEpoch,
    candidate_modules: &BTreeSet<String>,
) -> Vec<(PathBuf, Module<Prime>)> {
    let origin = path_string(&section.path);
    let target_names = section
        .items
        .iter()
        .flat_map(sig_item_names)
        .map(|name| QualifiedName::new(&origin, name))
        .collect::<BTreeSet<_>>();
    let mut by_module = BTreeMap::<String, Vec<Item<Prime>>>::new();
    let mut stub_names = BTreeSet::new();
    let references = origin_type_references(&section.items);
    let mut qualified_imports = BTreeMap::<String, String>::new();

    for name in &references.bare {
        stub_names.insert(QualifiedName::new(&origin, name));
    }
    for usage in &section.imports {
        match &usage.kind {
            crate::ast::ImportKind::Selective { items, from } => {
                let provider = path_string(from);
                if candidate_modules.contains(&provider) {
                    by_module.entry(provider.clone()).or_default();
                }
                for name in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                    stub_names.insert(QualifiedName::new(&provider, name));
                    stub_names.insert(QualifiedName::new(&origin, name));
                }
            }
            crate::ast::ImportKind::Qualified { path, alias } => {
                let provider = path_string(path);
                qualified_imports.insert(alias.clone(), provider.clone());
                if candidate_modules.contains(&provider) {
                    by_module.entry(provider.clone()).or_default();
                }
                stub_names.insert(QualifiedName::new(&origin, alias));
            }
            crate::ast::ImportKind::Comptime => {
                for name in crate::comptime::PUBLIC_COMPTIME_NAMES {
                    stub_names.insert(QualifiedName::new(&origin, *name));
                }
            }
            crate::ast::ImportKind::Intrinsics => {
                for name in crate::pass::resolve::PRIME_INTRINSICS {
                    stub_names.insert(QualifiedName::new(&origin, *name));
                }
            }
        }
    }
    for path in &references.qualified {
        let (leaf, owner) = path
            .split_last()
            .expect("a qualified type reference has an owner and leaf");
        let provider = if path.len() == 2 {
            qualified_imports
                .get(&owner[0])
                .cloned()
                .unwrap_or_else(|| owner.join("/"))
        } else {
            if qualified_imports.contains_key(&owner[0]) {
                // Nominal resolution rejects an alias followed by more than
                // one segment; retain that route instead of admitting an
                // otherwise same-spelled exact module path into the slice.
                continue;
            }
            owner.join("/")
        };
        if candidate_modules.contains(&provider) {
            by_module.entry(provider.clone()).or_default();
        }
        stub_names.insert(QualifiedName::new(provider, leaf));
    }

    for name in stub_names {
        if target_names.contains(&name) {
            continue;
        }
        let Some(declaration) = candidate.get(&name) else {
            continue;
        };
        by_module
            .entry(name.module_path)
            .or_default()
            .push(resolution_stub(&declaration.item));
    }
    by_module.entry(origin.clone()).or_default().extend(
        section
            .items
            .iter()
            .cloned()
            .map(sig_item_as_prime_module_item),
    );

    by_module
        .into_iter()
        .map(|(path, items)| {
            let imports = if path == origin {
                section.imports.clone()
            } else {
                Vec::new()
            };
            (
                PathBuf::from(format!("{path}.kio")),
                Module {
                    path: module_path_from_string(&path),
                    imports,
                    items,
                    meta: Meta::new(section.span),
                    doc: None,
                },
            )
        })
        .collect()
}

fn full_resolution_modules(
    section: &SigModuleSection<Prime>,
    candidate: &CanonicalEpoch,
) -> Vec<(PathBuf, Module<Prime>)> {
    let origin = path_string(&section.path);
    let target_names = section
        .items
        .iter()
        .flat_map(sig_item_names)
        .map(|name| QualifiedName::new(&origin, name))
        .collect::<BTreeSet<_>>();
    let mut by_module = BTreeMap::<String, Vec<Item<Prime>>>::new();
    for (name, declaration) in candidate {
        if !target_names.contains(name) {
            by_module
                .entry(name.module_path.clone())
                .or_default()
                .push(resolution_stub(&declaration.item));
        }
    }
    by_module.entry(origin.clone()).or_default().extend(
        section
            .items
            .iter()
            .cloned()
            .map(sig_item_as_prime_module_item),
    );
    by_module
        .into_iter()
        .map(|(path, items)| {
            let imports = if path == origin {
                section.imports.clone()
            } else {
                Vec::new()
            };
            (
                PathBuf::from(format!("{path}.kio")),
                Module {
                    path: module_path_from_string(&path),
                    imports,
                    items,
                    meta: Meta::new(section.span),
                    doc: None,
                },
            )
        })
        .collect()
}

#[derive(Default)]
struct OriginTypeReferences {
    bare: BTreeSet<String>,
    qualified: BTreeSet<Vec<String>>,
}

fn origin_type_references(items: &[SigItem<Prime>]) -> OriginTypeReferences {
    let mut references = OriginTypeReferences::default();
    for item in items {
        collect_sig_item_origin_type_references(item, &mut references);
    }
    references
}

fn collect_sig_item_origin_type_references(
    item: &SigItem<Prime>,
    references: &mut OriginTypeReferences,
) {
    match item {
        SigItem::HostType(_) => {}
        SigItem::HostFn(function) => collect_fn_origin_type_references(function, references),
        SigItem::ExportFn(export) => {
            collect_fn_origin_type_references(&export.function, references)
        }
        SigItem::TypeAlias(alias) => {
            let mut bound = alias
                .type_params
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect::<BTreeSet<_>>();
            collect_origin_type_references(&alias.body, &mut bound, references);
        }
        SigItem::Newtype(newtype) => {
            let mut bound = newtype
                .type_params
                .iter()
                .chain(&newtype.existential_params)
                .map(|parameter| parameter.name.clone())
                .collect::<BTreeSet<_>>();
            collect_origin_type_references(&newtype.payload, &mut bound, references);
        }
        SigItem::TypeRecGroup(group) => {
            for member in &group.members {
                match member {
                    TypeRecMember::TypeAlias(alias) => {
                        let mut bound = alias
                            .type_params
                            .iter()
                            .map(|parameter| parameter.name.clone())
                            .collect::<BTreeSet<_>>();
                        collect_origin_type_references(&alias.body, &mut bound, references);
                    }
                    TypeRecMember::Newtype(newtype) => {
                        let mut bound = newtype
                            .type_params
                            .iter()
                            .chain(&newtype.existential_params)
                            .map(|parameter| parameter.name.clone())
                            .collect::<BTreeSet<_>>();
                        collect_origin_type_references(&newtype.payload, &mut bound, references);
                    }
                    TypeRecMember::Labels(_, extension) => match *extension {},
                }
            }
        }
    }
}

fn collect_fn_origin_type_references(
    function: &HostFn<Prime>,
    references: &mut OriginTypeReferences,
) {
    let mut bound = BTreeSet::new();
    for parameter in &function.params {
        match parameter {
            HostFnParam::Type(parameter) => {
                bound.insert(parameter.name.clone());
            }
            HostFnParam::Value(parameter) => {
                collect_origin_type_references(&parameter.ty, &mut bound, references);
            }
        }
    }
    collect_origin_type_references(&function.ret, &mut bound, references);
}

fn collect_origin_type_references(
    ty: &Type<Prime>,
    bound: &mut BTreeSet<String>,
    references: &mut OriginTypeReferences,
) {
    match ty {
        Type::Path { segments, args, .. } => {
            if let [segment] = segments.as_slice() {
                if !bound.contains(&segment.name) {
                    references.bare.insert(segment.name.clone());
                }
            } else {
                references.qualified.insert(
                    segments
                        .iter()
                        .map(|segment| segment.name.clone())
                        .collect(),
                );
            }
            for argument in args {
                collect_origin_type_references(argument, bound, references);
            }
        }
        Type::Function { param, ret, .. } => {
            collect_origin_type_references(param, bound, references);
            collect_origin_type_references(ret, bound, references);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_origin_type_references(left, bound, references);
            collect_origin_type_references(right, bound, references);
        }
        Type::Forall { param, body, .. } => {
            let inserted = bound.insert(param.name.clone());
            collect_origin_type_references(body, bound, references);
            if inserted {
                bound.remove(&param.name);
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } => {}
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn resolution_stub(item: &SigItem<Prime>) -> Item<Prime> {
    let unit = |span| Type::Unit {
        meta: Meta::new(span),
    };
    match item {
        SigItem::HostType(item) => Item::HostType(item.clone()),
        SigItem::HostFn(item) | SigItem::ExportFn(SigExportFn { function: item, .. }) => {
            let mut item = item.clone();
            for parameter in &mut item.params {
                if let HostFnParam::Value(parameter) = parameter {
                    parameter.ty = unit(parameter.meta.span);
                }
            }
            item.ret = unit(item.meta.span);
            Item::HostFn(item)
        }
        SigItem::TypeAlias(item) => {
            let mut item = item.clone();
            item.body = unit(item.meta.span);
            Item::TypeAlias(item)
        }
        SigItem::Newtype(item) => {
            let mut item = item.clone();
            item.rec_span = None;
            item.payload = unit(item.meta.span);
            Item::Newtype(item)
        }
        SigItem::TypeRecGroup(_) => {
            unreachable!("a canonical epoch stores recursive-group members individually")
        }
    }
}

fn sig_item_names(item: &SigItem<Prime>) -> Vec<&str> {
    match item {
        SigItem::TypeRecGroup(group) => group.members.iter().map(type_rec_member_name).collect(),
        item => vec![sig_item_name(item)],
    }
}

fn canonicalize_sig_item<'a>(
    item: &mut SigItem<Prime>,
    provider: &NominalProvider<'a, Prime>,
    scope: crate::pass::resolve::NominalScope<'a, Prime>,
) -> Result<(), Error> {
    match item {
        SigItem::HostType(_) => Ok(()),
        SigItem::HostFn(item) => canonicalize_host_fn(item, provider, scope),
        SigItem::TypeAlias(item) => {
            let mut bound = item
                .type_params
                .iter()
                .map(|param| param.name.clone())
                .collect();
            canonicalize_type(&mut item.body, &mut bound, provider, scope)
        }
        SigItem::Newtype(item) => {
            let mut bound = item
                .type_params
                .iter()
                .chain(&item.existential_params)
                .map(|param| param.name.clone())
                .collect();
            canonicalize_type(&mut item.payload, &mut bound, provider, scope)
        }
        SigItem::TypeRecGroup(group) => {
            for member in &mut group.members {
                match member {
                    TypeRecMember::TypeAlias(item) => {
                        let mut bound = item
                            .type_params
                            .iter()
                            .map(|param| param.name.clone())
                            .collect();
                        canonicalize_type(&mut item.body, &mut bound, provider, scope)?;
                    }
                    TypeRecMember::Newtype(item) => {
                        let mut bound = item
                            .type_params
                            .iter()
                            .chain(&item.existential_params)
                            .map(|param| param.name.clone())
                            .collect();
                        canonicalize_type(&mut item.payload, &mut bound, provider, scope)?;
                    }
                    TypeRecMember::Labels(_, ext) => match *ext {},
                }
            }
            Ok(())
        }
        SigItem::ExportFn(item) => canonicalize_host_fn(&mut item.function, provider, scope),
    }
}

fn canonicalize_host_fn<'a>(
    function: &mut HostFn<Prime>,
    provider: &NominalProvider<'a, Prime>,
    scope: crate::pass::resolve::NominalScope<'a, Prime>,
) -> Result<(), Error> {
    let mut bound = Vec::new();
    for param in &mut function.params {
        match param {
            HostFnParam::Type(param) => bound.push(param.name.clone()),
            HostFnParam::Value(param) => {
                canonicalize_type(&mut param.ty, &mut bound, provider, scope)?
            }
        }
    }
    canonicalize_type(&mut function.ret, &mut bound, provider, scope)
}

fn canonicalize_type<'a>(
    ty: &mut Type<Prime>,
    bound: &mut Vec<String>,
    provider: &NominalProvider<'a, Prime>,
    scope: crate::pass::resolve::NominalScope<'a, Prime>,
) -> Result<(), Error> {
    match ty {
        Type::Path {
            segments,
            args,
            meta,
        } => {
            for argument in args {
                canonicalize_type(argument, bound, provider, scope)?;
            }
            if matches!(segments.as_slice(), [head] if bound.iter().rev().any(|name| name == head.as_str()))
            {
                return Ok(());
            }
            let written = segments
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>()
                .join(".");
            let comptime_type = segments.len() == 1
                && scope.module().is_some_and(|module| {
                    module
                        .imports
                        .iter()
                        .any(|usage| matches!(usage.kind, crate::ast::ImportKind::Comptime))
                })
                && crate::comptime::ComptimeBuiltin::from_public_name(segments[0].as_str())
                    .is_some_and(crate::comptime::ComptimeBuiltin::is_type_name);
            if comptime_type {
                return Ok(());
            }
            let (qualified, canonical) = provider.qualify(scope, segments, false);
            if !canonical
                || !matches!(
                    provider.select(scope, &qualified, canonical),
                    NominalSelection::Selected(_)
                )
            {
                return Err(Error::type_(
                    meta.span,
                    format!(
                        "type `{written}` does not resolve to a declaration in this signature version"
                    ),
                ));
            }
            *segments = qualified;
            Ok(())
        }
        Type::Function { param, ret, .. } => {
            canonicalize_type(param, bound, provider, scope)?;
            canonicalize_type(ret, bound, provider, scope)
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            canonicalize_type(left, bound, provider, scope)?;
            canonicalize_type(right, bound, provider, scope)
        }
        Type::Forall { param, body, .. } => {
            bound.push(param.name.clone());
            let result = canonicalize_type(body, bound, provider, scope);
            bound.pop();
            result
        }
        Type::Unit { .. } | Type::Bottom { .. } => Ok(()),
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn build_package(
    epoch: &CanonicalEpoch,
    origin_path: Option<&crate::ast::ModulePath>,
    origin_imports: &[Import],
) -> Result<Package<Prime>, Error> {
    let origin = origin_path.map(path_string);
    let modules = epoch_modules(epoch, origin.as_deref(), origin_imports);
    build_package_from_modules(modules)
}

fn build_package_from_modules(
    modules: Vec<(PathBuf, Module<Prime>)>,
) -> Result<Package<Prime>, Error> {
    #[cfg(test)]
    ACTIVE_CANONICALIZATION_WORK.with(|slot| {
        if let Some(work) = slot.borrow_mut().as_mut() {
            work.package_builds += 1;
            work.package_items += modules
                .iter()
                .map(|(_, module)| module.items.len())
                .sum::<usize>();
        }
    });
    #[cfg(any(feature = "surface", feature = "cli"))]
    let (package, name_errors) =
        Package::build_deferring_contract_checks(Path::new("."), modules, None)
            .map_err(|error| error.error)?;
    #[cfg(not(any(feature = "surface", feature = "cli")))]
    let package = Package::build(Path::new("."), modules, None).map_err(|error| error.error)?;
    package.resolve_imports().map_err(|error| error.error)?;
    #[cfg(any(feature = "surface", feature = "cli"))]
    if let Some(error) = name_errors.into_iter().next() {
        return Err(error.error);
    }
    package
        .check_binding_origins()
        .map_err(|error| error.error)?;
    Ok(package)
}

fn epoch_modules(
    epoch: &CanonicalEpoch,
    origin_path: Option<&str>,
    origin_imports: &[Import],
) -> Vec<(PathBuf, Module<Prime>)> {
    let declarations = || {
        let entries = epoch.iter();
        #[cfg(test)]
        let entries = entries.inspect(|_| {
            EPOCH_MODULE_DECLARATION_VISITS.with(|visits| visits.set(visits.get() + 1));
        });
        entries
    };
    let mut by_module = BTreeMap::<String, Vec<Item<Prime>>>::new();
    let mut comptime_imports = BTreeMap::new();
    let mut emitted_contexts = BTreeSet::new();
    for (name, declaration) in declarations() {
        if let Some(usage) = &declaration.comptime_import {
            comptime_imports
                .entry(name.module_path.as_str())
                .or_insert(usage);
        }
        if let Some(context) = &declaration.recursive_context {
            let key = Arc::as_ptr(context) as usize;
            if !emitted_contexts.insert(key) {
                continue;
            }
            for item in filtered_context_items(context, epoch, key) {
                by_module
                    .entry(name.module_path.clone())
                    .or_default()
                    .push(sig_item_as_prime_module_item(item));
            }
        } else {
            by_module
                .entry(name.module_path.clone())
                .or_default()
                .push(sig_item_as_prime_module_item(declaration.item.clone()));
        }
    }
    by_module
        .into_iter()
        .map(|(path, items)| {
            let module_path = module_path_from_string(&path);
            let imports = if origin_path == Some(path.as_str()) {
                origin_imports.to_vec()
            } else {
                comptime_imports
                    .get(path.as_str())
                    .map(|usage| (*usage).clone())
                    .into_iter()
                    .collect()
            };
            (
                PathBuf::from(format!("{path}.kio")),
                Module {
                    path: module_path,
                    imports,
                    items,
                    meta: Meta::new(Span::new(0, 0)),
                    doc: None,
                },
            )
        })
        .collect()
}

#[cfg(test)]
thread_local! {
    static EPOCH_MODULE_DECLARATION_VISITS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
pub(super) fn take_epoch_module_declaration_visits() -> usize {
    EPOCH_MODULE_DECLARATION_VISITS.with(|visits| visits.replace(0))
}

fn filtered_context_items(
    context: &SigModuleSection<Prime>,
    epoch: &CanonicalEpoch,
    context_key: usize,
) -> Vec<SigItem<Prime>> {
    context
        .items
        .iter()
        .filter_map(|item| match item {
            SigItem::TypeRecGroup(group) => {
                let mut group = group.clone();
                group.members.retain(|member| {
                    let name = QualifiedName::new(
                        path_string(&context.path),
                        type_rec_member_name(member),
                    );
                    epoch.get(&name).is_some_and(|declaration| {
                        declaration
                            .recursive_context
                            .as_ref()
                            .is_some_and(|candidate| Arc::as_ptr(candidate) as usize == context_key)
                    })
                });
                (!group.members.is_empty()).then_some(SigItem::TypeRecGroup(group))
            }
            item => {
                let name = QualifiedName::new(path_string(&context.path), sig_item_name(item));
                epoch
                    .get(&name)
                    .is_some_and(|declaration| {
                        declaration
                            .recursive_context
                            .as_ref()
                            .is_some_and(|candidate| Arc::as_ptr(candidate) as usize == context_key)
                    })
                    .then(|| item.clone())
            }
        })
        .collect()
}

fn install_context(epoch: &mut CanonicalEpoch, context: &SigModuleSection<Prime>) {
    let context = Arc::new(context.clone());
    let comptime_import = comptime_import(&context.imports);
    for item in &context.items {
        match item {
            SigItem::TypeRecGroup(group) => {
                for member in &group.members {
                    let item = match member {
                        TypeRecMember::TypeAlias(item) => SigItem::TypeAlias(item.clone()),
                        TypeRecMember::Newtype(item) => SigItem::Newtype(item.clone()),
                        TypeRecMember::Labels(_, ext) => match *ext {},
                    };
                    let name = QualifiedName::new(path_string(&context.path), sig_item_name(&item));
                    epoch.insert(
                        name,
                        CanonicalDeclaration {
                            item,
                            recursive_context: Some(context.clone()),
                            comptime_import: comptime_import.clone(),
                        },
                    );
                }
            }
            item => {
                let name = QualifiedName::new(path_string(&context.path), sig_item_name(item));
                epoch.insert(
                    name,
                    CanonicalDeclaration {
                        item: item.clone(),
                        recursive_context: Some(context.clone()),
                        comptime_import: comptime_import.clone(),
                    },
                );
            }
        }
    }
}

fn comptime_import(imports: &[Import]) -> Option<Import> {
    imports
        .iter()
        .find(|usage| matches!(usage.kind, crate::ast::ImportKind::Comptime))
        .cloned()
}

fn context_item(context: &SigModuleSection<Prime>, name: &QualifiedName) -> Option<SigItem<Prime>> {
    for item in &context.items {
        match item {
            SigItem::TypeRecGroup(group) => {
                for member in &group.members {
                    let item = match member {
                        TypeRecMember::TypeAlias(item) => SigItem::TypeAlias(item.clone()),
                        TypeRecMember::Newtype(item) => SigItem::Newtype(item.clone()),
                        TypeRecMember::Labels(_, ext) => match *ext {},
                    };
                    if sig_item_name(&item) == name.leaf {
                        return Some(item);
                    }
                }
            }
            item if sig_item_name(item) == name.leaf => return Some(item.clone()),
            _ => {}
        }
    }
    None
}

fn sig_item_as_prime_module_item(item: SigItem<Prime>) -> Item<Prime> {
    match item {
        SigItem::HostType(item) => Item::HostType(item),
        SigItem::HostFn(item) => Item::HostFn(item),
        SigItem::TypeAlias(item) => Item::TypeAlias(item),
        SigItem::Newtype(item) => Item::Newtype(item),
        SigItem::TypeRecGroup(item) => Item::TypeRecGroup(item),
        SigItem::ExportFn(item) => Item::HostFn(item.function),
    }
}

fn sig_item_name<P: crate::ast::Phase>(item: &SigItem<P>) -> &str {
    match item {
        SigItem::HostType(item) => &item.name,
        SigItem::HostFn(item) => &item.name,
        SigItem::TypeAlias(item) => &item.name,
        SigItem::Newtype(item) => &item.name,
        SigItem::ExportFn(item) => &item.function.name,
        SigItem::TypeRecGroup(group) => group
            .members
            .first()
            .map(type_rec_member_name)
            .unwrap_or(""),
    }
}

fn type_rec_member_name<P: crate::ast::Phase>(member: &TypeRecMember<P>) -> &str {
    match member {
        TypeRecMember::TypeAlias(item) => &item.name,
        TypeRecMember::Newtype(item) => &item.name,
        TypeRecMember::Labels(item, _) => item.type_alias_name.as_deref().unwrap_or(""),
    }
}

fn item_span<P: crate::ast::Phase>(item: &SigItem<P>) -> Span {
    match item {
        SigItem::HostType(item) => item.meta.span,
        SigItem::HostFn(item) => item.meta.span,
        SigItem::TypeAlias(item) => item.meta.span,
        SigItem::Newtype(item) => item.meta.span,
        SigItem::TypeRecGroup(item) => item.meta.span,
        SigItem::ExportFn(item) => item.function.meta.span,
    }
}

fn module_path(name: &QualifiedName) -> crate::ast::ModulePath {
    module_path_from_string(&name.module_path)
}

fn module_path_from_string(path: &str) -> crate::ast::ModulePath {
    crate::ast::ModulePath {
        segments: path
            .split('/')
            .map(|segment| crate::ast::PathSegment::synth(segment, Span::new(0, 0)))
            .collect(),
        span: Span::new(0, 0),
    }
}

fn path_string(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(crate::ast::PathSegment::as_str)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    fn compare_origin_with_full_epoch(
        section: &SigModuleSection<Prime>,
        candidate: &CanonicalEpoch,
        accepted: bool,
    ) {
        let candidate_modules = candidate
            .keys()
            .map(|name| name.module_path.clone())
            .collect();
        let sliced = canonicalize_section(section, &section.imports, candidate, &candidate_modules);
        let full = (|| {
            let resolution =
                build_package_from_modules(full_resolution_modules(section, candidate))?;
            resolution
                .check_in_body_resolution()
                .map_err(|error| error.error)?;
            let package = build_package(candidate, Some(&section.path), &section.imports)?;
            let path = path_string(&section.path);
            let owner = package.module(&path).expect("full epoch contains origin");
            let provider = NominalProvider::new(Some(&owner.module), Some(&package));
            let mut canonical = section.clone();
            canonical.imports = comptime_import(&section.imports).into_iter().collect();
            for item in &mut canonical.items {
                canonicalize_sig_item(item, &provider, provider.root())?;
            }
            Ok::<_, Error>(canonical)
        })();
        assert_eq!(full.is_ok(), accepted, "full origin result: {full:?}");
        assert_eq!(
            sliced.map_err(|error| format!("{error:?}")),
            full.map_err(|error| format!("{error:?}")),
            "origin: {section:?}"
        );
    }

    #[test]
    fn fresh_origin_slice_matches_full_epoch_resolution_and_qualification() {
        let cases = [
            (
                "bare dependency",
                "module api { host type Raw; pub type Wrapped = Raw; }",
                true,
            ),
            (
                "selective import",
                "module dep { host type Raw; host type Other; }; module api { import dep(Raw); pub fn wrap(value: Raw) -> Raw; }",
                true,
            ),
            (
                "qualified import",
                "module dep { host type Raw; }; module api { import dep as d; pub type Wrapped = d.Raw; }",
                true,
            ),
            (
                "unimported nested owner",
                "module foo/bar { host type Raw; }; module api { pub type Wrapped = foo/bar.Raw; }",
                false,
            ),
            (
                "imported nested owner",
                "module foo/bar { host type Raw; }; module api { import foo/bar as nested; pub type Wrapped = nested.Raw; }",
                true,
            ),
            (
                "alias precedence",
                "module dep { host type Raw; }; module other { host type Raw; }; module api { import dep as other; pub type Wrapped = other.Raw; }",
                true,
            ),
            (
                "generic shadowing",
                "module dep { host type Raw; }; module api { import dep(Raw); host fn choose[Raw](value: Raw) -> Raw; }",
                true,
            ),
            (
                "unused written import",
                "module dep { host type Raw; }; module api { import dep(Raw); pub type Wrapped = .; }",
                true,
            ),
            (
                "newtype payload",
                "module dep { host type Raw; }; module api { import dep(Raw); pub newtype Wrapped : Raw { pub constructor mk; pub projector get; }; }",
                true,
            ),
            (
                "higher kind and forall",
                "module dep { host type Raw; }; module api { import dep(Raw); pub type Wrapped[*F, A] = [Raw] F(Raw) -> A; }",
                true,
            ),
            (
                "missing module",
                "module dep { host type Raw; }; module api { import absent(Raw); pub type Wrapped = Raw; }",
                false,
            ),
            (
                "missing import",
                "module dep { host type Raw; }; module api { import dep(Missing); pub type Wrapped = .; }",
                false,
            ),
            (
                "missing local type",
                "module dep { host type Raw; }; module api { pub type Wrapped = Raw; }",
                false,
            ),
            (
                "missing qualified type",
                "module dep { host type Raw; }; module api { import dep as d; pub type Wrapped = d.Missing; }",
                false,
            ),
            (
                "local selective collision",
                "module dep { host type Raw; }; module api { import dep(Raw); host type Raw; pub type Wrapped = .; }",
                false,
            ),
            (
                "local alias collision",
                "module dep { host type Raw; }; module api { import dep as d; host fn d() -> .; pub type Wrapped = .; }",
                false,
            ),
            (
                "conflicting aliases",
                "module dep { host type Raw; }; module other { host type Raw; }; module api { import dep as d; import other as d; pub type Wrapped = .; }",
                false,
            ),
            (
                "alias suffix boundary",
                "module dep { host type Raw; }; module d/nested { host type Raw; }; module api { import dep as d; pub type Wrapped = d/nested.Raw; }",
                false,
            ),
            (
                "unimported comptime type",
                "module api { pub type Wrapped = __Type__; }",
                false,
            ),
            (
                "imported comptime type",
                "module api { import __comptime__; pub type Wrapped = __Type__; }",
                true,
            ),
        ];
        for (description, modules, accepted) in cases {
            let source =
                format!("signature app v(1); v(1) {{ nonbreaking {{ add {{ {modules} }} }} }}");
            let parsed = crate::pass::parser::parse_signature_file(&source, Some("app"))
                .unwrap_or_else(|error| panic!("{description}: {error:?}"));
            let sections = parsed.versions[0]
                .nonbreaking
                .as_ref()
                .expect("fixture change")
                .add
                .iter()
                .map(lower_section)
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_else(|error| panic!("{description}: {error:?}"));
            let origin = sections
                .iter()
                .find(|section| path_string(&section.path) == "api")
                .expect("fixture origin");
            let mut candidate = CanonicalEpoch::new();
            for section in &sections {
                for item in &section.items {
                    candidate.insert(
                        QualifiedName::new(path_string(&section.path), sig_item_name(item)),
                        CanonicalDeclaration {
                            item: item.clone(),
                            recursive_context: None,
                            comptime_import: comptime_import(&section.imports),
                        },
                    );
                }
            }
            for item in &origin.items {
                let section = SigModuleSection {
                    items: vec![item.clone()],
                    ..origin.clone()
                };
                compare_origin_with_full_epoch(&section, &candidate, accepted);
            }
        }
    }

    #[test]
    fn fresh_origin_slice_matches_full_epoch_recursive_context() {
        let source = "signature app v(1); v(1) {
          with { module api { rec {
            pub type A = . | B;
            pub newtype B : A { pub constructor mk; pub projector get; };
          } } };
          nonbreaking { add { api.A; api.B; } }
        }";
        let parsed = crate::pass::parser::parse_signature_file(source, Some("app"))
            .expect("parse recursive origin");
        let context = lower_section(&parsed.versions[0].with[0]).expect("lower recursive origin");
        let mut candidate = CanonicalEpoch::new();
        install_context(&mut candidate, &context);
        compare_origin_with_full_epoch(&context, &candidate, true);
    }

    #[test]
    fn epoch_modules_preserve_first_comptime_import_and_origin_override() {
        let mut epoch = recursive_epoch(3, 1, false);
        let usage = |start| Import {
            trailing_trivia: Vec::new(),
            kind: crate::ast::ImportKind::Comptime,
            span: Span::new(start, start + 1),
            leading_trivia: Vec::new(),
        };
        let first = usage(100);
        epoch
            .get_mut(&QualifiedName::new("api", "N1"))
            .expect("second member")
            .comptime_import = Some(first.clone());
        epoch
            .get_mut(&QualifiedName::new("api", "N2"))
            .expect("third member")
            .comptime_import = Some(usage(200));

        let mut foreign = epoch[&QualifiedName::new("api", "N0")].clone();
        foreign.recursive_context = None;
        foreign.comptime_import = Some(usage(300));
        epoch.insert(QualifiedName::new("before", "N0"), foreign);

        let mut empty = epoch[&QualifiedName::new("api", "N0")].clone();
        empty.comptime_import = Some(usage(400));
        empty.recursive_context = Some(Arc::new(SigModuleSection {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            path: module_path_from_string("empty"),
            imports: Vec::new(),
            items: Vec::new(),
            span: Span::new(0, 0),
        }));
        epoch.insert(QualifiedName::new("empty", "N0"), empty);

        let render = |origin, imports: &[Import]| {
            epoch_modules(&epoch, origin, imports)
                .into_iter()
                .map(|(_, module)| (path_string(&module.path), module))
                .collect::<BTreeMap<_, _>>()
        };
        let modules = render(None, &[]);
        assert_eq!(
            modules.keys().map(String::as_str).collect::<Vec<_>>(),
            ["api", "before"]
        );
        assert_eq!(modules["api"].imports, [first]);
        assert_eq!(modules["before"].imports, [usage(300)]);
        let [Item::TypeRecGroup(group)] = modules["api"].items.as_slice() else {
            panic!("one recursive context emits exactly one group")
        };
        assert_eq!(group.members.len(), 3);

        let override_imports = [usage(500)];
        let overridden = render(Some("api"), &override_imports);
        assert_eq!(overridden["api"].imports, override_imports);
        assert_eq!(overridden["before"].imports, modules["before"].imports);
        assert!(render(Some("api"), &[])["api"].imports.is_empty());
        assert!(epoch_modules(&CanonicalEpoch::new(), None, &override_imports).is_empty());
    }

    fn recursive_epoch(
        cycle_members: usize,
        parameters: usize,
        include_trailing_acyclic_member: bool,
    ) -> CanonicalEpoch {
        let binders = (0..parameters)
            .map(|index| format!("[A{index}]"))
            .collect::<String>();
        let arguments = (0..parameters)
            .map(|index| format!("A{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut source = String::from("module api; rec {");
        for index in 0..cycle_members {
            let next = (index + 1) % cycle_members;
            write!(
                source,
                " pub newtype N{index}{binders} : api.N{next}({arguments}) {{ pub constructor mk_n{index}; pub projector un_n{index}; }};"
            )
            .expect("write recursive type member");
        }
        if include_trailing_acyclic_member {
            source.push_str(
                " pub newtype Zextra : . { pub constructor mk_zextra; pub projector un_zextra; };",
            );
        }
        source.push_str(" }");

        let parsed = crate::pass::parser::parse(&source).expect("parse recursive epoch");
        let lowered = crate::prime::lower::lower_module(parsed).expect("lower recursive epoch");
        let path = lowered.path;
        let [Item::TypeRecGroup(group)] = lowered.items.as_slice() else {
            panic!("recursive epoch fixture must lower to one type group")
        };
        let context = Arc::new(SigModuleSection {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            path: path.clone(),
            imports: Vec::new(),
            items: vec![SigItem::TypeRecGroup(group.clone())],
            span: group.meta.span,
        });

        group
            .members
            .iter()
            .cloned()
            .map(|member| {
                let leaf = type_rec_member_name(&member).to_owned();
                let item = match member {
                    TypeRecMember::TypeAlias(alias) => SigItem::TypeAlias(alias),
                    TypeRecMember::Newtype(newtype) => SigItem::Newtype(newtype),
                    TypeRecMember::Labels(_, ext) => match ext {},
                };
                (
                    QualifiedName::new("api", leaf),
                    CanonicalDeclaration {
                        item,
                        recursive_context: Some(Arc::clone(&context)),
                        comptime_import: None,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn recursive_context_partition_scan_fires_linearly_at_the_production_seam() {
        const MEMBERS: usize = 128;
        const PARAMETERS: usize = 32;
        let epoch = recursive_epoch(MEMBERS, PARAMETERS, true);

        crate::pass::resolve::reset_type_rec_linear_work_counts();
        reset_recursive_context_member_comparisons();
        let error = validate_recursive_context_partition(&epoch)
            .expect_err("an over-broad recursive context must be rejected");
        assert!(
            error
                .diag()
                .1
                .contains("does not match one complete recursive component"),
            "got: {error:?}"
        );
        assert_eq!(recursive_context_member_comparisons(), MEMBERS);
        assert_eq!(
            crate::pass::resolve::type_rec_linear_work_counts(),
            (MEMBERS * PARAMETERS, MEMBERS + 1)
        );

        let mut nested_scan_comparisons = 0;
        for member in 0..=MEMBERS {
            for candidate in 0..MEMBERS {
                nested_scan_comparisons += 1;
                if member == candidate {
                    break;
                }
            }
        }
        assert_eq!(nested_scan_comparisons, MEMBERS * (MEMBERS + 3) / 2);
        assert!(nested_scan_comparisons > recursive_context_member_comparisons() * 60);
    }

    #[test]
    fn exact_recursive_context_bypasses_the_difference_scan() {
        const MEMBERS: usize = 128;
        const PARAMETERS: usize = 32;
        let epoch = recursive_epoch(MEMBERS, PARAMETERS, false);

        crate::pass::resolve::reset_type_rec_linear_work_counts();
        reset_recursive_context_member_comparisons();
        validate_recursive_context_partition(&epoch).expect("the exact recursive context is valid");
        assert_eq!(recursive_context_member_comparisons(), 0);
        assert_eq!(
            crate::pass::resolve::type_rec_linear_work_counts(),
            (MEMBERS * PARAMETERS, MEMBERS)
        );
    }
}
