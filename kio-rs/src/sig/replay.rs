//! Replay a parsed `*.sig.kio` changelog into the contract interface it
//! describes.
//!
//! The signature file is a changelog: oldest-first per-version blocks,
//! each partitioning its changes into `breaking` / `nonbreaking`
//! sections of `add` / `modify` / `remove` blocks. The *current
//! interface* is not stored — it is recovered by **replaying** the
//! blocks in version order:
//!
//! - `add` → the item becomes present (insert),
//! - `modify` → the item reshapes (replace its recorded signature),
//! - `remove` → the item drops (and its last-known frozen entry is
//!   recorded for deprecation recovery).
//!
//! A `remove` lists names only; the item's **side** (host vs export) and
//! **frozen signature** are recovered from its earlier `add` / `modify`
//! origin — which is *why* the changelog keeps no GC.
//!
//! Cross-version behavior: an add-then-remove leaves the name absent from
//! the current interface but **recorded** in `removed` (recoverable for
//! the deprecation re-emit — it was present at an earlier sealed
//! boundary); a later re-add on the **same contract side** clears that
//! side's pending removal. Re-adding the name on the opposite side preserves
//! an explicit retirement, and a host-to-export flip implicitly retires the
//! abandoned host requirement. A modify-then-remove collapses to the single
//! removal of the last frozen shape. (The Phase-3 *draft
//! recompute* — not replay — is where an add-and-remove within a single
//! uncommitted draft nets out to nothing recorded; replay reconstructs
//! sealed history, so it keeps the removal.)
//!
//! Placement under `breaking` / `nonbreaking` is presentational; replay
//! applies every change regardless of partition.

use super::{ContractEntry, ContractSide, ContractSnapshot, QualifiedName};
#[cfg(test)]
use super::{ContractKind, PublicNewtypeSurface};
use crate::ast::{
    HostFn, HostFnParam, HostType, Import, ImportKind, ModulePath, Newtype, Prime, SigChangeSet,
    SigItem, SigItemRef, SigModuleSection, SigVersion, SignatureFile, Surface, Type, TypeAlias,
    TypeRecMember,
};
use crate::error::Error;
use crate::span::Span;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// An error encountered while replaying a changelog — a structural
/// inconsistency such as removing a name that was never added. These are
/// internal contract violations in a *parsed* sig (the parser admits the
/// grammar; replay enforces history coherence), surfaced as a structured
/// [`Error`] with the offending span.
pub type ReplayError = Error;

/// The version-exact nominal declarations reachable from one frozen host
/// function origin. Each host function owns a separate logical closure: the
/// map is never unioned with declarations reachable from another root or from
/// a later live interface. [`Arc`] only shares immutable declaration storage
/// between roots frozen against the same replay generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FrozenTypeClosure {
    pub(crate) declarations: BTreeMap<QualifiedName, Arc<FrozenTypeDeclaration>>,
}

/// One declaration in a [`FrozenTypeClosure`], together with the imports from
/// that declaration's own module section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrozenTypeDeclaration {
    pub(crate) declaration: FrozenTypeItem,
    pub(crate) imports: Vec<Import>,
}

/// The three nominal declaration forms a host-function signature may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FrozenTypeItem {
    HostType(HostType<Surface>),
    TypeAlias(TypeAlias<Surface>),
    Newtype(Box<Newtype<Surface>>),
}

/// The interface a changelog describes after replay: the live items plus
/// the removed items recoverable for deprecation re-emit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayedInterface {
    /// The current interface — the live contract surface.
    pub current: ContractSnapshot,
    /// Items removed across the replayed history, each with the frozen
    /// signature (side + declaration) recovered from its last
    /// `add` / `modify` origin. A name added and removed without ever
    /// being re-added on that side appears here; re-adding the same side
    /// clears that side's retirement, while an opposite-side re-add preserves
    /// an explicit retirement. A host-to-export flip also retires its old host
    /// requirement. Phase 4's deprecated-re-emit reads this to re-emit the
    /// removed item against unchanged host source.
    pub removed: Vec<RemovedItem>,
    /// The frozen declaration of every *live* item — its last
    /// `add` / `modify` origin's `SigItem`. `kio sig compact` materializes
    /// these into the synthesized boundary block so the collapsed
    /// history's surviving interface is re-expressed as declarations
    /// (the `current` snapshot carries only normalized strings, not the
    /// AST). Keyed in module-qualified-name order.
    pub live_frozen: Vec<RemovedItem>,
}

/// One item removed across the replayed history — its normalized
/// contract entry, the frozen `SigItem` declaration recovered from its
/// last `add` / `modify` origin (the source of the deprecation re-emit),
/// the `import` clauses of that origin's module section (so a re-emit of
/// the frozen declaration re-qualifies cross-module references exactly
/// as the original did — `kio sig compact` rebuilds a boundary section's
/// `uses` from these), and the version at which it was last removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedItem {
    pub entry: ContractEntry,
    pub frozen: SigItem<Surface>,
    /// The `import` clauses of the module section the frozen declaration
    /// came from — needed to re-qualify its cross-module references.
    pub imports: Vec<Import>,
    /// Present exactly when `frozen` is a host function. Together with the
    /// outer `entry`, `frozen`, and `uses`, this captures one complete root at
    /// one replay generation without duplicating that root's data.
    pub(crate) frozen_type_closure: Option<FrozenTypeClosure>,
    /// Complete recursive declaration epoch owning this member at its last
    /// add/modify origin. Compact uses it to reconstruct the mandatory
    /// version-leading `with` group.
    pub(crate) recursive_context: Option<Arc<SigModuleSection<Surface>>>,
    pub removed_at_version: u32,
}

/// Replay a parsed signature changelog into its current interface plus
/// the recoverable removed items.
///
/// Versions are applied in ascending `version` order; within a version,
/// `breaking` then `nonbreaking`, and within a section `add` → `modify`
/// → `remove` (the partition is presentational, so the order only fixes
/// a deterministic application sequence). References inside each module
/// section are resolved through *that section's* `import` clauses, so a
/// type resolves at its own version even if a live type later changed.
pub fn replay(file: &SignatureFile<Surface>) -> Result<ReplayedInterface, ReplayError> {
    let mut state = ReplayState::default();

    let mut versions: Vec<&_> = file.versions.iter().collect();
    versions.sort_by_key(|v| v.version);

    for version in versions {
        state.apply_version(version)?;
    }

    Ok(state.finish())
}

/// Construct an intentionally unvalidated replay only for tests that exercise
/// a later phase's fail-closed handling of an impossible retained artifact.
/// Production signature input always enters through [`replay`].
#[cfg(test)]
pub(crate) fn replay_unvalidated_for_downstream_defense(
    file: &SignatureFile<Surface>,
) -> Result<ReplayedInterface, ReplayError> {
    let mut state = ReplayState::default();
    let mut versions = file.versions.iter().collect::<Vec<_>>();
    versions.sort_by_key(|version| version.version);
    for version in versions {
        state.apply_version_without_artifact_validation(version)?;
    }
    Ok(state.finish())
}

/// Mutable replay accumulator. `live` is the current interface; `frozen`
/// keeps the last-known origin per name so a names-only `remove` recovers
/// its exact side, signature, and closure. Retirements are keyed by name and
/// side: installing a new incarnation clears only that side's old retirement,
/// while a host-to-export flip retires the actually-live host incarnation.
#[derive(Clone, Default)]
struct ReplayState {
    live: HashMap<QualifiedName, ContractEntry>,
    frozen: HashMap<QualifiedName, FrozenRecord>,
    retired: BTreeMap<(QualifiedName, SideKey), RemovedItem>,
    /// The exact type generation visible after the last replayed version.
    type_catalog: TypeCatalog,
}

type TypeCatalog = BTreeMap<QualifiedName, Arc<FrozenTypeDeclaration>>;

#[derive(Debug, Clone)]
struct FrozenRecord {
    entry: ContractEntry,
    frozen: SigItem<Surface>,
    imports: Vec<Import>,
    frozen_type_closure: Option<FrozenTypeClosure>,
    recursive_context: Option<Arc<SigModuleSection<Surface>>>,
    canonical: Option<SigItem<Prime>>,
    canonical_recursive_context: Option<Arc<SigModuleSection<Prime>>>,
    canonical_comptime_import: Option<Import>,
}

#[derive(Debug, Clone)]
struct ContextDeclaration {
    item: SigItem<Surface>,
    imports: Vec<Import>,
    span: Span,
    group_id: Option<usize>,
    recursive_context: Option<Arc<SigModuleSection<Surface>>>,
}

type VersionContext = BTreeMap<QualifiedName, ContextDeclaration>;

