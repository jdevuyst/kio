//! Project a typechecked package's contract surface into the
//! `*.sig.kio` *declaration* form — the `SigItem<Surface>` nodes a
//! recorded `add` / `modify` writes into the changelog, plus the
//! per-module `import` clauses that resolve their references.
//!
//! [`super::ContractSnapshot::from_package`] projects the same surface
//! into *normalized strings* for the compat comparison; this module
//! produces the *declarations themselves*, so a `kio sig` write can
//! record a live item's frozen, backend-independent Kio′ signature.
//!
//! The declarations are the bridged modules' `pub` items, phase-cast to
//! `Surface` (via `crate::ast::convert_*`) and paired with the defining
//! module's own `import` clauses that its selected declarations reference —
//! exactly the clauses [`super::replay`]'s section qualifier needs, without
//! body-only lowering imports. A recorded section therefore round-trips to
//! the identical normalized form the live snapshot produced. A `host type`'s
//! source-compatible `{ owned }` annotation is dropped because it selects no
//! contract surface and the sig is backend-independent.

use super::{ContractSide, QualifiedName};
use crate::ast::{
    Import, ImportItem, ImportKind, Phase, Purity, SigItem, SigItemRef, SigModuleSection, Surface,
    TypeRecMember,
};
use crate::pass::resolve::{ExportContractPhase, Package};
use crate::span::Span;
use std::collections::BTreeMap;

/// One contract item in recorded form: its contract side plus the
/// `SigItem<Surface>` declaration to write into the changelog.
#[derive(Debug, Clone)]
pub struct RecordedItem {
    pub side: ContractSide,
    pub item: SigItem<Surface>,
}

/// The recorded-surface projection of a package: every contract item's
/// declaration, keyed by module-qualified name, plus each contributing
/// module's `import` clauses (so a recorded section carries the clauses
/// that resolve its references).
#[derive(Debug, Clone, Default)]
pub struct RecordedSurface {
    pub items: BTreeMap<QualifiedName, RecordedItem>,
    /// Module slash-path → that module's `import` clauses.
    pub module_imports: BTreeMap<String, Vec<Import>>,
    /// Exact public member name → the projected recursive declaration group
    /// that must appear once in a version-leading `with` block whenever that
    /// member is added or modified.
    pub recursive_contexts: BTreeMap<QualifiedName, RecordedTypeContext>,
}

#[derive(Debug, Clone)]
pub struct RecordedTypeContext {
    pub(super) key: String,
    pub(super) section: SigModuleSection<Surface>,
}

impl RecordedSurface {
    /// Build the recorded-surface projection of a typechecked package.
    pub fn from_package<P>(package: &Package<P>) -> Self
    where
        P: ExportContractPhase + Phase<FnPurity = Purity> + Clone,
    {
        let mut items = BTreeMap::new();
        let mut module_imports = BTreeMap::new();
        let mut recursive_contexts = BTreeMap::new();
        for module in super::surface::public_modules(package) {
            if module.declarations.is_empty() {
                continue;
            }
            let mut projected_items = Vec::new();
            for declaration in module.declarations {
                projected_items.push(declaration.item.clone());
                items.insert(
                    declaration.entry.name,
                    RecordedItem {
                        side: declaration.entry.side,
                        item: declaration.item,
                    },
                );
            }
            let imports = &module.origin.module.imports;
            module_imports.insert(
                module.path.to_owned(),
                imports_needed_by(&projected_items, imports.clone()),
            );
            for item in module.recursive_contexts {
                let group_imports = imports_needed_by(std::slice::from_ref(&item), imports.clone());
                let member_names = match &item {
                    SigItem::TypeRecGroup(group) => group
                        .members
                        .iter()
                        .filter_map(type_rec_member_name)
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                    SigItem::Newtype(newtype) if newtype.rec_span.is_some() => {
                        vec![newtype.name.clone()]
                    }
                    _ => unreachable!("only cyclic declarations have signature contexts"),
                };
                let key = format!("{}:{}", module.path, member_names.join(","));
                let section = SigModuleSection {
                    leading_trivia: Vec::new(),
                    trailing_trivia: Vec::new(),
                    path: module_path_of(module.path),
                    imports: group_imports,
                    items: vec![item],
                    span: Span::new(0, 0),
                };
                for member in member_names {
                    recursive_contexts.insert(
                        QualifiedName::new(module.path, member),
                        RecordedTypeContext {
                            key: key.clone(),
                            section: section.clone(),
                        },
                    );
                }
            }
        }
        RecordedSurface {
            items,
            module_imports,
            recursive_contexts,
        }
    }

