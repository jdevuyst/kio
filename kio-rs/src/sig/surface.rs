//! Build a [`ContractSnapshot`] from a typechecked package's contract
//! surface.
//!
//! The walk mirrors the resolver's bridge-contract validation: scan the
//! bridged modules, take every `pub` item, and qualify each signature
//! type to its fully module-qualified contract form before
//! canonicalizing. `pub host` items land on the env side, other `pub`
//! items on the export side.
//!
//! A function's identity is its full System-F type (`signature_ty`):
//! building the single `Type` preserves the curry-layer / binder
//! interleaving and lets the type normalizer alpha-rename every binder
//! uniformly, so two alpha-equivalent signatures — even with differently
//! named or differently grouped binders — share one normalized string.

use super::{
    ContractEntry, ContractKind, ContractSide, ContractSnapshot, PublicNewtypeSurface,
    QualifiedName,
};
use crate::ast::{
    HostFn, HostFnParam, HostType, Item, Kind, Newtype, Phase, Purity, Signature, SignatureParam,
    Type, TypeAlias,
};
use crate::pass::resolve::{
    ExportContractPhase, ModuleEntry, Package, bridged_module_paths, exported_contract_type,
};
use crate::span::Span;
use std::collections::HashMap;

/// Identity head-qualifier: the snapshot pre-qualifies every type via
/// [`exported_contract_type`], so the normalizer sees already-qualified
/// heads and passes them through.
fn identity_head(segments: &[String]) -> Vec<String> {
    segments.to_vec()
}

pub(super) fn snapshot_from_package<P>(package: &Package<P>) -> ContractSnapshot
where
    P: ExportContractPhase + Phase<FnPurity = Purity> + Clone,
{
    let bridged = bridged_module_paths(package);
    let mut items = std::collections::BTreeMap::new();
    for (path, entry) in package.modules() {
        if !bridged.contains(path) {
            continue;
        }
        for item in &entry.module.items {
            if let Item::TypeRecGroup(group) = item {
                for member in &group.members {
                    let contract_entry = match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) if alias.vis.is_exported() => {
                            Some(ContractEntry {
                                name: QualifiedName::new(path, alias.name.clone()),
                                side: ContractSide::Export,
                                kind: alias_kind(alias, entry),
                            })
                        }
                        crate::ast::TypeRecMember::Newtype(newtype)
                            if newtype.is_host_exported() =>
                        {
                            Some(ContractEntry {
                                name: QualifiedName::new(path, newtype.name.clone()),
                                side: ContractSide::Export,
                                kind: newtype_kind(newtype, entry),
                            })
                        }
                        crate::ast::TypeRecMember::TypeAlias(_)
                        | crate::ast::TypeRecMember::Newtype(_) => None,
                        crate::ast::TypeRecMember::Labels(_, ext) => match *ext {},
                    };
                    if let Some(contract_entry) = contract_entry {
                        items.insert(contract_entry.name.clone(), contract_entry);
                    }
                }
                continue;
            }
            if !item_is_exported(item) {
                continue;
            }
            if let Some(contract_entry) = entry_from_item(path, item, entry) {
                items.insert(contract_entry.name.clone(), contract_entry);
            }
        }
    }
    ContractSnapshot { items }
}

/// Whether the item is `pub` (host items are always public). Mirrors the
/// resolver's `item_is_exported`, kept local so the snapshot does not depend
/// on the resolver re-exporting it.
fn item_is_exported<P: Phase>(item: &Item<P>) -> bool {
    match item {
        Item::FnDef(d) => d.vis.is_exported(),
        Item::TypeAlias(a) => a.vis.is_exported(),
        Item::Newtype(d) => d.is_host_exported(),
        Item::HostType(_) | Item::HostFn(_) => true,
        Item::Labels(d, _) => d.vis.is_exported(),
        Item::LabelForward(_, _) => false,
        Item::Equiv(_, _) => false,
        Item::Elaborator(s, _) => s.vis.is_exported(),
        Item::LiteralAlias(_, _)
        | Item::Op(_, _)
        | Item::VariadicOperator(_, _)
        | Item::RecGroup(_, _) => false,
        Item::TypeRecGroup(group) => group.members.iter().any(|member| match member {
            crate::ast::TypeRecMember::TypeAlias(alias) => alias.vis.is_exported(),
            crate::ast::TypeRecMember::Newtype(newtype) => newtype.is_host_exported(),
            crate::ast::TypeRecMember::Labels(labels, _) => labels.vis.is_exported(),
        }),
    }
}

