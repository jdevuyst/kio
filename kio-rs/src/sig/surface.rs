//! One public declaration projection for live snapshots and signature history.
//!
//! Each live projection retains its defining module; historical callers retain
//! their own immutable section imports. Canonical entries and recorded syntax
//! derive from the same selected declaration, without combining origin scopes.

use super::{
    ContractEntry, ContractKind, ContractSide, ContractSnapshot, PublicNewtypeSurface,
    QualifiedName,
};
use crate::ast::{
    HostFn, HostFnParam, HostType, Item, Newtype, Param, Phase, Purity, SigExportFn, SigItem,
    Signature, SignatureParam, Surface, TypeAlias, TypeRecGroup, TypeRecMember,
};
use crate::pass::resolve::{
    ExportContractPhase, ModuleEntry, Package, bridged_module_paths,
    exported_contract_type_segments,
};
use crate::span::Span;
use std::collections::HashMap;

/// The syntax and comparison form of one declaration under one exact origin.
pub(super) struct PublicDeclaration<P: Phase = Surface> {
    pub(super) item: SigItem<P>,
    pub(super) entry: ContractEntry,
}

impl<P: Phase<FnPurity = Purity> + Clone> PublicDeclaration<P> {
    pub(super) fn from_origin(
        module_path: &str,
        item: SigItem<P>,
        qualify: &impl Fn(&[String]) -> Vec<String>,
    ) -> Self {
        let entry = entry_from_sig_item(module_path, &item, qualify);
        Self { item, entry }
    }
}

/// Borrowing the actual defining entry keeps its import and nominal scope
/// attached until both public views have been derived.
pub(super) struct PublicModule<'a, P: Phase> {
    pub(super) path: &'a str,
    pub(super) origin: &'a ModuleEntry<P>,
    pub(super) declarations: Vec<PublicDeclaration>,
    pub(super) recursive_contexts: Vec<SigItem<Surface>>,
}

pub(super) fn public_modules<P>(package: &Package<P>) -> impl Iterator<Item = PublicModule<'_, P>>
where
    P: ExportContractPhase + Phase<FnPurity = Purity> + Clone,
{
    let bridged = bridged_module_paths(package);
    package.modules().filter_map(move |(path, origin)| {
        if !bridged.contains(path) {
            return None;
        }
        let qualify = |head: &[String]| {
            let segments = head
                .iter()
                .map(|name| crate::ast::PathSegment::synth(name.clone(), Span::new(0, 0)))
                .collect::<Vec<_>>();
            exported_contract_type_segments(&segments, origin, &HashMap::new())
                .into_iter()
                .map(|segment| segment.name)
                .collect()
        };
        let mut declarations = Vec::new();
        let mut recursive_contexts = Vec::new();
        for item in &origin.module.items {
            if let Item::TypeRecGroup(group) = item {
                let (items, contexts) = project_type_rec_group(group);
                declarations.extend(
                    items
                        .into_iter()
                        .map(|item| PublicDeclaration::from_origin(path, item, &qualify)),
                );
                recursive_contexts.extend(contexts);
            } else if let Some(item) = project_item(item) {
                if matches!(&item, SigItem::Newtype(newtype) if newtype.rec_span.is_some()) {
                    recursive_contexts.push(item.clone());
                }
                declarations.push(PublicDeclaration::from_origin(path, item, &qualify));
            }
        }
        Some(PublicModule {
            path,
            origin,
            declarations,
            recursive_contexts,
        })
    })
}

pub(super) fn snapshot_from_package<P>(package: &Package<P>) -> ContractSnapshot
where
    P: ExportContractPhase + Phase<FnPurity = Purity> + Clone,
{
    ContractSnapshot {
        items: public_modules(package)
            .flat_map(|module| module.declarations)
            .map(|declaration| (declaration.entry.name.clone(), declaration.entry))
            .collect(),
    }
}

fn project_item<P>(item: &Item<P>) -> Option<SigItem<Surface>>
where
    P: ExportContractPhase + Phase<FnPurity = Purity>,
{
    Some(match item {
        Item::HostType(host) => {
            let mut host: HostType<Surface> = crate::ast::convert_host_type(host);
            host.owned = false;
            host.doc = None;
            SigItem::HostType(host)
        }
        Item::HostFn(host) => {
            let mut host = crate::ast::convert_host_fn(host);
            host.doc = None;
            SigItem::HostFn(host)
        }
        Item::FnDef(function) if function.vis.is_exported() => {
            SigItem::ExportFn(export_fn_from_def(function))
        }
        Item::TypeAlias(alias) if alias.vis.is_exported() => {
            let mut alias: TypeAlias<Surface> = crate::ast::convert_type_alias(alias);
            alias.doc = None;
            SigItem::TypeAlias(alias)
        }
        Item::Newtype(newtype) if newtype.is_host_exported() => SigItem::Newtype(
            super::project_newtype_declaration(crate::ast::convert_newtype(newtype)),
        ),
        _ => return None,
    })
}

