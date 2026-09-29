//! The draft / seal mechanism behind the `kio sig` write commands.
//!
//! A `*.sig.kio` changelog's top version `v(N)` is a mutable **draft**
//! until `kio sig commit` **seals** it. The break-check baseline is the
//! **last sealed** version — `v(N-1)` after replay — never the open
//! draft. The draft itself is recomputed from scratch every run as
//! `diff(sealed_baseline, live)`:
//!
//! - **Sealed baseline** — [`replay_through`] the changelog truncated to
//!   the last sealed version (`current_version - 1`). On a fresh package
//!   (no sig, or `v(1)` still a draft) the baseline is the empty
//!   interface, so every export is a compatible `add`.
//! - **Draft delta** — [`super::compare`]`(sealed_baseline, live)`
//!   classifies every difference; the draft records exactly these
//!   changes, partitioned `breaking` / `nonbreaking` by computed
//!   verdict and `add` / `modify` / `remove` by placement.
//!
//! Recompute-from-scratch is what makes the draft **idempotent** and
//! **nets out intra-draft churn** (reconciliation #B): the recorded
//! `remove` set is exactly `sealed \ live`. An item added *and* removed
//! within the open draft is in neither the sealed baseline nor the live
//! surface, so it never appears as a change — no spurious `remove`
//! reaches a later seal or Phase 4's deprecation re-emit. A `remove`
//! the draft does record is a *sealed* item the live surface dropped,
//! which is exactly the deprecation-carry obligation.

use super::record::RecordedSurface;
use super::replay::{ReplayError, ReplayedInterface};
use super::verdict::{ChangePlacement, CompatReport, Verdict};
use super::{ContractSnapshot, QualifiedName};
use crate::ast::{SigChangeSet, SigVersion, SignatureFile, Surface};
use crate::span::Span;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Replay a changelog truncated to versions `<= through_version`.
///
/// The sealed baseline a draft is checked against is the interface the
/// already-sealed versions describe; the open draft (the top version)
/// is excluded by passing `current_version - 1`. Versions strictly
/// above `through_version` are dropped before replay, so the result is
/// the contract surface as of the last seal.
pub fn replay_through(
    file: &SignatureFile<Surface>,
    through_version: u32,
) -> Result<ReplayedInterface, ReplayError> {
    let truncated = SignatureFile {
        pkg: file.pkg.clone(),
        version: through_version,
        versions: file
            .versions
            .iter()
            .filter(|v| v.version <= through_version)
            .cloned()
            .collect(),
        meta: file.meta.clone(),
    };
    super::replay(&truncated)
}

/// The result of recomputing a package's draft against its sealed
/// baseline.
pub struct DraftPlan {
    /// The compat report of `diff(sealed_baseline, live)`. Drives both
    /// the recorded draft and the `kio sig status` exit code.
    pub report: CompatReport,
    /// The recomputed signature file with version `N`'s draft block
    /// rewritten to record the delta. Written by `kio sig stage` /
    /// `kio sig stage --force`; the `kio sig stage` (no-force) path
    /// errors first if `report.is_breaking()`.
    pub recomputed: SignatureFile<Surface>,
}