impl FrozenRecord {
    fn removed_item(&self, removed_at_version: u32) -> RemovedItem {
        RemovedItem {
            entry: self.entry.clone(),
            frozen: self.frozen.clone(),
            imports: self.imports.clone(),
            frozen_type_closure: self.frozen_type_closure.clone(),
            recursive_context: self.recursive_context.clone(),
            removed_at_version,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SideKey {
    Env,
    Export,
}

impl From<ContractSide> for SideKey {
    fn from(side: ContractSide) -> Self {
        match side {
            ContractSide::Env => Self::Env,
            ContractSide::Export => Self::Export,
        }
    }
}

/// Whether a module section came from an `add` block (insert-only, a
/// brand-new name) or a `modify` block (reshape an existing item — the
/// name must already be present).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Add,
    Modify,
}

impl ReplayState {
    fn apply_version(&mut self, version: &SigVersion<Surface>) -> Result<(), ReplayError> {
        let (mut staged, pending) = self.stage_version(version)?;
        let pre_removal =
            super::validate::canonicalize_pending(self.canonical_live_epoch(), &pending)?;
        super::validate::validate_epoch(&pre_removal)?;
        staged.install_canonical_declarations(&pending, &pre_removal);
        let live_epoch = staged.canonical_live_epoch();
        super::validate::validate_recursive_context_partition(&live_epoch)?;
        super::validate::validate_epoch(&live_epoch)?;
        *self = staged;
        Ok(())
    }

    fn stage_version(
        &self,
        version: &SigVersion<Surface>,
    ) -> Result<
        (
            Self,
            BTreeMap<QualifiedName, super::validate::PendingDeclaration>,
        ),
        ReplayError,
    > {
        let context = build_version_context(&version.with)?;
        validate_version_references(version, &context)?;
        let sets = [version.breaking.as_ref(), version.nonbreaking.as_ref()];
        let (version_catalog, next_catalog) = self.stage_type_catalog(
            version.breaking.as_ref(),
            version.nonbreaking.as_ref(),
            &context,
        );
        validate_context_peers(
            &self.frozen,
            &self.live,
            version,
            &context,
            &version_catalog,
        )?;
        let mut staged = self.clone();
        for (name, declaration) in &context {
            if staged.live.contains_key(name)
                && let Some(record) = staged.frozen.get_mut(name)
            {
                // An unreferenced complete group advances unchanged peers to
                // a new recursive-context epoch (needed when one old SCC
                // splits). An unreferenced ordinary declaration clears the
                // old epoch for an unchanged peer that became acyclic.
                record.recursive_context = declaration.recursive_context.clone();
            }
        }
        for set in sets.into_iter().flatten() {
            staged.apply_change_set(set, version.version, &version_catalog, &context)?;
        }
        staged.type_catalog = next_catalog;
        let pending = pending_version_declarations(version, &context);
        Ok((staged, pending))
    }

    #[cfg(test)]
    fn apply_version_without_artifact_validation(
        &mut self,
        version: &SigVersion<Surface>,
    ) -> Result<(), ReplayError> {
        let (staged, _) = self.stage_version(version)?;
        *self = staged;
        Ok(())
    }

    fn canonical_live_epoch(&self) -> super::validate::CanonicalEpoch {
        self.live
            .keys()
            .map(|name| {
                let record = self
                    .frozen
                    .get(name)
                    .expect("every live signature entry has a frozen origin");
                (
                    name.clone(),
                    super::validate::CanonicalDeclaration {
                        item: record
                            .canonical
                            .clone()
                            .expect("every committed signature origin has validated Kio'"),
                        recursive_context: record.canonical_recursive_context.clone(),
                        comptime_import: record.canonical_comptime_import.clone(),
                    },
                )
            })
            .collect()
    }

    fn install_canonical_declarations(
        &mut self,
        pending: &BTreeMap<QualifiedName, super::validate::PendingDeclaration>,
        canonical: &super::validate::CanonicalEpoch,
    ) {
        for name in pending.keys() {
            let Some(validated) = canonical.get(name) else {
                continue;
            };
            let Some(record) = self.frozen.get_mut(name) else {
                continue;
            };
            record.canonical = Some(validated.item.clone());
            record.canonical_recursive_context = validated.recursive_context.clone();
            record.canonical_comptime_import = validated.comptime_import.clone();
            let entry = super::surface::entry_from_sig_item(
                &name.module_path,
                &validated.item,
                &|segments| segments.to_vec(),
            );
            record.entry = entry.clone();
            if let Some(live) = self.live.get_mut(name) {
                *live = entry;
            }
        }
    }

    /// Compute one sealed version's type environment before freezing any root
    /// in that version. This permits references before textual declaration and
    /// across the breaking/nonbreaking presentation split, without leaking a
    /// declaration backward from a later version.
    fn stage_type_catalog(
        &self,
        breaking: Option<&SigChangeSet<Surface>>,
        nonbreaking: Option<&SigChangeSet<Surface>>,
        context: &VersionContext,
    ) -> (TypeCatalog, TypeCatalog) {
        // Origins see every declaration introduced or modified in this sealed
        // version. Removals are deliberately applied only to `next_catalog`:
        // an add-then-remove in sealed history still has a recoverable origin.
        let mut origin_catalog = self.type_catalog.clone();
        stage_context_types(&mut origin_catalog, context);
        for set in [breaking, nonbreaking].into_iter().flatten() {
            for section in &set.add {
                stage_section_types(&mut origin_catalog, section);
            }
            for section in &set.modify {
                stage_section_types(&mut origin_catalog, section);
            }
        }

        // The carried catalog follows replay's deterministic operation order,
        // including remove-then-readd sequences across the two partitions.
        let mut next_catalog = self.type_catalog.clone();
        for set in [breaking, nonbreaking].into_iter().flatten() {
            for section in &set.add {
                stage_final_section_types(&mut next_catalog, &origin_catalog, section);
            }
            for reference in &set.add_refs {
                stage_context_for_ref(&mut next_catalog, context, reference);
            }
            for section in &set.modify {
                stage_final_section_types(&mut next_catalog, &origin_catalog, section);
            }
            for reference in &set.modify_refs {
                stage_context_for_ref(&mut next_catalog, context, reference);
            }
            for reference in &set.remove_refs {
                next_catalog.remove(&qualified_ref(reference));
            }
        }
        (origin_catalog, next_catalog)
    }

    fn apply_change_set(
        &mut self,
        set: &SigChangeSet<Surface>,
        version: u32,
        version_catalog: &TypeCatalog,
        context: &VersionContext,
    ) -> Result<(), ReplayError> {
        for section in &set.add {
            self.apply_section(section, SectionKind::Add, version, version_catalog, None)?;
        }
        for reference in &set.add_refs {
            self.apply_reference(
                reference,
                SectionKind::Add,
                version,
                version_catalog,
                context,
            )?;
        }
        for section in &set.modify {
            self.apply_section(section, SectionKind::Modify, version, version_catalog, None)?;
        }
        for reference in &set.modify_refs {
            self.apply_reference(
                reference,
                SectionKind::Modify,
                version,
                version_catalog,
                context,
            )?;
        }
        for reference in &set.remove_refs {
            self.apply_remove_reference(reference, version)?;
        }
        Ok(())
    }

    fn apply_reference(
        &mut self,
        reference: &SigItemRef,
        kind: SectionKind,
        version: u32,
        version_catalog: &TypeCatalog,
        context: &VersionContext,
    ) -> Result<(), ReplayError> {
        let qn = qualified_ref(reference);
        let declaration = context.get(&qn).ok_or_else(|| {
            Error::bridge(
                reference.span,
                format!(
                    "signature operation references `{qn}`, but this version's `with` block does not declare it"
                ),
            )
        })?;
        let section = SigModuleSection {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            path: reference.path.clone(),
            imports: declaration.imports.clone(),
            items: vec![declaration.item.clone()],
            span: declaration.span,
        };
        self.apply_section(
            &section,
            kind,
            version,
            version_catalog,
            declaration.recursive_context.clone(),
        )
    }

    fn apply_remove_reference(
        &mut self,
        reference: &SigItemRef,
        version: u32,
    ) -> Result<(), ReplayError> {
        let qn = qualified_ref(reference);
        self.live.remove(&qn);
        let Some(record) = self.frozen.get(&qn).cloned() else {
            return Err(Error::bridge(
                reference.span,
                format!(
                    "signature changelog removes `{qn}`, which was never added or modified in an earlier version"
                ),
            ));
        };
        self.retired
            .insert((qn, record.entry.side.into()), record.removed_item(version));
        Ok(())
    }

    fn apply_section(
        &mut self,
        section: &SigModuleSection<Surface>,
        kind: SectionKind,
        version: u32,
        version_catalog: &TypeCatalog,
        recursive_context: Option<Arc<SigModuleSection<Surface>>>,
    ) -> Result<(), ReplayError> {
        let module_path = module_path_str(&section.path);
        let qualify = section_qualifier(section, version_catalog);
        for item in &section.items {
            if let SigItem::HostType(host) = item {
                crate::pass::typecheck_core::check_host_type_parameters(host)?;
            }
            let projected_item = super::project_signature_item(item);
            let imports = super::record::imports_needed_by(
                std::slice::from_ref(&projected_item),
                section.imports.clone(),
            );
            let entry = super::surface::PublicDeclaration::from_origin(
                &module_path,
                projected_item,
                &qualify,
            );
            let projected_item = entry.item;
            let entry = entry.entry;
            let qn = entry.name.clone();
            if kind == SectionKind::Modify && !self.live.contains_key(&qn) {
                if self.frozen.contains_key(&qn) {
                    return Err(Error::bridge(
                        item_span(&projected_item),
                        format!(
                            "signature changelog modifies `{qn}`, but that declaration was removed"
                        ),
                    )
                    .with_help("use `add` to reintroduce a removed declaration"));
                }
                return Err(Error::bridge(
                    item_span(&projected_item),
                    format!(
                        "signature changelog modifies `{qn}`, which was never added in an earlier version"
                    ),
                ));
            }
            if kind == SectionKind::Add && self.live.contains_key(&qn) {
                return Err(Error::bridge(
                    item_span(&projected_item),
                    format!(
                        "signature changelog adds `{qn}`, but that declaration is already live; use `modify` to reshape it"
                    ),
                ));
            }
            // A side flip implicitly retires only an actually-live host
            // requirement. Export provisions are retained only by an explicit
            // `remove`; a stale frozen declaration is never retired again.
            if self.live.contains_key(&qn)
                && let Some(old) = self.frozen.get(&qn).cloned()
                && old.entry.side == ContractSide::Env
                && entry.side != ContractSide::Env
            {
                self.retired.insert(
                    (qn.clone(), old.entry.side.into()),
                    old.removed_item(version),
                );
            }

            let frozen_type_closure = match &projected_item {
                SigItem::HostFn(host_fn) => Some(freeze_host_fn_type_closure(
                    host_fn,
                    &module_path,
                    &imports,
                    version_catalog,
                )?),
                SigItem::HostType(_)
                | SigItem::TypeAlias(_)
                | SigItem::Newtype(_)
                | SigItem::TypeRecGroup(_)
                | SigItem::ExportFn(_) => None,
            };

            // A new incarnation clears only a pending retirement on its own
            // side. An opposite-side retirement remains distinct history.
            self.retired.remove(&(qn.clone(), entry.side.into()));
            self.frozen.insert(
                qn.clone(),
                FrozenRecord {
                    entry: entry.clone(),
                    frozen: projected_item,
                    imports,
                    frozen_type_closure,
                    recursive_context: recursive_context.clone(),
                    canonical: None,
                    canonical_recursive_context: None,
                    canonical_comptime_import: None,
                },
            );
            self.live.insert(qn, entry);
        }
        Ok(())
    }

    fn finish(self) -> ReplayedInterface {
        // The live items' frozen declarations, in module-qualified-name
        // order — compact materializes these into the boundary block.
        let mut live_frozen: Vec<RemovedItem> = self
            .live
            .keys()
            .filter_map(|name| self.frozen.get(name).map(|record| record.removed_item(0)))
            .collect();
        live_frozen.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));

        let current = ContractSnapshot {
            items: self.live.into_iter().collect(),
        };
        let removed = self.retired.into_values().collect();
        ReplayedInterface {
            current,
            removed,
            live_frozen,
        }
    }
}

fn build_version_context(
    sections: &[SigModuleSection<Surface>],
) -> Result<VersionContext, ReplayError> {
    let mut context = VersionContext::new();
    let mut next_group_id = 0_usize;
    for section in sections {
        let module_path = module_path_str(&section.path);
        for item in section.items.iter().map(super::project_signature_item) {
            match item {
                SigItem::Newtype(newtype) if newtype.rec_span.is_some() => {
                    let group_id = next_group_id;
                    next_group_id += 1;
                    let span = newtype.meta.span;
                    let item = SigItem::Newtype(newtype);
                    let context_imports = super::record::imports_needed_by(
                        std::slice::from_ref(&item),
                        section.imports.clone(),
                    );
                    let recursive_context = Arc::new(SigModuleSection {
                        leading_trivia: section.leading_trivia.clone(),
                        trailing_trivia: section.trailing_trivia.clone(),
                        path: section.path.clone(),
                        imports: context_imports.clone(),
                        items: vec![item.clone()],
                        span: section.span,
                    });
                    insert_context_declaration(
                        &mut context,
                        &module_path,
                        item,
                        context_imports,
                        span,
                        Some(group_id),
                        Some(recursive_context),
                    )?;
                }
                SigItem::TypeRecGroup(group) => {
                    let group_id = next_group_id;
                    next_group_id += 1;
                    let group_item = SigItem::TypeRecGroup(group.clone());
                    let group_imports = super::record::imports_needed_by(
                        std::slice::from_ref(&group_item),
                        section.imports.clone(),
                    );
                    let recursive_context = Arc::new(SigModuleSection {
                        leading_trivia: section.leading_trivia.clone(),
                        trailing_trivia: section.trailing_trivia.clone(),
                        path: section.path.clone(),
                        imports: group_imports.clone(),
                        items: vec![group_item],
                        span: section.span,
                    });
                    for member in &group.members {
                        let item = sig_item_from_type_rec_member(member).ok_or_else(|| {
                            Error::bridge(
                                member.meta().span,
                                "a signature-file recursive group contains only Kio' `type` and `newtype` declarations",
                            )
                        })?;
                        insert_context_declaration(
                            &mut context,
                            &module_path,
                            item,
                            group_imports.clone(),
                            member.meta().span,
                            Some(group_id),
                            Some(recursive_context.clone()),
                        )?;
                    }
                }
                item => {
                    let span = item_span(&item);
                    let imports = super::record::imports_needed_by(
                        std::slice::from_ref(&item),
                        section.imports.clone(),
                    );
                    insert_context_declaration(
                        &mut context,
                        &module_path,
                        item,
                        imports.clone(),
                        span,
                        None,
                        None,
                    )?;
                }
            }
        }
    }
    Ok(context)
}

fn insert_context_declaration(
    context: &mut VersionContext,
    module_path: &str,
    item: SigItem<Surface>,
    imports: Vec<Import>,
    span: Span,
    group_id: Option<usize>,
    recursive_context: Option<Arc<SigModuleSection<Surface>>>,
) -> Result<(), ReplayError> {
    let qn = QualifiedName::new(module_path, sig_item_name(&item));
    if context
        .insert(
            qn.clone(),
            ContextDeclaration {
                item,
                imports,
                span,
                group_id,
                recursive_context,
            },
        )
        .is_some()
    {
        return Err(Error::bridge(
            span,
            format!("signature version context declares `{qn}` more than once"),
        ));
    }
    Ok(())
}

fn validate_version_references(
    version: &SigVersion<Surface>,
    context: &VersionContext,
) -> Result<(), ReplayError> {
    validate_version_operation_targets(version)?;

    for set in [version.breaking.as_ref(), version.nonbreaking.as_ref()]
        .into_iter()
        .flatten()
    {
        for reference in set.add_refs.iter().chain(&set.modify_refs) {
            let qn = qualified_ref(reference);
            let Some(declaration) = context.get(&qn) else {
                return Err(Error::bridge(
                    reference.span,
                    format!(
                        "signature operation references `{qn}`, but this version's `with` block does not declare it"
                    ),
                ));
            };
            if declaration.group_id.is_none() {
                return Err(Error::bridge(
                    reference.span,
                    format!("nonrecursive declaration `{qn}` cannot be targeted from `with`"),
                )
                .with_secondary(
                    declaration.span,
                    "this is an ordinary nonrecursive declaration",
                )
                .with_help("write the declaration inline in its `add` or `modify` operation"));
            }
        }
    }

    Ok(())
}

