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
    HostType, Import, ImportItem, ImportKind, Item, Newtype, Phase, Purity, SigExportFn, SigItem,
    SigItemRef, SigModuleSection, Surface, TypeAlias, TypeRecGroup, TypeRecMember,
};
use crate::pass::resolve::{ExportContractPhase, Package, bridged_module_paths};
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
        let bridged = bridged_module_paths(package);
        let mut items = BTreeMap::new();
        let mut module_imports = BTreeMap::new();
        let mut recursive_contexts = BTreeMap::new();
        for (path, entry) in package.modules() {
            if !bridged.contains(path) {
                continue;
            }
            let mut any = false;
            let mut projected_items = Vec::new();
            let mut projected_contexts = Vec::new();
            for item in &entry.module.items {
                if let Item::TypeRecGroup(group) = item {
                    let (group_items, contexts) = recorded_type_rec_group(path, group);
                    for (name, recorded) in group_items {
                        projected_items.push(recorded.item.clone());
                        items.insert(name, recorded);
                        any = true;
                    }
                    projected_contexts.extend(contexts);
                    continue;
                }
                let Some((name, recorded)) = recorded_item(path, item) else {
                    continue;
                };
                if matches!(
                    &recorded.item,
                    SigItem::Newtype(newtype) if newtype.rec_span.is_some()
                ) {
                    projected_contexts.push(recorded.item.clone());
                }
                projected_items.push(recorded.item.clone());
                items.insert(name, recorded);
                any = true;
            }
            if any {
                let imports: Vec<Import> =
                    entry.module.imports.iter().map(convert_import).collect();
                module_imports.insert(
                    path.to_owned(),
                    imports_needed_by(&projected_items, imports.clone()),
                );
                for item in projected_contexts {
                    let group_imports =
                        imports_needed_by(std::slice::from_ref(&item), imports.clone());
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
                    let key = format!("{path}:{}", member_names.join(","));
                    let section = SigModuleSection {
                        leading_trivia: Vec::new(),
                        trailing_trivia: Vec::new(),
                        path: module_path_of(path),
                        imports: group_imports,
                        items: vec![item],
                        span: Span::new(0, 0),
                    };
                    for member in member_names {
                        recursive_contexts.insert(
                            QualifiedName::new(path, member),
                            RecordedTypeContext {
                                key: key.clone(),
                                section: section.clone(),
                            },
                        );
                    }
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

fn recorded_type_rec_group<P>(
    module_path: &str,
    group: &TypeRecGroup<P>,
) -> (Vec<(QualifiedName, RecordedItem)>, Vec<SigItem<Surface>>)
where
    P: ExportContractPhase + Phase<FnPurity = Purity>,
{
    let mut recorded = Vec::new();
    let mut members = Vec::new();
    for member in &group.members {
        match member {
            TypeRecMember::TypeAlias(alias) if alias.vis.is_exported() => {
                let mut alias: TypeAlias<Surface> = crate::ast::convert_type_alias(alias);
                alias.doc = None;
                let item = SigItem::TypeAlias(alias.clone());
                recorded.push((
                    QualifiedName::new(module_path, alias.name.clone()),
                    RecordedItem {
                        side: ContractSide::Export,
                        item,
                    },
                ));
                members.push(TypeRecMember::TypeAlias(alias));
            }
            TypeRecMember::Newtype(newtype) if newtype.is_host_exported() => {
                let newtype = recorded_newtype(newtype);
                let item = SigItem::Newtype(newtype.clone());
                recorded.push((
                    QualifiedName::new(module_path, newtype.name.clone()),
                    RecordedItem {
                        side: ContractSide::Export,
                        item,
                    },
                ));
                members.push(TypeRecMember::Newtype(newtype));
            }
            TypeRecMember::TypeAlias(_) | TypeRecMember::Newtype(_) => {}
            TypeRecMember::Labels(_, ext) => match *ext {},
        }
    }

    let projected = super::repartition_recorded_type_rec_group(TypeRecGroup {
        members,
        doc: None,
        source_layout: None,
        rec_span: group.rec_span,
        open_brace_span: None,
        close_brace_span: None,
        deferred_rec_labels_diagnostic: None,
        meta: crate::ast::Meta::new(group.meta.span),
    });
    let contexts = projected
        .into_iter()
        .filter(|item| {
            matches!(
                item,
                SigItem::TypeRecGroup(_)
                    | SigItem::Newtype(Newtype {
                        rec_span: Some(_),
                        ..
                    })
            )
        })
        .collect();
    (recorded, contexts)
}

fn type_rec_member_name(member: &TypeRecMember<Surface>) -> Option<&str> {
    match member {
        TypeRecMember::TypeAlias(alias) => Some(&alias.name),
        TypeRecMember::Newtype(newtype) => Some(&newtype.name),
        TypeRecMember::Labels(_, _) => None,
    }
}

/// Build the `(QualifiedName, RecordedItem)` for one module item, or
/// `None` for a non-contract item (private fn / surface-only form).
fn recorded_item<P>(module_path: &str, item: &Item<P>) -> Option<(QualifiedName, RecordedItem)>
where
    P: ExportContractPhase + Phase<FnPurity = Purity>,
{
    match item {
        Item::HostType(h) => {
            let mut h: HostType<Surface> = crate::ast::convert_host_type(h);
            // The sig is backend-independent: drop the redundant
            // source-compatible `{ owned }` annotation. Replay's host-type
            // identity is param kinds + role only, so this never changes the
            // recorded contract.
            h.owned = false;
            h.doc = None;
            Some((
                QualifiedName::new(module_path, h.name.clone()),
                RecordedItem {
                    side: ContractSide::Env,
                    item: SigItem::HostType(h),
                },
            ))
        }
        Item::HostFn(h) => {
            let mut h = crate::ast::convert_host_fn::<P, Surface>(h);
            h.doc = None;
            Some((
                QualifiedName::new(module_path, h.name.clone()),
                RecordedItem {
                    side: ContractSide::Env,
                    item: SigItem::HostFn(h),
                },
            ))
        }
        Item::FnDef(d) if d.vis.is_exported() => {
            // An exported fn records as a body-less `pub fn f(p) -> R;`
            // export signature, carried in a `HostFn` node (the
            // body-less fn-signature shape) and tagged `ExportFn`.
            let export = export_fn_from_def(d);
            Some((
                QualifiedName::new(module_path, d.name.clone()),
                RecordedItem {
                    side: ContractSide::Export,
                    item: SigItem::ExportFn(export),
                },
            ))
        }
        Item::TypeAlias(a) if a.vis.is_exported() => {
            let mut a: TypeAlias<Surface> = crate::ast::convert_type_alias(a);
            a.doc = None;
            Some((
                QualifiedName::new(module_path, a.name.clone()),
                RecordedItem {
                    side: ContractSide::Export,
                    item: SigItem::TypeAlias(a),
                },
            ))
        }
        Item::Newtype(n) if n.is_host_exported() => {
            let n = recorded_newtype(n);
            Some((
                QualifiedName::new(module_path, n.name.clone()),
                RecordedItem {
                    side: ContractSide::Export,
                    item: SigItem::Newtype(n),
                },
            ))
        }
        // Private fn / type / newtype, and every surface-only / non-
        // contract item, contribute no recorded contract entry.
        _ => None,
    }
}

/// Project a public newtype into ordinary signature-file syntax without
/// retaining facts hidden from the host contract. A hidden member needs a
/// syntactic stand-in because the ordinary newtype grammar always carries
/// both members; the stand-in is deterministic, private, and chosen without
/// consulting the hidden source name. With no public member the payload is
/// similarly replaced by unit and hidden existentials are dropped.
fn recorded_newtype<P>(newtype: &Newtype<P>) -> Newtype<Surface>
where
    P: ExportContractPhase + Phase<FnPurity = Purity>,
{
    super::project_newtype_declaration(crate::ast::convert_newtype(newtype))
}

/// Convert an exported `fn` definition into the body-less export-fn
/// `HostFn` shape the sig records. The body is discarded — only the
/// signature and return type are recorded.
fn export_fn_from_def<P>(d: &crate::ast::FnDef<P>) -> SigExportFn<Surface>
where
    P: ExportContractPhase + Phase<FnPurity = Purity>,
{
    let params = d
        .sig
        .params
        .iter()
        .map(|p| match p {
            crate::ast::SignatureParam::Type(tp) => crate::ast::HostFnParam::Type(tp.clone()),
            crate::ast::SignatureParam::Value(v) => {
                crate::ast::HostFnParam::Value(crate::ast::HostFnValueParam {
                    name: Some(v.name.clone()),
                    ty: v
                        .ty
                        .as_ref()
                        .map(crate::ast::convert_type::<P, Surface>)
                        .unwrap_or_else(|| crate::ast::Type::Unit {
                            meta: crate::ast::Meta::new(Span::new(0, 0)),
                        }),
                    meta: crate::ast::convert_meta(&v.meta),
                })
            }
        })
        .collect();
    SigExportFn {
        purity: d.purity,
        function: crate::ast::HostFn {
            name: d.name.clone(),
            params,
            param_groups: d.sig.groups.clone(),
            ret: crate::ast::convert_type::<P, Surface>(&d.ret),
            meta: crate::ast::convert_meta(&d.meta),
            doc: None,
        },
    }
}

fn convert_import(u: &Import) -> Import {
    // `Use` is phase-independent — clone verbatim.
    u.clone()
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
