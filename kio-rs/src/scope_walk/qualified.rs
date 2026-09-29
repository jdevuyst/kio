use super::*;
use crate::doc_entry::{DocEntry, documented_items_module};
use crate::pass::parser::CursorSlot;
use crate::pass::resolve::{
    IdentityAliasNewtypeIndex, for_each_item_declaration, identity_alias_binding_target_in_scope,
    identity_alias_progressive_scope_add_item, identity_alias_surface_type_scope, is_visible,
};

/// Supply only the selected provider and dependencies of its resolved identity
/// aliases. The shared identity index remains the authority on nominal forwarding.
pub(crate) fn qualified_providers<T>(
    current: &Module,
    head: &[crate::ast::PathSegment],
    slot: CursorSlot,
    mut load: impl FnMut(&str) -> Option<(T, Module)>,
) -> (std::collections::BTreeMap<String, (T, Module)>, bool) {
    let mut providers = std::collections::BTreeMap::new();
    let Some((mut owner, mut name)) = qualified_root(current, head) else {
        return (providers, false);
    };
    if name.is_some() && slot == CursorSlot::Type {
        return (providers, false);
    }
    let current_path = current.path.segments.join("/");
    let mut visited = std::collections::HashSet::new();
    let mut written = true;
    while visited.insert((owner.clone(), name.clone())) {
        let module = if owner == current_path {
            current
        } else {
            if !providers.contains_key(&owner) {
                let Some(provider) = load(&owner) else {
                    return (providers, true);
                };
                providers.insert(owner.clone(), provider);
            }
            &providers[&owner].1
        };
        let Some(selected) = &name else { break };
        let entries = documented_items_module(module);
        let mut matching = entries.iter().filter(|entry| entry.name() == selected);
        let Some(entry) = matching.next() else { break };
        if matching.next().is_some()
            || written && owner != current_path && !is_visible(entry.visibility(), &current.path)
        {
            break;
        }
        written = false;
        if !matches!(entry, DocEntry::TypeAlias(..)) {
            break;
        }
        let Some((target_owner, target_name)) = alias_target(module, selected) else {
            break;
        };
        owner = target_owner;
        name = Some(target_name);
    }
    (providers, false)
}

fn alias_target(module: &Module, name: &str) -> Option<(String, String)> {
    let mut scope = identity_alias_surface_type_scope(module);
    for (index, item) in module.items.iter().enumerate() {
        identity_alias_progressive_scope_add_item(&mut scope, index, item);
        let mut found = None;
        for_each_item_declaration(item, |declaration| {
            if let Some(alias) = declaration.type_alias()
                && alias.name == name
            {
                found = Some(identity_alias_binding_target_in_scope(
                    alias, module, &scope,
                ));
            }
        });
        if let Some(target) = found {
            return target;
        }
    }
    None
}

fn qualified_root(
    current: &Module,
    head: &[crate::ast::PathSegment],
) -> Option<(String, Option<String>)> {
    let qualified = |alias: &str| {
        let mut paths = current
            .imports
            .iter()
            .filter_map(|import| match &import.kind {
                ImportKind::Qualified {
                    path,
                    alias: written,
                } if written == alias => Some(path.segments.join("/")),
                _ => None,
            });
        let path = paths.next()?;
        paths.next().is_none().then_some(path)
    };
    match head {
        [alias] if qualified(alias.as_str()).is_some() => Some((qualified(alias.as_str())?, None)),
        [name] if crate::naming::is_type_reference_name(name.as_str()) => {
            let mut paths = current
                .imports
                .iter()
                .filter_map(|import| match &import.kind {
                    ImportKind::Selective { from, items }
                        if items
                            .iter()
                            .any(|item| item.as_name() == Some(name.as_str())) =>
                    {
                        Some(from.segments.join("/"))
                    }
                    _ => None,
                });
            let owner = paths
                .next()
                .unwrap_or_else(|| current.path.segments.join("/"));
            paths
                .next()
                .is_none()
                .then(|| (owner, Some(name.name.clone())))
        }
        [alias, name] => Some((qualified(alias.as_str())?, Some(name.name.clone()))),
        _ => None,
    }
}

/// A qualified route retains the exact source owner of its presentation data.
#[derive(Clone, Debug)]
pub(crate) struct QualifiedCandidate {
    pub(crate) label: String,
    pub(crate) kind: CandidateKind,
    pub(crate) owner: String,
    pub(crate) detail: Option<String>,
    pub(crate) doc: Option<crate::ast::DocComment>,
}