/// Within one operation kind, a version may target one declaration only once
/// across both verdict partitions and all inline/reference spellings. Replay
/// intentionally applies different kinds in `add` -> `modify` -> `remove`
/// order, so the operation kind is part of the uniqueness key.
fn validate_version_operation_targets(version: &SigVersion<Surface>) -> Result<(), ReplayError> {
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Operation {
        Add,
        Modify,
        Remove,
    }

    fn insert(
        seen: &mut BTreeMap<(Operation, QualifiedName), Span>,
        operation: Operation,
        name: QualifiedName,
        span: Span,
    ) -> Result<(), ReplayError> {
        let Some(previous) = seen.insert((operation, name.clone()), span) else {
            return Ok(());
        };
        Err(Error::bridge(
            span,
            format!("signature version targets `{name}` more than once"),
        )
        .with_secondary(previous, "the first operation target is here"))
    }

    let mut seen = BTreeMap::new();
    for set in [version.breaking.as_ref(), version.nonbreaking.as_ref()]
        .into_iter()
        .flatten()
    {
        for (operation, sections) in [(Operation::Add, &set.add), (Operation::Modify, &set.modify)]
        {
            for section in sections {
                let module_path = module_path_str(&section.path);
                for item in section.items.iter().map(super::project_signature_item) {
                    match item {
                        SigItem::TypeRecGroup(group) => {
                            for member in &group.members {
                                if let Some(name) = sig_type_rec_member_name(member) {
                                    insert(
                                        &mut seen,
                                        operation,
                                        QualifiedName::new(&module_path, name),
                                        member.meta().span,
                                    )?;
                                }
                            }
                        }
                        item => {
                            let span = item_span(&item);
                            insert(
                                &mut seen,
                                operation,
                                QualifiedName::new(&module_path, sig_item_name(&item)),
                                span,
                            )?;
                        }
                    }
                }
            }
        }
        for (operation, references) in [
            (Operation::Add, &set.add_refs),
            (Operation::Modify, &set.modify_refs),
            (Operation::Remove, &set.remove_refs),
        ] {
            for reference in references {
                insert(
                    &mut seen,
                    operation,
                    qualified_ref(reference),
                    reference.span,
                )?;
            }
        }
    }

    Ok(())
}

fn validate_context_peers(
    frozen: &HashMap<QualifiedName, FrozenRecord>,
    live: &HashMap<QualifiedName, ContractEntry>,
    version: &SigVersion<Surface>,
    context: &VersionContext,
    catalog: &TypeCatalog,
) -> Result<(), ReplayError> {
    let targets = version_target_spans(version);
    let directly_targeted_groups = targets
        .context_refs
        .keys()
        .filter_map(|name| {
            context
                .get(name)
                .and_then(|declaration| declaration.group_id)
        })
        .collect::<BTreeSet<_>>();
    // A context-only declaration is legitimate precisely when it is an
    // unchanged peer carried across a touched old context, or joins a new
    // group whose changed member is named by exact reference. Those are the
    // split/becomes-acyclic and merge cases respectively; repeating any other
    // declaration would make `with` an unrecorded operation partition.
    let touched_old_contexts = targets
        .context_refs
        .keys()
        .chain(targets.inline.keys())
        .chain(targets.removed.keys())
        .filter_map(|name| {
            if !live.contains_key(name) {
                return None;
            }
            frozen
                .get(name)
                .and_then(|record| record.recursive_context.as_ref())
                .map(|context| Arc::as_ptr(context) as usize)
        })
        .collect::<BTreeSet<_>>();
    for (name, declaration) in context {
        if targets.context_refs.contains_key(name)
            || !sig_item_is_contract_visible(&declaration.item)
        {
            continue;
        }
        if let Some(span) = targets.inline.get(name) {
            return Err(Error::bridge(
                *span,
                format!("`with` declaration `{name}` is also written inline"),
            )
            .with_secondary(declaration.span, "the redundant `with` declaration is here")
            .with_help(
                "name recursive context members by exact FQN, and keep ordinary declarations inline",
            ));
        }
        if let Some(span) = targets.removed.get(name) {
            return Err(Error::bridge(
                *span,
                format!("removed declaration `{name}` must not also appear in `with`"),
            )
            .with_secondary(declaration.span, "the redundant `with` declaration is here")
            .with_help("keep only the surviving peers needed by the new recursive context"));
        }
        let section = SigModuleSection {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            path: module_path_from_str(&name.module_path),
            imports: declaration.imports.clone(),
            items: vec![declaration.item.clone()],
            span: declaration.span,
        };
        let qualify = section_qualifier(&section, catalog);
        let entry =
            super::surface::entry_from_sig_item(&name.module_path, &declaration.item, &qualify);
        match live.get(name) {
            Some(current) if current == &entry => {}
            Some(_) => {
                return Err(Error::bridge(
                    declaration.span,
                    format!(
                        "`with` changes supporting declaration `{name}` without naming it in a `modify` operation"
                    ),
                ));
            }
            None => {
                return Err(Error::bridge(
                    declaration.span,
                    format!(
                        "`with` introduces public declaration `{name}` without naming it in an `add` operation"
                    ),
                ));
            }
        }
        let advances_touched_context = frozen
            .get(name)
            .and_then(|record| record.recursive_context.as_ref())
            .map(|context| Arc::as_ptr(context) as usize)
            .is_some_and(|context| touched_old_contexts.contains(&context));
        let joins_targeted_group = declaration
            .group_id
            .is_some_and(|group| directly_targeted_groups.contains(&group));
        if !advances_touched_context && !joins_targeted_group {
            return Err(Error::bridge(
                declaration.span,
                format!("`with` declaration `{name}` does not update a recursive context"),
            )
            .with_help(
                "remove declarations unrelated to a recursive group merge, split, removal, or acyclic transition",
            ));
        }
    }
    Ok(())
}

#[derive(Default)]
struct VersionTargetSpans {
    context_refs: BTreeMap<QualifiedName, Span>,
    inline: BTreeMap<QualifiedName, Span>,
    removed: BTreeMap<QualifiedName, Span>,
}

fn version_target_spans(version: &SigVersion<Surface>) -> VersionTargetSpans {
    fn insert_section(
        targets: &mut BTreeMap<QualifiedName, Span>,
        section: &SigModuleSection<Surface>,
    ) {
        let module = module_path_str(&section.path);
        for item in &section.items {
            if let SigItem::TypeRecGroup(group) = item {
                for member in &group.members {
                    if let Some(name) = sig_type_rec_member_name(member) {
                        targets.insert(QualifiedName::new(&module, name), member.meta().span);
                    }
                }
            } else {
                targets.insert(
                    QualifiedName::new(&module, sig_item_name(item)),
                    item_span(item),
                );
            }
        }
    }

    let mut targets = VersionTargetSpans::default();
    for set in [version.breaking.as_ref(), version.nonbreaking.as_ref()]
        .into_iter()
        .flatten()
    {
        for section in set.add.iter().chain(&set.modify) {
            insert_section(&mut targets.inline, section);
        }
        for reference in set.add_refs.iter().chain(&set.modify_refs) {
            targets
                .context_refs
                .insert(qualified_ref(reference), reference.span);
        }
        for reference in &set.remove_refs {
            targets
                .removed
                .insert(qualified_ref(reference), reference.span);
        }
    }
    targets
}

fn pending_version_declarations(
    version: &SigVersion<Surface>,
    context: &VersionContext,
) -> BTreeMap<QualifiedName, super::validate::PendingDeclaration> {
    let mut pending = BTreeMap::new();
    for (name, declaration) in context {
        pending.insert(
            name.clone(),
            super::validate::PendingDeclaration {
                item: declaration.item.clone(),
                imports: declaration.imports.clone(),
                recursive_context: declaration.recursive_context.clone(),
            },
        );
    }
    for set in [version.breaking.as_ref(), version.nonbreaking.as_ref()]
        .into_iter()
        .flatten()
    {
        for section in set.add.iter().chain(&set.modify) {
            insert_pending_section(&mut pending, section);
        }
    }
    pending
}

fn insert_pending_section(
    pending: &mut BTreeMap<QualifiedName, super::validate::PendingDeclaration>,
    section: &SigModuleSection<Surface>,
) {
    let module_path = module_path_str(&section.path);
    for item in section.items.iter().map(super::project_signature_item) {
        let imports =
            super::record::imports_needed_by(std::slice::from_ref(&item), section.imports.clone());
        let name = QualifiedName::new(&module_path, sig_item_name(&item));
        pending.insert(
            name,
            super::validate::PendingDeclaration {
                item,
                imports,
                recursive_context: None,
            },
        );
    }
}

fn sig_item_is_contract_visible(item: &SigItem<Surface>) -> bool {
    match item {
        SigItem::HostType(_) | SigItem::HostFn(_) | SigItem::ExportFn(_) => true,
        // A declaration occurrence in the signature artifact denotes a
        // contract entry even when the ordinary Kio' spelling omits outer
        // `pub`; this is the established signature-section convention.
        SigItem::TypeAlias(_) | SigItem::Newtype(_) => true,
        SigItem::TypeRecGroup(_) => {
            unreachable!("version contexts index recursive groups by individual member")
        }
    }
}

fn sig_type_rec_member_name(member: &TypeRecMember<Surface>) -> Option<&str> {
    match member {
        TypeRecMember::TypeAlias(alias) => Some(&alias.name),
        TypeRecMember::Newtype(newtype) => Some(&newtype.name),
        TypeRecMember::Labels(_, _) => None,
    }
}

fn sig_item_from_type_rec_member(member: &TypeRecMember<Surface>) -> Option<SigItem<Surface>> {
    match member {
        TypeRecMember::TypeAlias(alias) => Some(SigItem::TypeAlias(alias.clone())),
        TypeRecMember::Newtype(newtype) => Some(SigItem::Newtype(newtype.clone())),
        TypeRecMember::Labels(_, _) => None,
    }
}

fn qualified_ref(reference: &SigItemRef) -> QualifiedName {
    QualifiedName::new(module_path_str(&reference.path), reference.name.clone())
}

fn stage_context_types(catalog: &mut TypeCatalog, context: &VersionContext) {
    for (name, declaration) in context {
        if let Some(item) = frozen_type_item(&declaration.item) {
            catalog.insert(
                name.clone(),
                Arc::new(FrozenTypeDeclaration {
                    declaration: item,
                    imports: declaration.imports.clone(),
                }),
            );
        } else {
            catalog.remove(name);
        }
    }
}

fn stage_context_for_ref(
    catalog: &mut TypeCatalog,
    context: &VersionContext,
    reference: &SigItemRef,
) {
    let qn = qualified_ref(reference);
    let group_id = context
        .get(&qn)
        .and_then(|declaration| declaration.group_id);
    for (name, declaration) in context {
        if name != &qn && (group_id.is_none() || declaration.group_id != group_id) {
            continue;
        }
        if let Some(item) = frozen_type_item(&declaration.item) {
            catalog.insert(
                name.clone(),
                Arc::new(FrozenTypeDeclaration {
                    declaration: item,
                    imports: declaration.imports.clone(),
                }),
            );
        } else {
            catalog.remove(name);
        }
    }
}

fn stage_section_types(catalog: &mut TypeCatalog, section: &SigModuleSection<Surface>) {
    let module_path = module_path_str(&section.path);
    for projected in section.items.iter().map(super::project_signature_item) {
        let qn = QualifiedName::new(&module_path, sig_item_name(&projected));
        if let Some(declaration) = frozen_type_item(&projected) {
            let imports = super::record::imports_needed_by(
                std::slice::from_ref(&projected),
                section.imports.clone(),
            );
            catalog.insert(
                qn,
                Arc::new(FrozenTypeDeclaration {
                    declaration,
                    imports,
                }),
            );
        } else {
            // A same-name side/kind replacement means this generation no
            // longer declares a type, even if an earlier generation did.
            catalog.remove(&qn);
        }
    }
}

fn stage_final_section_types(
    catalog: &mut TypeCatalog,
    origin_catalog: &TypeCatalog,
    section: &SigModuleSection<Surface>,
) {
    let module_path = module_path_str(&section.path);
    for item in section.items.iter().map(super::project_signature_item) {
        let qn = QualifiedName::new(&module_path, sig_item_name(&item));
        if matches!(
            &item,
            SigItem::HostType(_) | SigItem::TypeAlias(_) | SigItem::Newtype(_)
        ) && let Some(declaration) = origin_catalog.get(&qn)
        {
            catalog.insert(qn, declaration.clone());
        } else {
            catalog.remove(&qn);
        }
    }
}

fn sig_item_name(item: &SigItem<Surface>) -> &str {
    match item {
        SigItem::HostType(host) => &host.name,
        SigItem::HostFn(host_fn) => &host_fn.name,
        SigItem::TypeAlias(alias) => &alias.name,
        SigItem::Newtype(newtype) => &newtype.name,
        SigItem::ExportFn(export) => &export.function.name,
        SigItem::TypeRecGroup(group) => group
            .members
            .first()
            .and_then(sig_type_rec_member_name)
            .unwrap_or(""),
    }
}

fn frozen_type_item(item: &SigItem<Surface>) -> Option<FrozenTypeItem> {
    match item {
        SigItem::HostType(host) => Some(FrozenTypeItem::HostType(host.clone())),
        SigItem::TypeAlias(alias) => Some(FrozenTypeItem::TypeAlias(alias.clone())),
        SigItem::Newtype(newtype) => Some(FrozenTypeItem::Newtype(Box::new(newtype.clone()))),
        SigItem::HostFn(_) | SigItem::ExportFn(_) | SigItem::TypeRecGroup(_) => None,
    }
}