/// Compute the draft plan for a package.
///
/// `existing` is the parsed on-disk changelog, or `None` for a package
/// with no `*.sig.kio` yet (the first `kio sig stage` mints `v(1)`).
/// `recorded` / `live` are the live package's recorded-surface
/// declarations and its normalized snapshot.
///
/// `force` controls placement of breaking changes: with `force`, a
/// breaking change is recorded into the draft's `breaking` section;
/// without, the caller must reject a breaking delta before writing (the
/// recomputed file still partitions correctly so a forced write and a
/// status check agree).
pub fn compute_draft(
    pkg_name: &str,
    existing: Option<&SignatureFile<Surface>>,
    recorded: &RecordedSurface,
    live: &ContractSnapshot,
) -> Result<DraftPlan, ReplayError> {
    // The current draft generation. A fresh package starts at v(1); an
    // existing file's header version is the open draft.
    let current_version = existing.map(|f| f.version).unwrap_or(1);

    // Sealed baseline = replay through the last sealed version
    // (current_version - 1). v(1) (or no file) has no sealed history.
    let sealed = match existing {
        Some(file) if current_version > 1 => replay_through(file, current_version - 1)?,
        _ => ReplayedInterface::default(),
    };

    let report = super::compare(&sealed.current, live);
    let (context_groups, context_clears) = recursive_context_updates(&report, &sealed, recorded);

    // Build the draft block (version `current_version`) from the
    // report. Sealed versions are carried verbatim; only the open draft
    // is recomputed.
    let draft_block = draft_version_block(
        current_version,
        &report,
        recorded,
        &context_groups,
        &context_clears,
    );

    let mut versions: Vec<SigVersion<Surface>> = existing
        .map(|f| {
            f.versions
                .iter()
                .filter(|v| v.version < current_version)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if let Some(block) = draft_block {
        versions.push(block);
    }

    let recomputed = SignatureFile {
        pkg: pkg_name.to_owned(),
        version: current_version,
        versions,
        meta: crate::ast::Meta::new(Span::new(0, 0)),
    };

    // A draft is a fresh phase artifact, not trusted compiler output. Replay
    // exactly what record/status/commit will consume before returning it so a
    // malformed reconstruction cannot be written and acquire authority from
    // its producer.
    super::replay(&recomputed)?;

    Ok(DraftPlan { report, recomputed })
}

/// Build the open-draft `v(N) { breaking { … } nonbreaking { … } }`
/// block from the compat report, or `None` when the delta is empty (a
/// fully-reconciled draft records nothing).
fn draft_version_block(
    version: u32,
    report: &CompatReport,
    recorded: &RecordedSurface,
    context_groups: &BTreeSet<QualifiedName>,
    context_clears: &BTreeSet<QualifiedName>,
) -> Option<SigVersion<Surface>> {
    if report.is_empty() {
        return None;
    }

    let breaking = change_set_for(report, Verdict::Breaking, recorded);
    let nonbreaking = change_set_for(report, Verdict::Compatible, recorded);
    let mut with = recorded.contexts_for(context_groups);
    with.extend(recorded.sections_for(context_clears));
    let with = merge_context_sections(with);

    Some(SigVersion {
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
        with_leading_trivia: Vec::new(),
        with_trailing_trivia: Vec::new(),
        // The open draft carries no message; `kio sig commit -m "…"`
        // attaches one when it seals the block.
        doc: None,
        version,
        with,
        breaking,
        nonbreaking,
        span: Span::new(0, 0),
    })
}

/// Compute the minimal current declaration-context closure whose epoch must
/// accompany this version. Starting from actual per-name operations, close
/// over both the sealed and live recursive groups. This is what makes a split
/// representable: an unchanged component on the other side of a removed edge
/// still receives its new complete group, while an unchanged member that
/// became acyclic receives one ordinary context declaration that clears its
/// old group epoch.
fn recursive_context_updates(
    report: &CompatReport,
    sealed: &ReplayedInterface,
    recorded: &RecordedSurface,
) -> (BTreeSet<QualifiedName>, BTreeSet<QualifiedName>) {
    let current = current_context_index(recorded);
    let previous = previous_context_index(sealed);

    let mut affected = report
        .changes
        .iter()
        .map(|change| change.name.clone())
        .collect::<BTreeSet<_>>();
    let mut queue = affected.iter().cloned().collect::<VecDeque<_>>();
    let mut expanded_current = BTreeSet::new();
    let mut expanded_previous = BTreeSet::new();
    while let Some(name) = queue.pop_front() {
        for (index, expanded) in [
            (&current, &mut expanded_current),
            (&previous, &mut expanded_previous),
        ] {
            let Some(group) = index.by_name.get(&name).copied() else {
                continue;
            };
            if !expanded.insert(group) {
                continue;
            }
            for member in &index.groups[group] {
                if affected.insert(member.clone()) {
                    queue.push_back(member.clone());
                }
            }
        }
    }

    let context_groups = affected
        .iter()
        .filter(|name| current.by_name.contains_key(*name))
        .cloned()
        .collect();
    let actively_changed = report
        .changes
        .iter()
        .filter(|change| {
            matches!(
                change.placement,
                ChangePlacement::Added | ChangePlacement::Modified
            )
        })
        .map(|change| change.name.clone())
        .collect::<BTreeSet<_>>();
    let context_clears = affected
        .into_iter()
        .filter(|name| {
            previous.by_name.contains_key(name)
                && !current.by_name.contains_key(name)
                && recorded.items.contains_key(name)
                && !actively_changed.contains(name)
        })
        .collect();
    (context_groups, context_clears)
}

struct RecursiveContextIndex {
    by_name: BTreeMap<QualifiedName, usize>,
    groups: Vec<BTreeSet<QualifiedName>>,
}

fn current_context_index(recorded: &RecordedSurface) -> RecursiveContextIndex {
    let mut by_name = BTreeMap::new();
    let mut groups = Vec::new();
    let mut ids = BTreeMap::new();
    for (name, context) in &recorded.recursive_contexts {
        let group = match ids.get(&context.key).copied() {
            Some(group) => group,
            None => {
                let group = groups.len();
                groups.push(context_member_names(&context.section));
                ids.insert(context.key.clone(), group);
                group
            }
        };
        by_name.insert(name.clone(), group);
    }
    RecursiveContextIndex { by_name, groups }
}

fn previous_context_index(sealed: &ReplayedInterface) -> RecursiveContextIndex {
    let mut by_name = BTreeMap::new();
    let mut groups = Vec::new();
    let mut ids = BTreeMap::new();
    for item in &sealed.live_frozen {
        let Some(context) = &item.recursive_context else {
            continue;
        };
        let identity = std::sync::Arc::as_ptr(context) as usize;
        let group = match ids.get(&identity).copied() {
            Some(group) => group,
            None => {
                let group = groups.len();
                groups.push(context_member_names(context));
                ids.insert(identity, group);
                group
            }
        };
        by_name.insert(item.entry.name.clone(), group);
    }
    RecursiveContextIndex { by_name, groups }
}

fn context_member_names(
    section: &crate::ast::SigModuleSection<Surface>,
) -> BTreeSet<QualifiedName> {
    use crate::ast::{SigItem, TypeRecMember};
    let module = module_path_string(&section.path);
    section
        .items
        .iter()
        .flat_map(|item| match item {
            SigItem::TypeRecGroup(group) => group
                .members
                .iter()
                .filter_map(|member| match member {
                    TypeRecMember::TypeAlias(alias) => Some(alias.name.clone()),
                    TypeRecMember::Newtype(newtype) => Some(newtype.name.clone()),
                    TypeRecMember::Labels(_, _) => None,
                })
                .collect::<Vec<_>>(),
            SigItem::HostType(item) => vec![item.name.clone()],
            SigItem::HostFn(item) => vec![item.name.clone()],
            SigItem::TypeAlias(item) => vec![item.name.clone()],
            SigItem::Newtype(item) => vec![item.name.clone()],
            SigItem::ExportFn(item) => vec![item.function.name.clone()],
        })
        .map(|name| QualifiedName::new(module.clone(), name))
        .collect()
}

fn merge_context_sections(
    sections: Vec<crate::ast::SigModuleSection<Surface>>,
) -> Vec<crate::ast::SigModuleSection<Surface>> {
    let mut by_module = BTreeMap::new();
    for section in sections {
        let module = module_path_string(&section.path);
        let combined = by_module
            .entry(module)
            .or_insert_with(|| crate::ast::SigModuleSection {
                leading_trivia: Vec::new(),
                trailing_trivia: Vec::new(),
                path: section.path.clone(),
                imports: Vec::new(),
                items: Vec::new(),
                span: Span::new(0, 0),
            });
        for usage in section.imports {
            if !combined.imports.contains(&usage) {
                combined.imports.push(usage);
            }
        }
        combined.items.extend(section.items);
    }
    by_module.into_values().collect()
}

fn module_path_string(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// Build the `add` / `modify` / `remove` blocks for the changes of one
/// verdict. Added / modified items pull their live declaration from the
/// recorded surface; removed items list names only (their frozen
/// signature lives in the earlier `add` / `modify` the replay recovers).
fn change_set_for(
    report: &CompatReport,
    verdict: Verdict,
    recorded: &RecordedSurface,
) -> Option<SigChangeSet<Surface>> {
    let added: Vec<&QualifiedName> = report
        .changes
        .iter()
        .filter(|c| c.verdict == verdict && c.placement == ChangePlacement::Added)
        .map(|c| &c.name)
        .collect();
    let modified: Vec<&QualifiedName> = report
        .changes
        .iter()
        .filter(|c| c.verdict == verdict && c.placement == ChangePlacement::Modified)
        .map(|c| &c.name)
        .collect();
    let removed_names: Vec<&QualifiedName> = report
        .changes
        .iter()
        .filter(|c| c.verdict == verdict && c.placement == ChangePlacement::Removed)
        .map(|c| &c.name)
        .collect();

    if added.is_empty() && modified.is_empty() && removed_names.is_empty() {
        return None;
    }

    let add = recorded.sections_for(added.iter().copied());
    let add_refs = recorded.refs_for(added.iter().copied());
    let modify = recorded.sections_for(modified.iter().copied());
    let modify_refs = recorded.refs_for(modified.iter().copied());
    let remove = Vec::new();
    let remove_refs = remove_refs(&removed_names);

    Some(SigChangeSet {
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
        add_leading_trivia: Vec::new(),
        add_trailing_trivia: Vec::new(),
        modify_leading_trivia: Vec::new(),
        modify_trailing_trivia: Vec::new(),
        remove_leading_trivia: Vec::new(),
        remove_trailing_trivia: Vec::new(),
        add,
        add_refs,
        modify,
        modify_refs,
        remove,
        remove_refs,
        span: Span::new(0, 0),
    })
}

fn remove_refs(names: &[&QualifiedName]) -> Vec<crate::ast::SigItemRef> {
    let mut names = names.to_vec();
    names.sort();
    names
        .into_iter()
        .map(|name| crate::ast::SigItemRef {
            leading_trivia: Vec::new(),
            path: module_path_of(&name.module_path),
            name: name.leaf.clone(),
            span: Span::new(0, 0),
        })
        .collect()
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