pub(crate) fn qualified_candidates(
    current: &Module,
    head: &[crate::ast::PathSegment],
    slot: CursorSlot,
    modules: &[&Module],
) -> Vec<QualifiedCandidate> {
    let current_path = current.path.segments.join("/");
    let find_module = |name: &str| {
        if name == current_path {
            return Some(current);
        }
        let mut matches = modules
            .iter()
            .copied()
            .filter(|module| module.path.segments.join("/") == name);
        let module = matches.next()?;
        matches.next().is_none().then_some(module)
    };
    let Some((owner, name)) = qualified_root(current, head) else {
        return Vec::new();
    };
    if name.is_none() {
        let Some(provider) = find_module(&owner) else {
            return Vec::new();
        };
        return documented_items_module(provider)
            .into_iter()
            .filter_map(|entry| {
                if !is_visible(entry.visibility(), &current.path) {
                    return None;
                }
                ordinary_candidate(&entry, &owner, slot)
            })
            .collect();
    }
    if slot == CursorSlot::Type {
        return Vec::new();
    }
    let selected = (owner, name.as_deref().unwrap());
    let Some(owner) = find_module(&selected.0) else {
        return Vec::new();
    };
    let entries = documented_items_module(owner);
    let mut matching = entries.iter().filter(|entry| entry.name() == selected.1);
    let Some(written) = matching.next() else {
        return Vec::new();
    };
    if matching.next().is_some()
        || selected.0 != current_path && !is_visible(written.visibility(), &current.path)
    {
        return Vec::new();
    }
    let terminal;
    let terminal_owner;
    let terminal_entry;
    let entry = if matches!(written, DocEntry::TypeAlias(..) | DocEntry::Labels(..)) {
        let index = IdentityAliasNewtypeIndex::build_for_surface_modules(
            std::iter::once(current).chain(
                modules
                    .iter()
                    .copied()
                    .filter(|module| module.path.segments.join("/") != current_path),
            ),
        );
        terminal = index.terminal_key(&selected.0, selected.1).cloned();
        let Some((path, name)) = &terminal else {
            return Vec::new();
        };
        let Some(owner) = find_module(path) else {
            return Vec::new();
        };
        terminal_owner = path.clone();
        terminal_entry = documented_items_module(owner)
            .into_iter()
            .find(|entry| entry.name() == name);
        let Some(entry) = &terminal_entry else {
            return Vec::new();
        };
        entry
    } else {
        terminal_owner = selected.0.clone();
        written
    };
    let nominal;
    let newtype = match entry {
        DocEntry::Newtype(newtype, _) => *newtype,
        DocEntry::LabelNominal {
            labels,
            entry,
            name,
            ..
        } => {
            nominal = crate::pass::label_elab::label_nominal_declaration(
                labels,
                entry,
                name.clone(),
                entry.payload.clone(),
            );
            &nominal
        }
        _ => return Vec::new(),
    };
    [&newtype.constructor, &newtype.projector]
        .into_iter()
        .filter_map(|member| {
            if terminal_owner != current_path && !is_visible(&member.vis, &current.path) {
                return None;
            }
            Some(QualifiedCandidate {
                label: member.name.clone(),
                kind: CandidateKind::Function,
                owner: terminal_owner.clone(),
                detail: None,
                doc: entry.doc().cloned(),
            })
        })
        .collect()
}

fn ordinary_candidate(
    entry: &DocEntry<'_>,
    owner: &str,
    slot: CursorSlot,
) -> Option<QualifiedCandidate> {
    let (mut label, kind) = match entry {
        DocEntry::Fn(_) | DocEntry::HostFn(_) | DocEntry::Elaborator(_) => {
            (entry.name().to_owned(), CandidateKind::Function)
        }
        DocEntry::LiteralAlias(_) => (entry.name().to_owned(), CandidateKind::Constant),
        DocEntry::TypeAlias(..)
        | DocEntry::Newtype(..)
        | DocEntry::HostType(_)
        | DocEntry::Labels(..)
        | DocEntry::LabelNominal { .. } => (entry.name().to_owned(), CandidateKind::Type),
        DocEntry::LabelForward(..) | DocEntry::Op(..) | DocEntry::VariadicOperator(..) => {
            return None;
        }
    };
    if matches!(slot, CursorSlot::Value | CursorSlot::OperatorContinuation)
        && kind == CandidateKind::Type
        || slot == CursorSlot::Type && kind != CandidateKind::Type
    {
        return None;
    }
    if matches!(entry, DocEntry::Elaborator(_)) {
        label.push('!');
    }
    Some(QualifiedCandidate {
        label,
        kind,
        owner: owner.to_owned(),
        detail: match entry {
            DocEntry::Elaborator(elaborator) => {
                Some(crate::pretty::pretty_type(&elaborator.call_ty))
            }
            _ => entry.ty(),
        },
        doc: entry.doc().cloned(),
    })
}