fn freeze_host_fn_type_closure(
    declaration: &HostFn<Surface>,
    module_path: &str,
    imports: &[Import],
    catalog: &TypeCatalog,
) -> Result<FrozenTypeClosure, ReplayError> {
    let mut collector = TypeClosureCollector {
        catalog,
        declarations: BTreeMap::new(),
    };
    let mut bound = BTreeSet::new();
    for param in &declaration.params {
        match param {
            HostFnParam::Type(param) => {
                bound.insert(param.name.clone());
            }
            HostFnParam::Value(param) => {
                collector.visit_type(&param.ty, module_path, imports, &bound, &mut Vec::new())?;
            }
        }
    }
    collector.visit_type(
        &declaration.ret,
        module_path,
        imports,
        &bound,
        &mut Vec::new(),
    )?;

    Ok(FrozenTypeClosure {
        declarations: collector.declarations,
    })
}

struct TypeClosureCollector<'a> {
    catalog: &'a TypeCatalog,
    declarations: BTreeMap<QualifiedName, Arc<FrozenTypeDeclaration>>,
}

impl TypeClosureCollector<'_> {
    fn visit_type(
        &mut self,
        ty: &Type<Surface>,
        module_path: &str,
        imports: &[Import],
        bound: &BTreeSet<String>,
        alias_path: &mut Vec<QualifiedName>,
    ) -> Result<(), ReplayError> {
        match ty {
            Type::Path {
                segments,
                args,
                meta,
            } => {
                for arg in args {
                    self.visit_type(arg, module_path, imports, bound, alias_path)?;
                }
                if segments.len() == 1 && bound.contains(&segments[0].name) {
                    return Ok(());
                }
                let head: Vec<String> = segments
                    .iter()
                    .map(|segment| segment.name.clone())
                    .collect();
                let qualified = qualify_type_head(module_path, imports, self.catalog, &head);
                let Some((leaf, prefix)) = qualified.split_last() else {
                    return Ok(());
                };
                let qn = QualifiedName::new(prefix.join("/"), leaf.clone());
                self.visit_declaration(qn, meta.span, alias_path)
            }
            Type::Function { param, ret, .. } => {
                self.visit_type(param, module_path, imports, bound, alias_path)?;
                self.visit_type(ret, module_path, imports, bound, alias_path)
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                self.visit_type(left, module_path, imports, bound, alias_path)?;
                self.visit_type(right, module_path, imports, bound, alias_path)
            }
            Type::Forall { param, body, .. } => {
                let mut body_bound = bound.clone();
                body_bound.insert(param.name.clone());
                self.visit_type(body, module_path, imports, &body_bound, alias_path)
            }
            Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => Ok(()),
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    fn visit_declaration(
        &mut self,
        qn: QualifiedName,
        reference_span: Span,
        alias_path: &mut Vec<QualifiedName>,
    ) -> Result<(), ReplayError> {
        let Some(declaration) = self.catalog.get(&qn).cloned() else {
            return Ok(());
        };
        match &declaration.declaration {
            FrozenTypeItem::HostType(_) => {
                self.declarations.entry(qn).or_insert(declaration.clone());
                Ok(())
            }
            FrozenTypeItem::TypeAlias(alias) => {
                if let Some(cycle_start) = alias_path.iter().position(|name| name == &qn) {
                    let mut cycle: Vec<String> = alias_path[cycle_start..]
                        .iter()
                        .map(ToString::to_string)
                        .collect();
                    cycle.push(qn.to_string());
                    return Err(Error::bridge(
                        reference_span,
                        format!(
                            "signature type-alias cycle has no nominal boundary: {}",
                            cycle.join(" -> ")
                        ),
                    ));
                }
                if self.declarations.contains_key(&qn) {
                    return Ok(());
                }
                self.declarations.insert(qn.clone(), declaration.clone());
                alias_path.push(qn.clone());
                let bound = alias
                    .type_params
                    .iter()
                    .map(|param| param.name.clone())
                    .collect();
                let result = self.visit_type(
                    &alias.body,
                    &qn.module_path,
                    &declaration.imports,
                    &bound,
                    alias_path,
                );
                alias_path.pop();
                result
            }
            FrozenTypeItem::Newtype(newtype) => {
                if self.declarations.contains_key(&qn) {
                    return Ok(());
                }
                self.declarations.insert(qn.clone(), declaration.clone());
                if let Some(payload) = newtype.host_surface().and_then(|surface| surface.payload())
                {
                    let bound = newtype
                        .type_params
                        .iter()
                        .chain(&newtype.existential_params)
                        .map(|param| param.name.clone())
                        .collect();
                    // A nominal declaration is a valid recursive knot. Alias
                    // recursion reached through it is not an alias-only cycle.
                    self.visit_type(
                        payload,
                        &qn.module_path,
                        &declaration.imports,
                        &bound,
                        &mut Vec::new(),
                    )?;
                }
                Ok(())
            }
        }
    }
}

/// A head-qualifier for one module section: resolve a written type head
/// to its fully module-qualified segment list, using the section's
/// `import` clauses and its module's declared type names. Mirrors the
/// resolver's `exported_contract_type_segments` — which decides a bare
/// same-module head by walking the *whole module*
/// (`module_declares_type_name`), not just the declarations local to the
/// referencing site. The version-local catalog is that whole-module oracle:
/// it contains prior declarations plus the complete current sealed version,
/// and no declaration from a later version.
fn section_qualifier<'a>(
    section: &'a SigModuleSection<Surface>,
    catalog: &'a TypeCatalog,
) -> impl Fn(&[String]) -> Vec<String> + 'a {
    let module_path = module_path_str(&section.path);
    move |head| qualify_type_head(&module_path, &section.imports, catalog, head)
}

fn qualify_type_head(
    module_path: &str,
    imports: &[Import],
    catalog: &TypeCatalog,
    head: &[String],
) -> Vec<String> {
    let Some((first, tail)) = head.split_first() else {
        return Vec::new();
    };
    if let Some(path) = selective_import_path(imports, first) {
        let mut out = path;
        out.extend_from_slice(tail);
        return out;
    }
    if let Some(path) = qualified_import_path(imports, first) {
        let mut out = path;
        out.extend_from_slice(tail);
        return out;
    }
    if tail.is_empty() && catalog.contains_key(&QualifiedName::new(module_path, first.clone())) {
        let mut out: Vec<String> = module_path.split('/').map(str::to_owned).collect();
        out.push(first.clone());
        return out;
    }
    head.to_vec()
}

fn selective_import_path(imports: &[Import], name: &str) -> Option<Vec<String>> {
    imports.iter().find_map(|u| match &u.kind {
        ImportKind::Selective { items, from } => {
            let source = items
                .iter()
                .find(|item| item.as_local_name() == Some(name))?
                .as_name()?;
            let mut out: Vec<String> = from.segments.iter().map(|s| s.name.clone()).collect();
            out.push(source.to_owned());
            Some(out)
        }
        _ => None,
    })
}

fn qualified_import_path(imports: &[Import], alias: &str) -> Option<Vec<String>> {
    imports.iter().find_map(|u| match &u.kind {
        ImportKind::Qualified { path, alias: a, .. } if a == alias => {
            Some(path.segments.iter().map(|s| s.name.clone()).collect())
        }
        _ => None,
    })
}

/// The source span of a `SigItem` — used to anchor a replay diagnostic
/// at the offending declaration.
fn item_span(item: &SigItem<Surface>) -> Span {
    match item {
        SigItem::HostType(h) => h.meta.span,
        SigItem::HostFn(h) => h.meta.span,
        SigItem::ExportFn(export) => export.function.meta.span,
        SigItem::TypeAlias(a) => a.meta.span,
        SigItem::Newtype(n) => n.meta.span,
        SigItem::TypeRecGroup(group) => group.meta.span,
    }
}