    /// Assemble the `add` / `modify` module sections for the given set
    /// of qualified names, grouped by module in module-path order. Each
    /// section carries the recorded `import` clauses its items need. Names not
    /// present in the recorded surface (e.g. a removed item) are
    /// skipped — the caller routes those through a `remove` block.
    pub fn sections_for<'a>(
        &self,
        names: impl IntoIterator<Item = &'a QualifiedName>,
    ) -> Vec<SigModuleSection<Surface>> {
        let mut by_module: BTreeMap<String, Vec<&QualifiedName>> = BTreeMap::new();
        for name in names {
            if self.items.contains_key(name) && !self.recursive_contexts.contains_key(name) {
                by_module
                    .entry(name.module_path.clone())
                    .or_default()
                    .push(name);
            }
        }
        let mut sections = Vec::new();
        for (module_path, mut names) in by_module {
            names.sort();
            let items: Vec<SigItem<Surface>> = names
                .iter()
                .map(|name| self.items[*name].item.clone())
                .collect();
            let mut imports = self
                .module_imports
                .get(&module_path)
                .cloned()
                .unwrap_or_default();
            imports = imports_needed_by(&items, imports);
            sections.push(SigModuleSection {
                leading_trivia: Vec::new(),
                trailing_trivia: Vec::new(),
                path: module_path_of(&module_path),
                imports,
                items,
                span: Span::new(0, 0),
            });
        }
        sections
    }

    pub fn refs_for<'a>(
        &self,
        names: impl IntoIterator<Item = &'a QualifiedName>,
    ) -> Vec<SigItemRef> {
        let mut refs = names
            .into_iter()
            .filter(|name| self.recursive_contexts.contains_key(*name))
            .map(|name| SigItemRef {
                leading_trivia: Vec::new(),
                path: module_path_of(&name.module_path),
                name: name.leaf.clone(),
                span: Span::new(0, 0),
            })
            .collect::<Vec<_>>();
        refs.sort_by(|left, right| {
            module_path_string(&left.path)
                .cmp(&module_path_string(&right.path))
                .then_with(|| left.name.cmp(&right.name))
        });
        refs
    }

    pub fn contexts_for<'a>(
        &self,
        names: impl IntoIterator<Item = &'a QualifiedName>,
    ) -> Vec<SigModuleSection<Surface>> {
        let mut selected = BTreeMap::new();
        for name in names {
            if let Some(context) = self.recursive_contexts.get(name) {
                selected
                    .entry(context.key.clone())
                    .or_insert_with(|| context.section.clone());
            }
        }

        let mut by_module: BTreeMap<String, SigModuleSection<Surface>> = BTreeMap::new();
        for section in selected.into_values() {
            let module = module_path_string(&section.path);
            let combined = by_module.entry(module).or_insert_with(|| SigModuleSection {
                leading_trivia: Vec::new(),
                trailing_trivia: Vec::new(),
                path: section.path.clone(),
                imports: Vec::new(),
                items: Vec::new(),
                span: Span::new(0, 0),
            });
            combined.imports.extend(section.imports);
            combined.items.extend(section.items);
        }
        for section in by_module.values_mut() {
            let relevant = imports_needed_by(&section.items, std::mem::take(&mut section.imports));
            let mut unique = Vec::new();
            for usage in relevant {
                if !unique.contains(&usage) {
                    unique.push(usage);
                }
            }
            section.imports = unique;
        }
        by_module.into_values().collect()
    }
}

#[derive(Default)]
struct TypeReferences {
    bare: std::collections::BTreeSet<String>,
    qualifiers: std::collections::BTreeSet<String>,
}

pub(super) fn imports_needed_by(items: &[SigItem<Surface>], imports: Vec<Import>) -> Vec<Import> {
    let mut referenced = TypeReferences::default();
    for item in items {
        collect_sig_item_type_refs(item, &mut referenced);
    }
    imports.into_iter()
        .filter_map(|mut usage| match &mut usage.kind {
            ImportKind::Selective { items, .. } => {
                items.retain(
                    |item| matches!(item, ImportItem::Name { name, .. } if referenced.bare.contains(name)),
                );
                (!items.is_empty()).then_some(usage)
            }
            ImportKind::Qualified { alias, .. } => {
                referenced.qualifiers.contains(alias).then_some(usage)
            }
            ImportKind::Comptime => referenced
                .bare
                .iter()
                .any(|name| {
                    crate::comptime::ComptimeBuiltin::from_public_name(name)
                        .is_some_and(crate::comptime::ComptimeBuiltin::is_type_name)
                })
                .then_some(usage),
            ImportKind::Intrinsics => None,
        })
        .collect()
}