fn project_type_rec_group<P>(
    group: &TypeRecGroup<P>,
) -> (Vec<SigItem<Surface>>, Vec<SigItem<Surface>>)
where
    P: ExportContractPhase + Phase<FnPurity = Purity>,
{
    let mut members = Vec::new();
    let mut items = Vec::new();
    for member in &group.members {
        match member {
            TypeRecMember::TypeAlias(alias) if alias.vis.is_exported() => {
                let mut alias: TypeAlias<Surface> = crate::ast::convert_type_alias(alias);
                alias.doc = None;
                items.push(SigItem::TypeAlias(alias.clone()));
                members.push(TypeRecMember::TypeAlias(alias));
            }
            TypeRecMember::Newtype(newtype) if newtype.is_host_exported() => {
                let newtype =
                    super::project_newtype_declaration(crate::ast::convert_newtype(newtype));
                items.push(SigItem::Newtype(newtype.clone()));
                members.push(TypeRecMember::Newtype(newtype));
            }
            TypeRecMember::TypeAlias(_) | TypeRecMember::Newtype(_) => {}
            TypeRecMember::Labels(_, witness) => match *witness {},
        }
    }
    let contexts = super::repartition_recorded_type_rec_group(TypeRecGroup {
        members,
        doc: None,
        source_layout: None,
        rec_span: group.rec_span,
        open_brace_span: None,
        close_brace_span: None,
        deferred_rec_labels_diagnostic: None,
        meta: crate::ast::Meta::new(group.meta.span),
    })
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
    (items, contexts)
}

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
                        .expect(
                            "public function parameters retain their mandatory type annotations",
                        ),
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

fn host_type_kind<P: Phase>(h: &HostType<P>) -> ContractKind {
    ContractKind::HostType {
        param_kinds: h
            .type_params
            .iter()
            .map(|p| p.effective_kind().to_string())
            .collect(),
        role: h.role.map(|r| r.role.as_str().to_owned()),
    }
}

pub(super) fn entry_from_sig_item<P>(
    module_path: &str,
    item: &SigItem<P>,
    qualify: &impl Fn(&[String]) -> Vec<String>,
) -> ContractEntry
where
    P: Phase<FnPurity = Purity> + Clone,
{
    match item {
        SigItem::HostType(h) => ContractEntry {
            name: QualifiedName::new(module_path, h.name.clone()),
            side: ContractSide::Env,
            kind: host_type_kind(h),
        },
        SigItem::HostFn(h) => ContractEntry {
            name: QualifiedName::new(module_path, h.name.clone()),
            side: ContractSide::Env,
            kind: ContractKind::Fn {
                signature: sig_fn_signature(h, qualify),
                pure: false,
            },
        },
        SigItem::ExportFn(export) => ContractEntry {
            name: QualifiedName::new(module_path, export.function.name.clone()),
            side: ContractSide::Export,
            kind: ContractKind::Fn {
                signature: sig_fn_signature(&export.function, qualify),
                pure: export.purity.is_pure(),
            },
        },
        SigItem::TypeAlias(a) => ContractEntry {
            name: QualifiedName::new(module_path, a.name.clone()),
            side: ContractSide::Export,
            kind: sig_alias_kind(a, qualify),
        },
        SigItem::Newtype(n) => ContractEntry {
            name: QualifiedName::new(module_path, n.name.clone()),
            side: ContractSide::Export,
            kind: sig_newtype_kind(n, qualify),
        },
        SigItem::TypeRecGroup(_) => {
            unreachable!("signature operation targets one recursive-group member, not the group")
        }
    }
}

/// Canonical signature of a sig `host fn` / export fn: build its
/// System-F type and canonicalize the whole thing through the section
/// qualifier — the same shape the live snapshot produces.
fn sig_fn_signature<P>(h: &HostFn<P>, qualify: &impl Fn(&[String]) -> Vec<String>) -> String
where
    P: Phase + Clone,
{
    let span = Span::new(0, 0);
    let params: Vec<SignatureParam<P>> = h
        .params
        .iter()
        .map(|p| match p {
            HostFnParam::Type(tp) => SignatureParam::Type(tp.clone()),
            HostFnParam::Value(v) => SignatureParam::Value(Param {
                name: v.name.clone().unwrap_or_default(),
                ty: Some(v.ty.clone()),
                pattern: Default::default(),
                meta: v.meta.clone(),
            }),
        })
        .collect();
    let sig = Signature::from_parts(params, h.param_groups.clone());
    let ty = sig.signature_ty(h.ret.clone(), span);
    super::canonical_type(&ty, qualify)
}

fn sig_alias_kind<P>(a: &TypeAlias<P>, qualify: &impl Fn(&[String]) -> Vec<String>) -> ContractKind
where
    P: Phase,
{
    ContractKind::Alias {
        param_kinds: a
            .type_params
            .iter()
            .map(|p| p.effective_kind().to_string())
            .collect(),
        expansion: super::canonical_declaration_type(&a.body, &a.type_params, &[], qualify),
    }
}

fn sig_newtype_kind<P>(n: &Newtype<P>, qualify: &impl Fn(&[String]) -> Vec<String>) -> ContractKind
where
    P: Phase,
{
    let surface = PublicNewtypeSurface::from_host_surface(n.host_surface(), |payload| {
        super::canonical_declaration_type(payload, &n.type_params, &n.existential_params, qualify)
    });
    ContractKind::Newtype {
        param_kinds: n
            .type_params
            .iter()
            .map(|p| p.effective_kind().to_string())
            .collect(),
        surface,
    }
}