fn module_path_str(p: &ModulePath) -> String {
    p.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn module_path_from_str(path: &str) -> ModulePath {
    ModulePath {
        segments: path
            .split('/')
            .map(|segment| crate::ast::PathSegment::synth(segment, Span::new(0, 0)))
            .collect(),
        span: Span::new(0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SigItem;
    use crate::pass::parser::parse_signature_file;
    use crate::sig::ContractSide;

    fn replay_src(src: &str) -> ReplayedInterface {
        let file = parse_signature_file(src, None)
            .unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"));
        replay(&file).unwrap_or_else(|e| panic!("replay failed: {e:?}"))
    }

    fn name(m: &str, l: &str) -> QualifiedName {
        QualifiedName::new(m, l)
    }

    fn live_frozen_item<'a>(
        replayed: &'a ReplayedInterface,
        module: &str,
        leaf: &str,
    ) -> &'a RemovedItem {
        replayed
            .live_frozen
            .iter()
            .find(|item| item.entry.name == name(module, leaf))
            .unwrap_or_else(|| panic!("missing live frozen item for {module}.{leaf}"))
    }

    fn live_host_closure<'a>(
        replayed: &'a ReplayedInterface,
        module: &str,
        leaf: &str,
    ) -> &'a FrozenTypeClosure {
        live_frozen_item(replayed, module, leaf)
            .frozen_type_closure
            .as_ref()
            .unwrap_or_else(|| panic!("missing live frozen host closure for {module}.{leaf}"))
    }

    #[test]
    fn type_only_exports_replay_binder_order_and_sealed_unit_stage() {
        let file = parse_signature_file(
            "sig app v(2);
            v(1) { nonbreaking { add { mod api {
                pub pure fn direct[A] -> A -> A;
                pub fn delayed[A]() -> .;
                pub fn adjacent[A][B] -> A -> B -> A;
                pub fn mixed[A](value: A)[B] -> A;
            } } } }
            v(2) { breaking { modify { mod api {
                pub fn delayed[A] -> .;
            } } } }",
            None,
        )
        .unwrap();
        let sealed = crate::sig::replay_through(&file, 1).unwrap();
        let current = replay(&file).unwrap();
        for (leaf, signature, pure) in [
            ("direct", "forall<*>fn(#tv0)->#tv0", true),
            ("delayed", "forall<*>fn(.)->.", false),
            (
                "adjacent",
                "forall<*>forall<*>fn(#tv0)->fn(#tv1)->#tv0",
                false,
            ),
            ("mixed", "forall<*>fn(#tv0)->forall<*>#tv0", false),
        ] {
            let entry = &sealed.current.items[&name("api", leaf)];
            assert_eq!(entry.side, ContractSide::Export);
            assert_eq!(
                entry.kind,
                ContractKind::Fn {
                    signature: signature.to_owned(),
                    pure,
                },
                "{leaf}"
            );
            if leaf != "delayed" {
                assert_eq!(entry, &current.current.items[&name("api", leaf)]);
            }
        }
        assert_eq!(
            current.current.items[&name("api", "delayed")].kind,
            ContractKind::Fn {
                signature: "forall<*>.".to_owned(),
                pure: false,
            }
        );
    }

    #[test]
    fn header_only_is_empty_interface() {
        let r = replay_src("sig app v(1);\n");
        assert!(r.current.items.is_empty());
        assert!(r.removed.is_empty());
    }

    #[test]
    fn version_context_recursive_group_replays_each_referenced_member() {
        let r = replay_src(
            r#"sig app v(1);
v(1) {
  with {
    mod api {
      rec {
        pub type A = B;
        pub type { B : A { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking {
    add {
      api.A;
      api.B;
    }
  }
}
"#,
        );
        assert!(r.current.items.contains_key(&name("api", "A")));
        assert!(r.current.items.contains_key(&name("api", "B")));
        assert!(matches!(
            &live_frozen_item(&r, "api", "A").frozen,
            SigItem::TypeAlias(_)
        ));
        assert!(matches!(
            &live_frozen_item(&r, "api", "B").frozen,
            SigItem::Newtype(_)
        ));
    }

    #[test]
    fn fresh_repartitioned_recursive_context_replays() {
        let r = replay_src(
            r#"sig app v(1);
v(1) {
  with {
    mod api {
      pub rec type { A : A | B {
        pub mk;
        get;
      }; };
    }
  };
  nonbreaking {
    add {
      api.A;
      mod api {
        pub type { B : . {
          mk;
          get;
        }; };
      }
    }
  }
}
"#,
        );
        assert!(r.current.items.contains_key(&name("api", "A")));
        assert!(r.current.items.contains_key(&name("api", "B")));
        let a_context = live_frozen_item(&r, "api", "A")
            .recursive_context
            .as_ref()
            .expect("A keeps a recursive context");
        assert!(matches!(
            &a_context.items[..],
            [SigItem::Newtype(newtype)] if newtype.name == "A" && newtype.rec_span.is_some()
        ));
        assert!(live_frozen_item(&r, "api", "B").recursive_context.is_none());
    }

    #[test]
    fn fresh_context_rejects_multiple_written_recursive_components_before_projection() {
        let source = r#"sig app v(1);
v(1) {
  with {
    mod api {
      rec {
        pub type { A : . | A { pub mk; pub get; }; };
        pub type { B : . | B { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse multiple written SCCs");
        let error = replay(&file).expect_err("fresh replay must not silently split the group");
        assert!(
            error.diag().1.contains("multiple independent components"),
            "got: {error:?}"
        );
    }

    #[test]
    fn fresh_context_rejects_a_written_acyclic_helper_before_projection() {
        let source = r#"sig app v(1);
v(1) {
  with {
    mod api {
      rec {
        pub type { A : A | B { pub mk; pub get; }; };
        pub type { B : . { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse group with acyclic helper");
        let error = replay(&file).expect_err("fresh replay must reject the over-broad group");
        assert!(
            error.diag().1.contains("multiple independent components"),
            "got: {error:?}"
        );
        assert!(
            error
                .diagnostic()
                .secondary()
                .iter()
                .any(|secondary| secondary.text.contains("acyclic component")),
            "got: {error:?}"
        );
    }

    #[test]
    fn signature_projection_preserves_a_qualified_path_and_written_group_container() {
        let source = r#"sig app v(1);
v(1) {
  with {
    mod recursive {
      import dep as d;
      rec {
        pub type Peer = Wrapped | d.External;
        pub type { Wrapped : Peer {
          pub mk;
          pub get;
        }; };
      }
    }
  };
  nonbreaking { add { recursive.Peer; recursive.Wrapped; } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse qualified written group");
        let written = &file.versions[0].with[0].items[0];
        let projected = crate::sig::project_signature_item(written);
        let SigItem::TypeRecGroup(group) = projected else {
            panic!("projection must not silently repartition parsed signature input")
        };
        let TypeRecMember::TypeAlias(alias) = &group.members[0] else {
            panic!("first member must remain the written alias")
        };
        let Type::Sum { left, right, .. } = &alias.body else {
            panic!("alias body must remain the written sum")
        };
        let Type::Path {
            segments: mutual_segments,
            ..
        } = left.as_ref()
        else {
            panic!("left arm must remain the bare mutual edge")
        };
        assert_eq!(
            mutual_segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>(),
            ["Wrapped"]
        );
        let Type::Path { segments, .. } = right.as_ref() else {
            panic!("right arm must remain the qualified external path")
        };
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>(),
            ["d", "External"]
        );
    }

    #[test]
    fn version_contexts_cannot_split_one_maximal_recursive_component() {
        let source = r#"sig app v(1);
v(1) {
  with {
    mod api {
      pub rec type { A : A | B { pub mk; pub get; }; };
      pub rec type { B : B | A { pub mk; pub get; }; };
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse split recursive contexts");
        let error = replay(&file).expect_err("one SCC cannot be split across `with` contexts");
        assert!(
            error.diag().1.contains("is split across `with` contexts"),
            "got: {error:?}"
        );
        assert_eq!(
            error.diagnostic().help(),
            Some(
                "put every member of this maximal recursive component in one complete recursive context"
            ),
            "got: {error:?}"
        );
        assert_eq!(error.diagnostic().secondary().len(), 1, "got: {error:?}");
    }

    #[test]
    fn independent_recursive_singletons_keep_distinct_contexts() {
        let replayed = replay_src(
            r#"sig app v(1);
v(1) {
  with {
    mod api {
      pub rec type { A : . | A { pub mk; pub get; }; };
      pub rec type { B : . | B { pub mk; pub get; }; };
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
"#,
        );
        let a = live_frozen_item(&replayed, "api", "A");
        let b = live_frozen_item(&replayed, "api", "B");
        assert!(!Arc::ptr_eq(
            a.recursive_context.as_ref().expect("A context"),
            b.recursive_context.as_ref().expect("B context"),
        ));
    }

    #[test]
    fn recursive_singletons_can_merge_into_one_context_epoch() {
        let replayed = replay_src(
            r#"sig app v(2);
v(1) {
  with {
    mod api {
      pub rec type { A : . | A { pub mk; pub get; }; };
      pub rec type { B : . | B { pub mk; pub get; }; };
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
v(2) {
  with {
    mod api {
      rec {
        pub type { A : (A | B) | . { pub mk; pub get; }; };
        pub type { B : (B | A) | . { pub mk; pub get; }; };
      }
    }
  };
  breaking { modify { api.A; api.B; } }
}
"#,
        );
        let a = live_frozen_item(&replayed, "api", "A");
        let b = live_frozen_item(&replayed, "api", "B");
        assert!(Arc::ptr_eq(
            a.recursive_context.as_ref().expect("A context"),
            b.recursive_context.as_ref().expect("B context"),
        ));
    }

    #[test]
    fn version_context_reference_must_resolve_exactly() {
        let file = parse_signature_file(
            "sig app v(1);
v(1) { nonbreaking { add { api.A; } } }
",
            None,
        )
        .expect("parse exact reference");
        let error = replay(&file).expect_err("reference without with declaration must fail");
        assert!(
            error.diag().1.contains("`with` block does not declare it"),
            "got: {error:?}"
        );
    }

    #[test]
    fn ordinary_with_declaration_cannot_be_an_operation_target() {
        let source = r#"sig app v(1);
v(1) {
  with {
    mod api { pub type T = .; }
  };
  nonbreaking { add { api.T; } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse ordinary with target");
        let error = replay(&file).expect_err("ordinary declarations must remain inline");
        assert!(
            error
                .diag()
                .1
                .contains("nonrecursive declaration `api.T` cannot be targeted from `with`"),
            "got: {error:?}"
        );
    }

    #[test]
    fn unrelated_unchanged_declaration_is_not_version_context() {
        let source = r#"sig app v(2);
v(1) {
  nonbreaking { add {
    mod api { pub type T = .; }
  } }
}
v(2) {
  with {
    mod api { pub type T = .; }
  };
  nonbreaking { add {
    mod api { pub fn f() -> .; }
  } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse extraneous with declaration");
        let error = replay(&file).expect_err("unrelated context declaration must be rejected");
        assert!(
            error
                .diag()
                .1
                .contains("`with` declaration `api.T` does not update a recursive context"),
            "got: {error:?}"
        );
    }

    #[test]
    fn unchanged_recursive_context_is_not_repeated_for_an_unrelated_change() {
        let source = r#"sig app v(2);
v(1) {
  with {
    mod api {
      pub rec type { Loop : . | Loop { pub mk; pub get; }; };
    }
  };
  nonbreaking { add { api.Loop; } }
}
v(2) {
  with {
    mod api {
      pub rec type { Loop : . | Loop { pub mk; pub get; }; };
    }
  };
  nonbreaking { add {
    mod api { pub fn f() -> .; }
  } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse repeated recursive context");
        let error = replay(&file).expect_err("unchanged context is not relevant to the version");
        assert!(
            error
                .diag()
                .1
                .contains("`with` declaration `api.Loop` does not update a recursive context"),
            "got: {error:?}"
        );
    }

    #[test]
    fn unchanged_ordinary_peer_can_join_a_new_recursive_context() {
        let replayed = replay_src(
            r#"sig app v(2);
v(1) {
  nonbreaking { add {
    mod api {
      pub type A = B;
      pub type { B : . { pub mk; pub get; }; };
    }
  } }
}
v(2) {
  with {
    mod api {
      rec {
        pub type A = B;
        pub type { B : . | A { pub mk; pub get; }; };
      }
    }
  };
  breaking { modify { api.B; } }
}
"#,
        );
        let a = live_frozen_item(&replayed, "api", "A");
        let b = live_frozen_item(&replayed, "api", "B");
        assert!(Arc::ptr_eq(
            a.recursive_context.as_ref().expect("A context"),
            b.recursive_context.as_ref().expect("B context"),
        ));
    }

    #[test]
    fn removed_declaration_is_not_repeated_in_version_context() {
        let source = r#"sig app v(2);
v(1) {
  with {
    mod api {
      pub rec type { Loop : . | Loop { pub mk; pub get; }; };
    }
  };
  nonbreaking { add { api.Loop; } }
}
v(2) {
  with {
    mod api {
      pub rec type { Loop : . | Loop { pub mk; pub get; }; };
    }
  };
  breaking { remove { api.Loop; } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse redundant removed context");
        let error = replay(&file).expect_err("a removed member is not retained in context");
        assert!(
            error
                .diag()
                .1
                .contains("removed declaration `api.Loop` must not also appear in `with`"),
            "got: {error:?}"
        );
        assert_eq!(error.diagnostic().secondary().len(), 1, "got: {error:?}");
    }

    #[test]
    fn recursive_context_member_is_not_modified_inline() {
        let source = r#"sig app v(2);
v(1) {
  with {
    mod api {
      pub rec type { Loop : . | Loop { pub mk; pub get; }; };
    }
  };
  nonbreaking { add { api.Loop; } }
}
v(2) {
  with {
    mod api {
      pub rec type { Loop : (. | Loop) | . { pub mk; pub get; }; };
    }
  };
  breaking { modify {
    mod api {
      pub type { Loop : (. | Loop) | . { pub mk; pub get; }; };
    }
  } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse inline recursive modify");
        let error = replay(&file).expect_err("recursive members use exact operation references");
        assert!(
            error
                .diag()
                .1
                .contains("`with` declaration `api.Loop` is also written inline"),
            "got: {error:?}"
        );
        assert_eq!(error.diagnostic().secondary().len(), 1, "got: {error:?}");
    }

    #[test]
    fn version_context_rejects_an_alias_only_recursive_group() {
        let file = parse_signature_file(
            r#"sig app v(1);
v(1) {
  with {
    mod api {
      rec {
        pub type A = B;
        pub type B = A;
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
"#,
            None,
        )
        .expect("parse recursive context");
        let error = replay(&file).expect_err("alias-only context must fail");
        assert!(
            error
                .diag()
                .1
                .contains("recursive type component has no `newtype` boundary"),
            "got: {error:?}"
        );
    }

    #[test]
    fn version_context_rejects_surface_type_inference_inside_a_group() {
        let file = parse_signature_file(
            r#"sig app v(1);
v(1) {
  with {
    mod api {
      rec {
        pub type A = _ | B;
        pub type { B : A { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
"#,
            None,
        )
        .expect("the shared parser represents `_` before Kio' validation");
        let error = replay(&file).expect_err("signature context must be explicit Kio'");
        assert!(
            error.diag().1.contains("`_` type placeholder"),
            "got: {error:?}"
        );
    }

    fn replay_error_at(src: &str, needle: &str) -> Error {
        let file = parse_signature_file(src, None).expect("parse invalid semantic history");
        let error = replay(&file).expect_err("fresh signature artifact must be rejected");
        let span = error.diagnostic().span;
        let expected = u32::try_from(
            src.find(needle)
                .expect("primary-span needle occurs in source"),
        )
        .expect("test source offset fits in u32");
        let end = expected + u32::try_from(needle.len()).expect("test needle length fits in u32");
        assert!(
            span.start >= expected && span.start < end,
            "diagnostic {span:?} is not anchored in {needle:?} at {expected}: {error:?}"
        );
        error
    }

    #[test]
    fn fresh_history_rejects_direct_negative_recursion() {
        let src = r#"sig app v(1);
v(1) {
  with { mod api {
    pub rec type { Bad : (Bad -> .) { pub mk; pub get; }; };
  } };
  nonbreaking { add { api.Bad; } }
}
"#;
        let error = replay_error_at(src, "Bad -> .");
        assert!(error.diag().1.contains("strictly positive"), "{error:?}");
    }

    #[test]
    fn fresh_history_rejects_alias_hidden_negative_recursion() {
        let src = r#"sig app v(1);
v(1) {
  with { mod api {
    pub rec type { Bad : Negate[Bad] { pub mk; pub get; }; };
  } };
  nonbreaking { add {
    mod api { pub type Negate[A] = A -> .; };
    api.Bad;
  } }
}
"#;
        let error = replay_error_at(src, "Bad]");
        assert!(error.diag().1.contains("strictly positive"), "{error:?}");
    }

    #[test]
    fn fresh_history_rejects_invariant_host_type_recursion() {
        let src = r#"sig app v(1);
v(1) {
  with { mod api {
    pub rec type { Bad : Stream[Bad] { pub mk; pub get; }; };
  } };
  nonbreaking { add {
    mod api { host type Stream[A]; };
    api.Bad;
  } }
}
"#;
        let error = replay_error_at(src, "Bad]");
        assert!(error.diag().1.contains("strictly positive"), "{error:?}");
    }

    #[test]
    fn fresh_history_rejects_kind_mismatch() {
        let src = r#"sig app v(1);
v(1) { nonbreaking { add { mod api {
  host type Box[A];
  type { Bad : Box { pub mk; pub get; }; };
} } } }
"#;
        let error = replay_error_at(src, "Box {");
        assert!(error.diag().1.contains("kind"), "{error:?}");
    }

    #[test]
    fn fresh_history_rejects_nullary_type_overapplication() {
        let src = r#"sig app v(1);
v(1) { nonbreaking { add { mod api {
  host type Atom;
  type { Bad : Atom[.] { pub mk; pub get; }; };
} } } }
"#;
        let error = replay_error_at(src, "Atom[.]");
        assert!(error.diag().1.contains("cannot be applied"), "{error:?}");
    }

    #[test]
    fn fresh_history_rejects_an_unresolved_type() {
        let src = r#"sig app v(1);
v(1) { nonbreaking { add { mod api {
  host fn consume(value: Missing) -> .;
} } } }
"#;
        let error = replay_error_at(src, "Missing");
        assert!(error.diag().1.contains("unbound name"), "{error:?}");
    }

    #[test]
    fn fresh_history_rejects_an_unimported_comptime_type() {
        let src = r#"sig app v(1);
v(1) { nonbreaking { add { mod api {
  host fn consume(value: __Type__) -> .;
} } } }
"#;
        let error = replay_error_at(src, "__Type__");
        assert!(error.diag().1.contains("unbound name"), "{error:?}");
    }

    #[test]
    fn fresh_history_rejects_a_missing_imported_module() {
        let src = r#"sig app v(1);
v(1) { nonbreaking { add { mod api {
  import absent {type Missing};
  host fn consume(value: Missing) -> .;
} } } }
"#;
        let error = replay_error_at(src, "absent");
        assert!(error.diag().1.contains("not found"), "{error:?}");
    }

    #[test]
    fn fresh_history_rejects_a_standalone_alias_only_cycle() {
        let src = r#"sig app v(1);
v(1) { nonbreaking { add { mod api {
  type A = B;
  type B = A;
} } } }
"#;
        let error = replay_error_at(src, "B;");
        assert!(
            error.diag().1.contains("no `newtype` boundary"),
            "{error:?}"
        );
    }

    #[test]
    fn fresh_history_accepts_covariant_phantom_imported_and_shadowed_recursion() {
        replay_src(
            r#"sig app v(1);
v(1) {
  with {
    mod api {
      import shapes {type Box; type Phantom};
      pub rec type { Covariant : Box[Covariant] { pub mk; pub get; }; };
      pub rec type { Ignored : Phantom[Ignored] { pub mk; pub get; }; };
      pub rec type { Shadow[A] : . | (A & Shadow[A]) { pub mk; pub get; }; };
    }
  };
  nonbreaking { add {
    mod shapes {
      pub type { Box[A] : A { pub mk; pub get; }; };
      pub type { Phantom[A] : . { pub mk; pub get; }; };
    };
    mod api { host type A; };
    api.Covariant; api.Ignored; api.Shadow;
  } }
}
"#,
        );
    }

    #[test]
    fn later_origin_import_does_not_rebind_an_earlier_declaration() {
        let replayed = replay_src(
            r#"sig app v(2);
v(1) {
  nonbreaking { add {
    mod left { host type Token; };
    mod api {
      import left {type Token};
      host fn consume_left(value: Token) -> .;
    }
  } }
}
v(2) {
  nonbreaking { add {
    mod right { host type Token; };
    mod api {
      import right {type Token};
      host fn consume_right(value: Token) -> .;
    }
  } }
}
"#,
        );
        let signature = |leaf: &str| {
            let ContractKind::Fn { signature, .. } =
                &replayed.current.items[&name("api", leaf)].kind
            else {
                panic!("{leaf} must be a function")
            };
            signature.as_str()
        };
        assert!(signature("consume_left").contains("@left/Token"));
        assert!(signature("consume_right").contains("@right/Token"));
    }

    #[test]
    fn fresh_canonicalization_work_is_linear_for_independent_declarations() {
        use std::fmt::Write as _;

        for count in [64, 128] {
            let mut source = String::from(
                "sig app v(1);\nv(1) {\n  nonbreaking {\n    add {\n      mod api {\n",
            );
            for index in 0..count {
                writeln!(source, "        pub type T{index} = .;").expect("write source");
            }
            source.push_str("      }\n    }\n  }\n}\n");

            let replayed = replay_src(&source);
            assert_eq!(replayed.current.items.len(), count);
            let work = super::super::validate::take_last_canonicalization_work();
            assert_eq!(work.package_items, count);
            assert_eq!(work.package_builds, count);
        }
    }

    #[test]
    fn fresh_epoch_construction_work_is_linear_across_modules() {
        use std::fmt::Write as _;

        for count in [64, 128] {
            let mut source = String::from("sig app v(1); v(1) { nonbreaking { add {\n");
            for index in 0..count {
                writeln!(source, "mod m{index} {{ pub type T = .; }};").expect("write source");
            }
            source.push_str("} } }\n");

            super::super::validate::take_epoch_module_declaration_visits();
            let replayed = replay_src(&source);
            assert_eq!(replayed.current.items.len(), count);
            assert_eq!(
                super::super::validate::take_epoch_module_declaration_visits(),
                2 * count,
                "pre-removal and post-removal validation each traverse the epoch once"
            );
            let work = super::super::validate::take_last_canonicalization_work();
            assert_eq!(work.package_items, count);
            assert_eq!(work.package_builds, count);
        }
    }

    #[test]
    fn fresh_canonicalization_slice_contains_each_referenced_import() {
        replay_src(
            r#"sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod dep { host type Raw; };
      mod api {
        import dep as d;
        pub type Wrapped = d.Raw;
      }
    }
  }
}
"#,
        );
        let work = super::super::validate::take_last_canonicalization_work();
        assert_eq!(work.package_items, 3);
        assert_eq!(work.package_builds, 2);
    }

    #[test]
    fn fresh_canonicalization_slice_routes_imported_module_type_paths() {
        let replayed = replay_src(
            r#"sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod provider { host type Tag; };
      mod foo/bar { host type Tag; };
      mod api {
        import provider as provider;
        import foo/bar as nested;
        host fn consume_shallow(value: provider.Tag) -> .;
        host fn consume_nested(value: nested.Tag) -> .;
      }
    }
  }
}
"#,
        );
        let signature = |leaf: &str| {
            let ContractKind::Fn { signature, .. } =
                &replayed.current.items[&name("api", leaf)].kind
            else {
                panic!("{leaf} must be a function")
            };
            signature.as_str()
        };
        assert!(signature("consume_shallow").contains("@provider/Tag"));
        assert!(signature("consume_nested").contains("@foo/bar/Tag"));

        let work = super::super::validate::take_last_canonicalization_work();
        assert_eq!(work.package_items, 6);
        assert_eq!(work.package_builds, 4);
    }

    #[test]
    fn fresh_canonicalization_slice_prefers_a_two_segment_import_alias() {
        let replayed = replay_src(
            r#"sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod imported { host type Tag; };
      mod shadow { host type Tag; };
      mod api {
        import imported as shadow;
        host fn consume(value: shadow.Tag) -> .;
      }
    }
  }
}
"#,
        );
        let ContractKind::Fn { signature, .. } =
            &replayed.current.items[&name("api", "consume")].kind
        else {
            panic!("consume must be a function")
        };
        assert!(signature.contains("@imported/Tag"));
        assert!(!signature.contains("@shadow/Tag"));

        let work = super::super::validate::take_last_canonicalization_work();
        assert_eq!(work.package_items, 4);
        assert_eq!(work.package_builds, 3);
    }

    #[test]
    fn unchanged_recursive_peer_advances_to_the_versions_context_epoch() {
        let r = replay_src(
            r#"sig app v(2);
v(1) {
  with {
    mod api {
      rec {
        pub type A = B;
        pub type { B : A { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
v(2) {
  with {
    mod api {
      rec {
        pub type A = . | B;
        pub type { B : A { pub mk; pub get; }; };
      }
    }
  };
  breaking { modify { api.A; } }
}
"#,
        );
        let b = live_frozen_item(&r, "api", "B");
        let context = b
            .recursive_context
            .as_ref()
            .expect("unchanged peer retains the latest complete group");
        let SigItem::TypeRecGroup(group) = &context.items[0] else {
            panic!("expected recursive context")
        };
        let TypeRecMember::TypeAlias(alias) = &group.members[0] else {
            panic!("expected A alias")
        };
        assert!(matches!(alias.body, Type::Sum { .. }));
    }

    #[test]
    fn changed_recursive_peer_must_have_its_own_modify_reference() {
        let file = parse_signature_file(
            r#"sig app v(2);
v(1) {
  with {
    mod api {
      rec {
        pub type A = B;
        pub type { B : A { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
v(2) {
  with {
    mod api {
      rec {
        pub type A = B;
        pub type { B : . | A { pub mk; pub get; }; };
      }
    }
  };
  breaking { modify { api.A; } }
}
"#,
            None,
        )
        .expect("parse changed peer context");
        let error = replay(&file).expect_err("silent peer change must be rejected");
        assert!(
            error
                .diag()
                .1
                .contains("without naming it in a `modify` operation"),
            "got: {error:?}"
        );
    }

    #[test]
    fn recursive_member_cannot_be_modified_both_inline_and_by_reference() {
        let source = r#"sig app v(2);
v(1) {
  with {
    mod api {
      rec {
        pub type { A : A | B { pub mk; pub get; }; };
        pub type { B : B | A { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
v(2) {
  with {
    mod api {
      rec {
        pub type { A : (A | B) | . { pub mk; pub get; }; };
        pub type { B : B | A { pub mk; pub get; }; };
      }
    }
  };
  breaking { modify {
    mod api {
      pub type { A : (A | B) | . { pub mk; pub get; }; };
    };
    api.A;
  } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse duplicate operation target");
        let error = replay(&file).expect_err("cross-form duplicate must be rejected");
        let expected = u32::try_from(source.rfind("api.A;").expect("duplicate reference"))
            .expect("test source offset fits u32");
        assert_eq!(error.diagnostic().span.start, expected, "got: {error:?}");
        assert!(
            error.diag().1.contains("targets `api.A` more than once"),
            "got: {error:?}"
        );
        assert_eq!(error.diagnostic().secondary().len(), 1, "got: {error:?}");
    }

    #[test]
    fn ordinary_inline_signature_operation_remains_valid() {
        replay_src(
            r#"sig app v(1);
v(1) {
  nonbreaking { add {
    mod api {
      pub type { Token : . { pub mk; pub get; }; };
    }
  } }
}
"#,
        );
    }

    #[test]
    fn operation_target_is_unique_across_verdicts_and_remove_spellings() {
        let source = r#"sig app v(2);
v(1) {
  nonbreaking { add {
    mod api { pub type { Token : . { pub mk; pub get; }; }; }
  } }
}
v(2) {
  breaking { remove {  api.Token;  } };
  nonbreaking { remove { api.Token; } }
}
"#;
        let file = parse_signature_file(source, None).expect("parse duplicate removal targets");
        let error = replay(&file).expect_err("one version cannot remove one target twice");
        assert!(
            error
                .diag()
                .1
                .contains("targets `api.Token` more than once"),
            "got: {error:?}"
        );
        assert_eq!(error.diagnostic().secondary().len(), 1, "got: {error:?}");
    }

    #[test]
    fn different_operation_kinds_may_target_one_name_in_one_version() {
        let replayed = replay_src(
            r#"sig app v(2);
v(1) {
  nonbreaking { add {
    mod api { pub type { Token : . { pub mk; pub get; }; }; }
  } }
}
v(2) {
  breaking { remove { api.Token; } };
  nonbreaking { add {
    mod api { pub type { Token : . | . { pub mk; pub get; }; }; }
  } }
}
"#,
        );
        assert!(matches!(
            &live_frozen_item(&replayed, "api", "Token").frozen,
            SigItem::Newtype(newtype) if matches!(newtype.payload, Type::Sum { .. })
        ));
    }

    #[test]
    fn one_context_can_drive_mixed_recursive_member_modifications() {
        let r = replay_src(
            r#"sig app v(2);
v(1) {
  with {
    mod api {
      rec {
        pub type A = B;
        pub type { B : A { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
v(2) {
  with {
    mod api {
      rec {
        pub type A = . | B;
        pub type { B : . | A { pub mk; pub get; }; };
      }
    }
  };
  breaking { modify { api.A; } };
  nonbreaking { modify { api.B; } }
}
"#,
        );
        let a = live_frozen_item(&r, "api", "A");
        let b = live_frozen_item(&r, "api", "B");
        assert!(matches!(
            &a.frozen,
            SigItem::TypeAlias(alias) if matches!(alias.body, Type::Sum { .. })
        ));
        assert!(matches!(
            &b.frozen,
            SigItem::Newtype(newtype) if matches!(newtype.payload, Type::Sum { .. })
        ));
        assert!(Arc::ptr_eq(
            a.recursive_context.as_ref().expect("A context"),
            b.recursive_context.as_ref().expect("B context"),
        ));
    }

    #[test]
    fn removed_recursive_group_can_be_readded_from_a_new_context_epoch() {
        let r = replay_src(
            r#"sig app v(3);
v(1) {
  with {
    mod api {
      rec {
        pub type A = B;
        pub type { B : A { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
v(2) {
  breaking { remove { api.A; api.B; } }
}
v(3) {
  with {
    mod api {
      rec {
        pub type A = . | B;
        pub type { B : A { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
"#,
        );
        assert!(r.removed.is_empty(), "same-side re-add clears retirements");
        assert!(matches!(
            &live_frozen_item(&r, "api", "A").frozen,
            SigItem::TypeAlias(alias) if matches!(alias.body, Type::Sum { .. })
        ));
    }

    #[test]
    fn split_group_requires_a_replacement_context_when_a_member_is_readded() {
        let file = parse_signature_file(
            r#"sig app v(2);
v(1) {
  with {
    mod api {
      rec {
        pub type A = B;
        pub type { B : . | A { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}
v(2) {
  breaking { remove { api.B; } };
  nonbreaking { add { mod api {
    pub type { B : . { pub mk; pub get; }; };
  } } }
}
"#,
            None,
        )
        .expect("parse missing replacement context");
        let error = replay(&file).expect_err("a stale filtered recursive context must be rejected");
        assert!(
            error
                .diag()
                .1
                .contains("one recursive data declaration uses a `rec` modifier, not a group"),
            "got: {error:?}"
        );
    }

    #[test]
    fn role_bearing_parameterized_host_type_is_rejected() {
        let file = parse_signature_file(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type Count[A] role(i32);
      }
    }
  }
}
",
            None,
        )
        .expect("parse");

        let err = replay(&file).expect_err("invalid frozen host type must be rejected");
        assert_eq!(err.exit_code(), crate::exit_code::ExitCode::Type);
        assert!(
            err.diag()
                .1
                .contains("role-bearing host type `Count` cannot have type parameters"),
            "got: {}",
            err.diag().1
        );
        assert_eq!(err.diagnostic().secondary().len(), 1);
        assert!(err.diagnostic().help().is_some());
    }

    #[test]
    fn higher_kinded_host_type_parameter_is_rejected() {
        let file = parse_signature_file(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type Wrapper[*F];
      }
    }
  }
}
",
            None,
        )
        .expect("parse");

        let err = replay(&file).expect_err("invalid frozen host type must be rejected");
        assert_eq!(err.exit_code(), crate::exit_code::ExitCode::Type);
        assert!(
            err.diag()
                .1
                .contains("host type `Wrapper` cannot have higher-kinded type parameter `F`"),
            "got: {}",
            err.diag().1
        );
        assert!(err.diagnostic().help().is_some());
    }

    #[test]
    fn add_makes_present() {
        let r = replay_src(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type H role(i32);
        pub fn f(x: H) -> H;
      }
    }
  }
}
",
        );
        assert!(r.current.items.contains_key(&name("api", "H")));
        assert!(r.current.items.contains_key(&name("api", "f")));
        assert_eq!(r.current.items[&name("api", "H")].side, ContractSide::Env);
        assert_eq!(
            r.current.items[&name("api", "f")].side,
            ContractSide::Export
        );
        assert!(r.removed.is_empty());
    }

    #[test]
    fn accepted_private_signature_newtype_replays_as_opaque() {
        let replayed = replay_src(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        type { Id : . { mk; get; }; };
      }
    }
  }
}
",
        );
        let ContractKind::Newtype { surface, .. } =
            &replayed.current.items[&name("api", "Id")].kind
        else {
            panic!("Id should be a newtype");
        };
        assert_eq!(surface, &PublicNewtypeSurface::Opaque);
    }

    #[test]
    fn signature_newtypes_imply_public_outer_visibility_for_member_projection() {
        let replayed = replay_src(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        type { Opaque : . { mk; get; }; };
        type { Make : . { pub mk; get; }; };
        type { Read : . { mk; pub get; }; };
        type { Both : . { pub mk; pub get; }; };
      }
    }
  }
}
",
        );
        let surface = |name: &str| {
            let ContractKind::Newtype { surface, .. } =
                &replayed.current.items[&self::name("api", name)].kind
            else {
                panic!("{name} should be a newtype");
            };
            surface
        };

        assert_eq!(surface("Opaque"), &PublicNewtypeSurface::Opaque);
        assert!(matches!(
            surface("Make"),
            PublicNewtypeSurface::Constructor { name, .. } if name == "mk"
        ));
        assert!(matches!(
            surface("Read"),
            PublicNewtypeSurface::Projector { name, .. } if name == "get"
        ));
        assert!(matches!(
            surface("Both"),
            PublicNewtypeSurface::ConstructorAndProjector {
                constructor,
                projector,
                ..
            } if constructor == "mk" && projector == "get"
        ));
    }

    #[test]
    fn exported_function_purity_survives_replay() {
        let replayed = replay_src(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        pub pure fn f() -> .;
      }
    }
  }
}
",
        );
        let ContractKind::Fn { pure, .. } = &replayed.current.items[&name("api", "f")].kind else {
            panic!("f should be a function");
        };
        assert!(*pure);
    }

    #[test]
    fn modify_reshapes() {
        // v(1) adds `f(x: H) -> H`; v(2) modifies it to `f() -> H`. The
        // current interface carries the modified shape.
        let r = replay_src(
            "sig app v(2);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type H role(i32);
        pub fn f(x: H) -> H;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      mod api {
        pub fn f() -> H;
      }
    }
  }
}
",
        );
        // Replay the v(1)-only history to get the original shape, then
        // confirm the v(2) modify reshaped it.
        let original = replay_src(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type H role(i32);
        pub fn f(x: H) -> H;
      }
    }
  }
}
",
        );
        let ContractKind::Fn {
            signature: orig, ..
        } = &original.current.items[&name("api", "f")].kind
        else {
            panic!("original f should be a fn");
        };
        let ContractKind::Fn {
            signature: modified,
            ..
        } = &r.current.items[&name("api", "f")].kind
        else {
            panic!("modified f should be a fn");
        };
        assert_ne!(orig, modified, "the v(2) modify must reshape f");
        // The original takes a value param (`@.../H`); the modified takes
        // none (a unit domain).
        assert!(
            orig.contains("/H)->"),
            "original f domain should be H: {orig}"
        );
        assert!(
            modified.contains("(.)->"),
            "modified f domain should be unit: {modified}"
        );
    }

    #[test]
    fn remove_drops_and_recovers_side() {
        // v(1) adds host type H + host fn open; v(2) removes open.
        let r = replay_src(
            "sig app v(2);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type H role(i32);
        host fn open() -> H;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {

        api.open;

    }
  }
}
",
        );
        assert!(!r.current.items.contains_key(&name("api", "open")));
        assert!(r.current.items.contains_key(&name("api", "H")));
        // The removed item's side (host/env) and frozen declaration are
        // recovered by replay to its v(1) origin.
        assert_eq!(r.removed.len(), 1);
        let removed = &r.removed[0];
        assert_eq!(removed.entry.name, name("api", "open"));
        assert_eq!(removed.entry.side, ContractSide::Env);
        assert!(matches!(removed.frozen, SigItem::HostFn(_)));
        assert_eq!(removed.removed_at_version, 2);
    }

    #[test]
    fn add_then_remove_nets_out() {
        // v(1) adds `g`; v(2) removes it. `g` is absent from the current
        // interface; it is recorded as removed (recoverable) since it
        // was present at v(1)'s sealed boundary.
        let r = replay_src(
            "sig app v(2);
v(1) {
  nonbreaking {
    add {
      mod api {
        pub fn g() -> .;
      }
    }
  }
}
v(2) {
  breaking {
    remove {

        api.g;

    }
  }
}
",
        );
        assert!(!r.current.items.contains_key(&name("api", "g")));
        assert_eq!(r.removed.len(), 1);
        assert_eq!(r.removed[0].entry.name, name("api", "g"));
    }

    #[test]
    fn add_then_remove_then_readd_is_live_not_removed() {
        // v(1) adds `g`; v(2) removes it; v(3) re-adds it. It is live in
        // the current interface and NOT in the removed set (re-add
        // clears the pending removal).
        let r = replay_src(
            "sig app v(3);
v(1) {
  nonbreaking {
    add {
      mod api {
        pub fn g() -> .;
      }
    }
  }
}
v(2) {
  breaking {
    remove {

        api.g;

    }
  }
}
v(3) {
  nonbreaking {
    add {
      mod api {
        pub fn g() -> .;
      }
    }
  }
}
",
        );
        assert!(r.current.items.contains_key(&name("api", "g")));
        assert!(r.removed.iter().all(|x| x.entry.name != name("api", "g")));
    }

    #[test]
    fn modify_then_remove_collapses_to_one_removal() {
        // v(1) adds `f(x: H) -> H`; v(2) modifies it; v(3) removes it.
        // The current interface drops `f`; the removed set has exactly
        // one `f` carrying its LAST frozen (modified) shape.
        let r = replay_src(
            "sig app v(3);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type H role(i32);
        pub fn f(x: H) -> H;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      mod api {
        pub fn f() -> H;
      }
    }
  }
}
v(3) {
  breaking {
    remove {

        api.f;

    }
  }
}
",
        );
        assert!(!r.current.items.contains_key(&name("api", "f")));
        let fs: Vec<_> = r
            .removed
            .iter()
            .filter(|x| x.entry.name == name("api", "f"))
            .collect();
        assert_eq!(
            fs.len(),
            1,
            "modify-then-remove must collapse to one removal"
        );
        let SigItem::ExportFn(export) = &fs[0].frozen else {
            panic!("frozen f should be an export fn");
        };
        // The frozen shape is the LAST one (the v(2) modify: no params).
        assert!(
            export.function.params.is_empty(),
            "frozen f should be the modified (param-less) shape"
        );
        assert_eq!(fs[0].removed_at_version, 3);
    }

    #[test]
    fn module_removal_as_empty_drops_all_items() {
        // Removing every item of a module leaves it contributing nothing.
        let r = replay_src(
            "sig app v(2);
v(1) {
  nonbreaking {
    add {
      mod api {
        pub fn a() -> .;
        pub fn b() -> .;
      }
    }
  }
}
v(2) {
  breaking {
    remove {

        api.a;
        api.b;

    }
  }
}
",
        );
        assert!(r.current.items.is_empty());
        assert_eq!(r.removed.len(), 2);
    }

    #[test]
    fn remove_never_added_is_error() {
        let file = parse_signature_file(
            "sig app v(1);
v(1) {
  breaking {
    remove {

        api.ghost;

    }
  }
}
",
            None,
        )
        .expect("parse");
        let err = replay(&file).expect_err("removing a never-added name must error");
        assert!(
            err.diag().1.contains("never added"),
            "got: {}",
            err.diag().1
        );
    }

    #[test]
    fn modify_never_added_is_error() {
        // A `modify` of a name that was never added (nor modified) in an
        // earlier version is a history incoherence — mirroring the
        // `remove` gate, replay rejects it rather than silently inserting.
        let file = parse_signature_file(
            "sig app v(1);
v(1) {
  breaking {
    modify {
      mod api {
        pub fn ghost() -> .;
      }
    }
  }
}
",
            None,
        )
        .expect("parse");
        let err = replay(&file).expect_err("modifying a never-added name must error");
        assert!(
            err.diag().1.contains("never added"),
            "got: {}",
            err.diag().1
        );
    }

    #[test]
    fn modify_removed_inline_declaration_is_error() {
        let source = include_str!(
            "../../../test-data/goldens/40_build_error/sig_modify_removed/workdir/app.sig.kio"
        );
        let file = parse_signature_file(source, None).expect("parse removed declaration history");
        let error = replay(&file).expect_err("a frozen declaration is not a live Modify target");
        assert_eq!(
            error.diag().1,
            "signature changelog modifies `api.serve`, but that declaration was removed"
        );
        assert_eq!(
            error.diagnostic().span.start as usize,
            source.rfind("fn serve").expect("Modify declaration")
        );
        assert_eq!(
            error.diagnostic().help(),
            Some("use `add` to reintroduce a removed declaration")
        );
    }

    #[test]
    fn modify_removed_recursive_reference_is_error() {
        let source = include_str!(
            "../../../test-data/goldens/40_build_error/sig_modify_removed_recursive/workdir/app.sig.kio"
        );
        let file = parse_signature_file(source, None).expect("parse complete recursive contexts");
        let error = replay(&file).expect_err("a recursive FQN must select a live declaration");
        assert_eq!(
            error.diag().1,
            "signature changelog modifies `api.A`, but that declaration was removed"
        );
        assert_eq!(
            error.diagnostic().span.start as usize,
            source
                .rfind("pub type A")
                .expect("version-local declaration")
        );
        assert_eq!(
            error.diagnostic().help(),
            Some("use `add` to reintroduce a removed declaration")
        );
    }

    #[test]
    fn readded_declaration_can_be_modified_and_retired_from_its_new_origin() {
        let r = replay_src(
            r#"sig app v(5);
v(1) { nonbreaking { add { mod api { pub fn serve() -> .; } } } }
v(2) { breaking { remove { api.serve; } } }
v(3) { nonbreaking { add { mod api { pub fn serve() -> .; } } } }
v(4) { breaking { modify { mod api { pub fn serve(value: .) -> .; } } } }
v(5) { breaking { remove { api.serve; } } }
"#,
        );
        assert!(r.current.items.is_empty());
        assert_eq!(r.removed.len(), 1);
        let retired = &r.removed[0];
        assert_eq!(retired.entry.name, name("api", "serve"));
        assert_eq!(retired.entry.side, ContractSide::Export);
        assert_eq!(retired.removed_at_version, 5);
        let SigItem::ExportFn(export) = &retired.frozen else {
            panic!("the re-added export keeps its own declaration kind");
        };
        assert_eq!(export.function.params.len(), 1);
    }

    #[test]
    fn side_flip_supersedes_host_item_for_reemit() {
        // v(1) declares `host fn f`; v(2) modifies it to `pub fn f` — a
        // host→export side flip. The old host `f` is abandoned: the
        // host's `impl` still declares it, so it must remain recoverable
        // as an env-side removal for the deprecation re-emit even though
        // `f` is live again as an export.
        let r = replay_src(
            "sig app v(2);
v(1) {
  nonbreaking {
    add {
      mod api {
        host fn f() -> .;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      mod api {
        pub fn f() -> .;
      }
    }
  }
}
",
        );
        // `f` is live as an export.
        assert_eq!(
            r.current.items[&name("api", "f")].side,
            ContractSide::Export
        );
        // And the superseded host `f` is recoverable on the env side.
        let host_f = r
            .removed
            .iter()
            .find(|x| x.entry.name == name("api", "f") && x.entry.side == ContractSide::Env)
            .expect("superseded host f recoverable for deprecation re-emit");
        assert!(matches!(host_f.frozen, SigItem::HostFn(_)));
        assert_eq!(host_f.removed_at_version, 2);
    }

    #[test]
    fn side_flip_then_remove_keeps_host_item_recoverable() {
        // As above, but v(3) also removes the export `f`. The superseded
        // host `f` must still be recoverable on the env side (the explicit
        // export removal must not shadow it).
        let r = replay_src(
            "sig app v(3);
v(1) {
  nonbreaking {
    add {
      mod api {
        host fn f() -> .;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      mod api {
        pub fn f() -> .;
      }
    }
  }
}
v(3) {
  breaking {
    remove {

        api.f;

    }
  }
}
",
        );
        assert!(!r.current.items.contains_key(&name("api", "f")));
        let host_f = r
            .removed
            .iter()
            .find(|x| x.entry.name == name("api", "f") && x.entry.side == ContractSide::Env)
            .expect("superseded host f still recoverable after export removal");
        assert!(matches!(host_f.frozen, SigItem::HostFn(_)));
    }

    #[test]
    fn side_flip_back_to_host_clears_the_obsolete_host_retirement() {
        let replayed = replay_src(
            "sig app v(3);
v(1) {
  nonbreaking {
    add {
      mod api {
        host fn f() -> .;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      mod api {
        pub fn f() -> .;
      }
    }
  }
}
v(3) {
  breaking {
    modify {
      mod api {
        host fn f(x: .) -> .;
      }
    }
  }
}
",
        );

        assert_eq!(
            replayed.current.items[&name("api", "f")].side,
            ContractSide::Env
        );
        assert!(replayed.removed.iter().all(|item| {
            item.entry.name != name("api", "f") || item.entry.side != ContractSide::Env
        }));
        assert!(replayed.removed.iter().all(|item| {
            item.entry.name != name("api", "f") || item.entry.side != ContractSide::Export
        }));
    }

    #[test]
    fn export_to_host_flip_does_not_implicitly_retire_the_export() {
        let replayed = replay_src(
            "sig app v(2);
v(1) {
  nonbreaking {
    add {
      mod api {
        pub fn f() -> .;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      mod api {
        host fn f() -> .;
      }
    }
  }
}
",
        );

        assert_eq!(
            replayed.current.items[&name("api", "f")].side,
            ContractSide::Env
        );
        assert!(
            replayed
                .removed
                .iter()
                .all(|item| item.entry.name != name("api", "f"))
        );
    }

    #[test]
    fn explicit_host_removal_survives_an_export_readd() {
        let replayed = replay_src(
            "sig app v(3);
v(1) {
  nonbreaking {
    add {
      mod api {
        host fn f() -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {

        api.f;

    }
  }
}
v(3) {
  nonbreaking {
    add {
      mod api {
        pub fn f() -> .;
      }
    }
  }
}
",
        );

        assert_eq!(
            replayed.current.items[&name("api", "f")].side,
            ContractSide::Export
        );
        let retired_host = replayed
            .removed
            .iter()
            .find(|item| {
                item.entry.name == name("api", "f") && item.entry.side == ContractSide::Env
            })
            .expect("explicit host removal remains retired");
        assert_eq!(retired_host.removed_at_version, 2);
    }

    #[test]
    fn repeated_side_flip_retires_the_latest_host_incarnation() {
        let replayed = replay_src(
            "sig app v(4);
v(1) {
  nonbreaking {
    add {
      mod api {
        host fn f() -> .;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      mod api {
        pub fn f() -> .;
      }
    }
  }
}
v(3) {
  breaking {
    modify {
      mod api {
        host fn f(x: .) -> .;
      }
    }
  }
}
v(4) {
  breaking {
    modify {
      mod api {
        pub fn f() -> .;
      }
    }
  }
}
",
        );

        let host = replayed
            .removed
            .iter()
            .find(|item| {
                item.entry.name == name("api", "f") && item.entry.side == ContractSide::Env
            })
            .expect("latest host incarnation must be retired");
        let SigItem::HostFn(declaration) = &host.frozen else {
            panic!("retired env item should be a host fn");
        };
        assert_eq!(declaration.params.len(), 1);
        assert_eq!(host.removed_at_version, 4);
    }

    #[test]
    fn remove_readd_remove_retires_the_readded_incarnation() {
        let replayed = replay_src(
            "sig app v(4);
v(1) {
  nonbreaking {
    add {
      mod api {
        host fn f() -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {

        api.f;

    }
  }
}
v(3) {
  nonbreaking {
    add {
      mod api {
        host fn f(x: .) -> .;
      }
    }
  }
}
v(4) {
  nonbreaking {
    remove {

        api.f;

    }
  }
}
",
        );

        let retired: Vec<_> = replayed
            .removed
            .iter()
            .filter(|item| item.entry.name == name("api", "f"))
            .collect();
        assert_eq!(retired.len(), 1);
        let SigItem::HostFn(declaration) = &retired[0].frozen else {
            panic!("retired item should be a host fn");
        };
        assert_eq!(declaration.params.len(), 1);
        assert_eq!(retired[0].removed_at_version, 4);
    }

    #[test]
    fn host_root_freezes_types_across_one_versions_partition_and_item_order() {
        let replayed = replay_src(
            "sig app v(1);
v(1) {
  breaking {
    add {
      mod api {
        host fn consume(x: Wrapped) -> .;
      }
    }
  };
  nonbreaking {
    add {
      mod api {
        type Wrapped = Raw;
        host type Raw;
      }
    }
  }
}
",
        );

        let closure = live_host_closure(&replayed, "api", "consume");
        assert!(matches!(
            closure.declarations[&name("api", "Wrapped")].declaration,
            FrozenTypeItem::TypeAlias(_)
        ));
        assert!(matches!(
            closure.declarations[&name("api", "Raw")].declaration,
            FrozenTypeItem::HostType(_)
        ));
    }

    #[test]
    fn each_host_root_gets_only_its_own_reachable_types() {
        let replayed = replay_src(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type Left;
        host type Right;
        host fn take_left(x: Left) -> .;
        host fn take_right(x: Right) -> .;
      }
    }
  }
}
",
        );

        let left = live_host_closure(&replayed, "api", "take_left");
        let right = live_host_closure(&replayed, "api", "take_right");
        assert_eq!(
            left.declarations.keys().cloned().collect::<Vec<_>>(),
            vec![name("api", "Left")]
        );
        assert_eq!(
            right.declarations.keys().cloned().collect::<Vec<_>>(),
            vec![name("api", "Right")]
        );
    }

    #[test]
    fn sealed_add_then_remove_keeps_the_roots_type_origin() {
        let replayed = replay_src(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type Raw;
        host fn f(x: Raw) -> .;
      }
    };
    remove {

        api.Raw;
        api.f;

    }
  }
}
",
        );

        let removed = replayed
            .removed
            .iter()
            .find(|item| item.entry.name == name("api", "f"))
            .expect("f removed at the sealed boundary");
        assert!(
            removed
                .frozen_type_closure
                .as_ref()
                .expect("frozen host root")
                .declarations
                .contains_key(&name("api", "Raw"))
        );
    }

    #[test]
    fn frozen_closure_keeps_each_declarations_own_imports() {
        let replayed = replay_src(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod raw {
        host type Raw;
      };
      mod aliases {
        import raw {type Raw};
        type Wrapped = Raw;
      };
      mod api {
        import aliases {type Wrapped};
        host fn consume(x: Wrapped) -> .;
      }
    }
  }
}
",
        );

        let root = live_frozen_item(&replayed, "api", "consume");
        assert_eq!(
            selective_import_path(&root.imports, "Wrapped"),
            Some(vec!["aliases".to_owned(), "Wrapped".to_owned()])
        );
        let closure = root
            .frozen_type_closure
            .as_ref()
            .expect("frozen host closure");
        let wrapped = &closure.declarations[&name("aliases", "Wrapped")];
        assert_eq!(
            selective_import_path(&wrapped.imports, "Raw"),
            Some(vec!["raw".to_owned(), "Raw".to_owned()])
        );
        assert!(closure.declarations.contains_key(&name("raw", "Raw")));
    }

    #[test]
    fn nested_callback_and_return_types_stay_in_one_root_closure() {
        let replayed = replay_src(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type Input;
        host type Output;
        host fn register(cb: (Input) -> Output) -> (Output) -> Input;
      }
    }
  }
}
",
        );

        let closure = live_host_closure(&replayed, "api", "register");
        assert_eq!(
            closure.declarations.keys().cloned().collect::<Vec<_>>(),
            vec![name("api", "Input"), name("api", "Output")]
        );
    }

    #[test]
    fn removed_root_keeps_its_historical_alias_generation() {
        let replayed = replay_src(
            "sig app v(2);
v(1) {
  nonbreaking {
    add {
      mod api {
        host type Raw;
        type Wrapped = Raw;
        host fn consume(x: Wrapped) -> .;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      mod api {
        type { Wrapped : Raw { pub mk; pub get; }; };
      }
    };
    remove {

        api.consume;

    }
  }
}
",
        );

        let removed = replayed
            .removed
            .iter()
            .find(|item| item.entry.name == name("api", "consume"))
            .expect("consume removed");
        let closure = removed
            .frozen_type_closure
            .as_ref()
            .expect("frozen host root");
        assert!(matches!(
            closure.declarations[&name("api", "Wrapped")].declaration,
            FrozenTypeItem::TypeAlias(_)
        ));
        assert!(matches!(
            replayed
                .live_frozen
                .iter()
                .find(|item| item.entry.name == name("api", "Wrapped"))
                .expect("live Wrapped")
                .frozen,
            SigItem::Newtype(_)
        ));
    }

    #[test]
    fn missing_historical_type_is_rejected_before_a_future_declaration_can_rebind_it() {
        let file = parse_signature_file(
            "sig app v(2);
v(1) {
  nonbreaking {
    add {
      mod api {
        host fn f(x: Later) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    add {
      mod api {
        host type Later;
      }
    }
  }
}
",
            None,
        )
        .expect("parse signature history");
        let error = replay(&file).expect_err("the first version is independently invalid");
        assert!(error.diag().1.contains("unbound name"), "got: {error:?}");
    }

    #[test]
    fn alias_only_cycle_is_rejected_when_a_host_root_reaches_it() {
        let file = parse_signature_file(
            "sig app v(1);
v(1) {
  nonbreaking {
    add {
      mod api {
        type A = B;
        type B = A;
        host fn f(x: A) -> .;
      }
    }
  }
}
",
            None,
        )
        .expect("parse");
        let error = replay(&file).expect_err("alias-only cycle must be rejected");
        assert!(
            error.diag().1.contains("type-alias cycle"),
            "got: {}",
            error.diag().1
        );
    }

    #[test]
    fn newtype_is_a_nominal_boundary_for_alias_recursion() {
        let replayed = replay_src(
            "sig app v(1);
v(1) {
  with {
    mod api {
      rec {
        pub type A = N;
        pub type { N : (A & Payload) { pub mk; pub get; }; };
      }
    }
  };
  nonbreaking {
    add {
      mod api {
        host type Payload;
        host fn f(x: A) -> .;
      };
      api.A;
      api.N;
    }
  }
}
",
        );
        let closure = live_host_closure(&replayed, "api", "f");
        assert!(matches!(
            closure.declarations[&name("api", "A")].declaration,
            FrozenTypeItem::TypeAlias(_)
        ));
        assert!(matches!(
            closure.declarations[&name("api", "N")].declaration,
            FrozenTypeItem::Newtype(_)
        ));
        assert!(matches!(
            closure.declarations[&name("api", "Payload")].declaration,
            FrozenTypeItem::HostType(_)
        ));
    }
}