/// Collect the external type heads a [`SigItem`] references: bare nominal
/// names and the leading alias of qualified paths. Declaration binders are
/// excluded because they resolve inside the signature itself.
fn collect_sig_item_type_refs(item: &SigItem<Surface>, out: &mut TypeReferences) {
    match item {
        SigItem::HostType(_) => {}
        SigItem::HostFn(h) => collect_fn_type_refs(h, out),
        SigItem::ExportFn(export) => collect_fn_type_refs(&export.function, out),
        SigItem::TypeAlias(alias) => {
            let bound = alias
                .type_params
                .iter()
                .map(|param| param.name.as_str())
                .collect();
            collect_type_refs(&alias.body, &bound, out);
        }
        SigItem::Newtype(newtype) => {
            if let Some(payload) = newtype.host_surface().and_then(|surface| surface.payload()) {
                let bound = newtype
                    .type_params
                    .iter()
                    .chain(&newtype.existential_params)
                    .map(|param| param.name.as_str())
                    .collect();
                collect_type_refs(payload, &bound, out);
            }
        }
        SigItem::TypeRecGroup(group) => {
            for member in &group.members {
                match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => {
                        let bound = alias
                            .type_params
                            .iter()
                            .map(|param| param.name.as_str())
                            .collect();
                        collect_type_refs(&alias.body, &bound, out);
                    }
                    crate::ast::TypeRecMember::Newtype(newtype) => {
                        let bound = newtype
                            .type_params
                            .iter()
                            .chain(&newtype.existential_params)
                            .map(|param| param.name.as_str())
                            .collect();
                        collect_type_refs(&newtype.payload, &bound, out);
                    }
                    crate::ast::TypeRecMember::Labels(_, _) => {}
                }
            }
        }
    }
}

fn collect_fn_type_refs(function: &crate::ast::HostFn<Surface>, out: &mut TypeReferences) {
    let mut bound = std::collections::BTreeSet::new();
    for param in &function.params {
        match param {
            crate::ast::HostFnParam::Type(param) => {
                bound.insert(param.name.as_str());
            }
            crate::ast::HostFnParam::Value(param) => {
                collect_type_refs(&param.ty, &bound, out);
            }
        }
    }
    collect_type_refs(&function.ret, &bound, out);
}

/// Collect bare single-segment nominal heads in a `Type<Surface>`.
fn collect_type_refs<'a>(
    ty: &'a crate::ast::Type<Surface>,
    bound: &std::collections::BTreeSet<&'a str>,
    out: &mut TypeReferences,
) {
    use crate::ast::Type;
    match ty {
        Type::Path { segments, args, .. } => {
            if segments.len() == 1 {
                if !bound.contains(segments[0].name.as_str()) {
                    out.bare.insert(segments[0].name.clone());
                }
            } else if let Some(first) = segments.first() {
                out.qualifiers.insert(first.name.clone());
            }
            for arg in args {
                collect_type_refs(arg, bound, out);
            }
        }
        Type::Function { param, ret, .. } => {
            collect_type_refs(param, bound, out);
            collect_type_refs(ret, bound, out);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_type_refs(left, bound, out);
            collect_type_refs(right, bound, out);
        }
        Type::Forall { param, body, .. } => {
            let mut body_bound = bound.clone();
            body_bound.insert(param.name.as_str());
            collect_type_refs(body, &body_bound, out);
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } | Type::LabelSugar { .. } => {}
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn type_rec_member_name(member: &TypeRecMember<Surface>) -> Option<&str> {
    match member {
        TypeRecMember::TypeAlias(alias) => Some(&alias.name),
        TypeRecMember::Newtype(newtype) => Some(&newtype.name),
        TypeRecMember::Labels(_, _) => None,
    }
}

fn module_path_of(path: &str) -> crate::ast::ModulePath {
    let segments = path
        .split('/')
        .map(|s| crate::ast::PathSegment::synth(s, Span::new(0, 0)))
        .collect();
    crate::ast::ModulePath {
        segments,
        span: Span::new(0, 0),
    }
}

fn module_path_string(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}