fn entry_from_item<P>(
    module_path: &str,
    item: &Item<P>,
    defining: &ModuleEntry<P>,
) -> Option<ContractEntry>
where
    P: ExportContractPhase + Phase<FnPurity = Purity> + Clone,
{
    match item {
        Item::HostType(h) => Some(ContractEntry {
            name: QualifiedName::new(module_path, h.name.clone()),
            side: ContractSide::Env,
            kind: host_type_kind(h),
        }),
        Item::HostFn(h) => Some(ContractEntry {
            name: QualifiedName::new(module_path, h.name.clone()),
            side: ContractSide::Env,
            kind: ContractKind::Fn {
                signature: host_fn_signature(h, defining),
                pure: false,
            },
        }),
        Item::FnDef(d) => Some(ContractEntry {
            name: QualifiedName::new(module_path, d.name.clone()),
            side: ContractSide::Export,
            kind: ContractKind::Fn {
                signature: fn_def_signature(d, defining),
                pure: d.purity.is_pure(),
            },
        }),
        Item::TypeAlias(a) => Some(ContractEntry {
            name: QualifiedName::new(module_path, a.name.clone()),
            side: ContractSide::Export,
            kind: alias_kind(a, defining),
        }),
        Item::Newtype(n) => Some(ContractEntry {
            name: QualifiedName::new(module_path, n.name.clone()),
            side: ContractSide::Export,
            kind: newtype_kind(n, defining),
        }),
        // Surface-only / non-contract items never reach the export
        // surface (uninhabited or filtered before the typechecked
        // phase); they contribute no contract entry.
        Item::Labels(_, _)
        | Item::LabelForward(_, _)
        | Item::Equiv(_, _)
        | Item::Elaborator(_, _)
        | Item::LiteralAlias(_, _)
        | Item::Op(_, _)
        | Item::VariadicOperator(_, _)
        | Item::RecGroup(_, _)
        | Item::TypeRecGroup(_) => None,
    }
}

pub(super) fn host_type_kind<P: Phase>(h: &HostType<P>) -> ContractKind {
    ContractKind::HostType {
        param_kinds: h
            .type_params
            .iter()
            .map(|p| p.effective_kind().to_string())
            .collect(),
        role: h.role.map(|r| r.role.as_str().to_owned()),
    }
}

/// Qualify a type through the defining module's `import` clauses, then
/// canonicalize it. `locals` carries any binder kinds in scope (so a
/// binder reference is not mistaken for a nominal head during
/// qualification); the normalizer then alpha-renames all binders.
fn canon<P>(ty: &Type<P>, defining: &ModuleEntry<P>, locals: &HashMap<String, Kind>) -> String
where
    P: ExportContractPhase + Clone,
{
    let qualified = exported_contract_type(ty, defining, locals);
    super::canonical_type(&qualified, &identity_head)
}

/// Canonical signature of a host fn — build its System-F type and
/// canonicalize the whole thing.
pub(super) fn host_fn_signature<P>(h: &HostFn<P>, defining: &ModuleEntry<P>) -> String
where
    P: ExportContractPhase + Clone,
{
    let span = Span::new(0, 0);
    let params: Vec<SignatureParam<P>> = h
        .params
        .iter()
        .map(|p| match p {
            HostFnParam::Type(tp) => SignatureParam::Type(tp.clone()),
            HostFnParam::Value(v) => SignatureParam::Value(crate::ast::Param {
                name: v.name.clone().unwrap_or_default(),
                ty: Some(v.ty.clone()),
                pattern: Default::default(),
                meta: v.meta.clone(),
            }),
        })
        .collect();
    let sig = Signature::from_parts(params, h.param_groups.clone());
    let ty = sig.signature_ty(h.ret.clone(), span);
    canon(&ty, defining, &HashMap::new())
}

/// Canonical signature of an exported fn def — its full System-F type.
pub(super) fn fn_def_signature<P>(d: &crate::ast::FnDef<P>, defining: &ModuleEntry<P>) -> String
where
    P: ExportContractPhase + Clone,
{
    let ty = d.sig.signature_ty(d.ret.clone(), d.meta.span);
    canon(&ty, defining, &HashMap::new())
}

pub(super) fn alias_kind<P>(a: &TypeAlias<P>, defining: &ModuleEntry<P>) -> ContractKind
where
    P: ExportContractPhase + Clone,
{
    let mut locals = HashMap::new();
    let mut param_kinds = Vec::new();
    for tp in &a.type_params {
        locals.insert(tp.name.clone(), tp.effective_kind());
        param_kinds.push(tp.effective_kind().to_string());
    }
    ContractKind::Alias {
        param_kinds,
        expansion: {
            let qualified = exported_contract_type(&a.body, defining, &locals);
            super::canonical_declaration_type(&qualified, &a.type_params, &[], &identity_head)
        },
    }
}

pub(super) fn newtype_kind<P>(n: &Newtype<P>, defining: &ModuleEntry<P>) -> ContractKind
where
    P: ExportContractPhase + Clone,
{
    let mut locals = HashMap::new();
    let mut param_kinds = Vec::new();
    for tp in &n.type_params {
        locals.insert(tp.name.clone(), tp.effective_kind());
        param_kinds.push(tp.effective_kind().to_string());
    }
    for tp in &n.existential_params {
        locals.insert(tp.name.clone(), tp.effective_kind());
    }
    let surface = PublicNewtypeSurface::from_host_surface(n.host_surface(), |payload| {
        let qualified = exported_contract_type(payload, defining, &locals);
        super::canonical_declaration_type(
            &qualified,
            &n.type_params,
            &n.existential_params,
            &identity_head,
        )
    });
    ContractKind::Newtype {
        param_kinds,
        surface,
    }
}
