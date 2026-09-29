//! Inline intra-doc reference scanner and resolver.
//!
//! Kiodoc supports `` [`name`] `` intra-doc links inside the prose of
//! both `.md` files and `///` doc-comments. When `kio doc check` encounters
//! such a link, it validates that `name` is in scope — against the
//! surrounding module's name resolution rules for `///` doc-comments, and
//! against the package's exported names for `.md` files.
//!
//! See [`specs/kiodoc.md`](../../../../specs/kiodoc.md) §
//! "Intra-doc references" for the full contract.
//!
//! ## Syntax
//!
//! The canonical spelling is `` [`name`] `` — backticks inside the
//! brackets. The backticks are part of the Kiodoc syntax: plain
//! `[name]` without backticks is treated as regular Markdown link text
//! and is not validated.
//!
//! ## Override semantics
//!
//! A standard Markdown reference-link definition (`[name]: url`) in prose
//! overrides auto-resolution for that key. Definitions inside fenced
//! examples are literal example text and do not participate.
//!
//! ## Name forms
//!
//! Two forms are accepted, mirroring imports:
//!
//! - **Simple name** — a single identifier (bare name, label, imported
//!   alias). Operators are spelled as their token sequence.
//! - **Qualified path** — a module-qualified item (`pkg/mod.name`),
//!   the module segments `/`-separated and the item reached with `.`,
//!   or an alias-rooted member access (`m.name`).
//!
//! ## Resolution scope
//!
//! - Inside a `///` doc-comment: the module's top-level scope (all
//!   declared items, all items brought in by `import`, including private
//!   ones) and the declared item's own name.
//! - Inside a `.md` file: the package's exported names plus any
//!   qualified path the package's module structure can reach.

use crate::span::Span;

/// One intra-doc reference found in a prose text block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntraRef {
    /// The `name` payload inside `` [`name`] ``.
    pub name: String,
    /// Byte offset of the `` [`name`] `` pattern inside the prose
    /// text it was found in (not the overall source file).
    pub offset: u32,
}

/// An unresolved intra-doc reference — one that should produce a
/// diagnostic.
#[derive(Debug, Clone)]
pub struct UnresolvedRef {
    pub name: String,
    /// Byte offset of the whole `` [`name`] `` pattern within the
    /// prose text block it was found in.
    pub offset: u32,
}

/// Scope context for resolving intra-doc references inside a `///`
/// doc-comment on a module-level item.
///
/// The resolver checks this against `[`name`]` occurrences found by
/// [`scan_refs`]. Resolution succeeds when `name` appears in any of
/// the sets here.
#[derive(Debug, Default)]
pub struct ModuleScope {
    /// Top-level item names declared in the module (all items, whether
    /// `pub` or not — doc-comments see private items, same as `{@}`
    /// snippets).
    pub top_level: Vec<String>,
    /// Names brought in by `import` statements in the module's import
    /// block. This includes selectively-imported names and the alias
    /// introduced by `import … as alias;`.
    pub imported: Vec<String>,
    /// Qualified-import aliases from `import path/to/mod as m;`. Listed
    /// separately so the docref resolver can accept `m.name` paths.
    pub qualified_aliases: Vec<String>,
    /// Whether `import __intrinsics__;` is in scope — lets `` [`__left__`] ``
    /// etc. resolve.
    pub intrinsics_in_scope: bool,
    /// Non-value declaration selectors: ordinary type names and braced
    /// label-forwarding names. The `` [`@type term`] `` directive rejects
    /// these selectors; constructor and projector value paths are separate.
    pub type_level: Vec<(String, TypeLevelKind)>,
}

/// The declaration kind behind a selector that has no bound-value type.
/// Used to phrase the kind-aware `@type` rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeLevelKind {
    /// A `type Name = …;` declaration.
    TypeAlias,
    /// A `labels Name = { … };` declaration.
    Labels,
    /// A `newtype Name = …;` declaration.
    Newtype,
    /// A `host type Name;` declaration.
    HostType,
    /// A nonminting `type {label} = {target};` declaration.
    LabelForward,
}

impl TypeLevelKind {
    /// A human-readable description of the kind, for diagnostics —
    /// e.g. "a type alias", "a `labels` declaration".
    pub fn describe(self) -> &'static str {
        match self {
            TypeLevelKind::TypeAlias => "a type alias",
            TypeLevelKind::Labels => "a `labels` declaration",
            TypeLevelKind::Newtype => "a `newtype`",
            TypeLevelKind::HostType => "a `host type`",
            TypeLevelKind::LabelForward => "a label declaration",
        }
    }
}

/// Resolution scope for `.md` files. The `.md` file is associated with
/// a package; the scope is the package boundary.
#[derive(Debug, Default)]
pub struct PackageScope {
    /// Declaration selectors forming the package boundary, gathered from
    /// bridged modules. All are valid in-scope names for `.md` prose.
    pub exported_names: Vec<String>,
    /// Module path segments that the package declares (e.g. for a
    /// module `pkg.util`, "util" is a valid segment after "pkg").
    pub module_segments: Vec<String>,
    /// Exported declaration selectors without a bound-value type, paired
    /// with the kind reported by the `` [`@type term`] `` directive.
    pub type_level: Vec<(String, TypeLevelKind)>,
}

/// A resolved or overridden reference — the outcome of
/// [`check_refs`] for one `` [`name`] `` form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefOutcome {
    /// The name was found in the active scope.
    Resolved,
    /// A Markdown reference-link definition overrode the auto-resolution
    /// for this key — no further checking is performed.
    Overridden,
    /// The name was not found in any scope.
    Unresolved,
}

// ---- Scanner ----------------------------------------------------------------

/// Scan `text` for `` [`name`] `` intra-doc reference patterns and
/// return the list of references found. Text blocks should be
/// prose (not fence bodies — fence bodies are validated separately
/// via the harness/snippet machinery).
///
/// A pattern is `` [`name`] `` where `name` matches
/// `[^`\]]+` (anything except backtick and `]`). Leading and
/// trailing whitespace inside the backticks is significant — names
/// may be operators with spaces like `_ + _`.
///
/// An override definition (`[name]: ...`) found in the same text
/// is collected into `overrides`; callers pass that to
/// [`check_refs`] to suppress auto-resolution for those keys.
pub fn scan_refs(text: &str) -> Vec<IntraRef> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;

    while i < len {
        if let Some(end) = super::parse::inline_literal_end(text, i) {
            i = end;
            continue;
        }
        // Look for `[`
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        // We found `[`. Now check if it's followed by a backtick.
        let bracket_start = i;
        i += 1;
        if i >= len || bytes[i] != b'`' {
            // Not `[`…` — skip; may be plain `[text]` or `[text](url)`.
            continue;
        }
        i += 1; // consume the backtick
        // Scan to the closing backtick.
        let name_start = i;
        while i < len && bytes[i] != b'`' && bytes[i] != b']' && bytes[i] != b'\n' {
            i += 1;
        }
        if i >= len || bytes[i] != b'`' {
            // Didn't find a closing backtick (hit ] or newline first) — not a ref.
            continue;
        }
        let name_end = i;
        i += 1; // consume the closing backtick
        // Require a `]` immediately after the closing backtick.
        if i >= len || bytes[i] != b']' {
            continue;
        }
        i += 1; // consume the `]`

        let name = &text[name_start..name_end];
        if name.is_empty() {
            continue;
        }
        // Skip if the name starts with `@` — those are directive patterns
        // (`` [`@KEYWORD(ARG)`] ``) handled by the directive scanner, not
        // plain intra-doc refs.
        if name.starts_with('@') {
            continue;
        }
        // Skip if the next non-whitespace token after `]` is `(`, `[`, or `:` —
        // those are standard Markdown inline link, reference link *use*,
        // and reference-link *definition* patterns. We only want the
        // bare `` [`name`] `` form.
        let peek = text[i..].trim_start();
        if peek.starts_with('(') || peek.starts_with('[') {
            continue;
        }

        out.push(IntraRef {
            name: name.to_owned(),
            offset: bracket_start as u32,
        });
    }

    out
}

/// Collect all reference-link definition keys from `text`. A reference-
/// link definition is a line of the form `[key]: ...` where `key`
/// does not start with a backtick (those are the intra-doc refs). This
/// lets authors override auto-resolution by adding `[name]: url` to
/// their document.
///
/// Returns the set of override keys found.
pub fn collect_ref_overrides(text: &str) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let prose = super::parse::blank_fences(text);
    for line in prose.lines() {
        let trimmed = line.trim();
        // Reference-link definition: `[key]: <target>` at the
        // beginning of a (optionally indented) line, with at most
        // 3 spaces of indent.
        if !trimmed.starts_with('[') {
            continue;
        }
        // The key must not start with a backtick (that would be a
        // misparse of a `` [`name`] `` inline ref at the start of
        // a line — those are not definitions).
        let rest = &trimmed[1..];
        if rest.starts_with('`') {
            continue;
        }
        // Find the closing `]` followed by `:`.
        if let Some(close_bracket) = rest.find(']') {
            let after = rest[close_bracket + 1..].trim_start();
            if after.starts_with(':') {
                let key = rest[..close_bracket].trim().to_owned();
                if !key.is_empty() {
                    set.insert(key);
                }
            }
        }
    }
    set
}

// ---- Resolution against a module scope -------------------------------------

/// Check whether `name` is in scope in a Kio module, given the
/// pre-collected [`ModuleScope`]. Returns the resolution outcome.
///
/// This is the resolution rule for `[`name`]` inside a `///`
/// doc-comment: names brought in by `import`, top-level declarations,
/// qualified-import aliases, and (when `import __intrinsics__` is
/// present) the intrinsic names.
pub fn resolve_in_module(name: &str, scope: &ModuleScope) -> RefOutcome {
    if scope.top_level.iter().any(|n| n == name) || scope.imported.iter().any(|n| n == name) {
        return RefOutcome::Resolved;
    }

    // Qualified path: `prefix<sep>rest`. An alias-rooted member
    // access (`m.name`) has a single-segment alias prefix; a
    // module-qualified item (`pkg/mod.name`) leads with a module
    // segment. The leading segment ends at the first `/` or `.`.
    if let Some(sep) = name.find(['/', '.']) {
        let prefix = &name[..sep];
        if scope.qualified_aliases.iter().any(|a| a == prefix) {
            return RefOutcome::Resolved;
        }
        // Not a known alias prefix → fall through to plain name check.
        // A fully-qualified path with a package prefix (e.g. `pkg/mod.name`)
        // can't be validated without the package graph; accept it optimistically
        // if it looks like a valid qualified path (no spaces, valid ident segments).
        if looks_like_qualified_path(name) {
            return RefOutcome::Resolved;
        }
        return RefOutcome::Unresolved;
    }

    if scope.intrinsics_in_scope && is_intrinsic(name) {
        return RefOutcome::Resolved;
    }

    RefOutcome::Unresolved
}

/// Check whether `name` is in scope in a package, given the
/// pre-collected [`PackageScope`]. Returns the resolution outcome.
///
/// This is the resolution rule for `[`name`]` inside a `.md`
/// file: exported names and module segments are valid references.
pub fn resolve_in_package(name: &str, scope: &PackageScope) -> RefOutcome {
    if scope.exported_names.iter().any(|n| n == name) {
        return RefOutcome::Resolved;
    }

    // Qualified path: accept if the leading segment is a known module segment.
    // The module-path part uses `/` separators and reaches its item with `.`, so
    // the leading segment ends at the first `/` or `.`.
    if let Some(sep) = name.find(['/', '.']) {
        let prefix = &name[..sep];
        if scope.module_segments.iter().any(|s| s == prefix) {
            return RefOutcome::Resolved;
        }
        // Multi-segment path from an unknown prefix: accept it
        // optimistically. The standalone `kio doc` walker cannot
        // resolve references outside the documented package.
        if looks_like_qualified_path(name) {
            return RefOutcome::Resolved;
        }
        return RefOutcome::Unresolved;
    }

    RefOutcome::Unresolved
}

// ---- Type-level classification ---------------------------------------------

/// The kind of `name` if it selects a declaration without a bound-value
/// type in this module scope. The `` [`@type term`] `` directive rejects it.
///
/// A qualified path is never type-level here — it cannot be resolved
/// to a concrete declaration without the full package graph, so the
/// `@type` checker treats it optimistically.
pub fn type_level_kind_in_module(name: &str, scope: &ModuleScope) -> Option<TypeLevelKind> {
    if name.contains('.') {
        return None;
    }
    scope
        .type_level
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, k)| *k)
}

/// The kind of `name` if it selects an exported declaration without a
/// bound-value type. Qualified paths retain the module form's treatment.
pub fn type_level_kind_in_package(name: &str, scope: &PackageScope) -> Option<TypeLevelKind> {
    if name.contains('.') {
        return None;
    }
    scope
        .type_level
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, k)| *k)
}

// ---- Convenience wrapper ----------------------------------------------------

/// One failed reference check, carrying the name and the source offset
/// within the prose text block.
#[derive(Debug, Clone)]
pub struct RefError {
    /// The name that could not be resolved.
    pub name: String,
    /// Byte offset of the `` [`name`] `` form within the prose text
    /// block it was found in.
    pub offset: u32,
}

/// Validate all `` [`name`] `` references in `text` against a
/// module scope, returning one error per unresolved reference.
///
/// `overrides` is the set of reference-link definition keys collected
/// from the same document — those keys are skipped.
///
/// Span is a base offset added to each error's `offset` so callers
/// can map errors back into the overall source file.
pub fn check_refs_in_module(
    text: &str,
    scope: &ModuleScope,
    overrides: &std::collections::HashSet<String>,
) -> Vec<RefError> {
    let refs = scan_refs(text);
    let mut errors = Vec::new();
    for r in refs {
        if overrides.contains(&r.name) {
            continue;
        }
        match resolve_in_module(&r.name, scope) {
            RefOutcome::Resolved | RefOutcome::Overridden => {}
            RefOutcome::Unresolved => errors.push(RefError {
                name: r.name,
                offset: r.offset,
            }),
        }
    }
    errors
}

/// Validate all `` [`name`] `` references in `text` against a package
/// scope, returning one error per unresolved reference.
pub fn check_refs_in_package(
    text: &str,
    scope: &PackageScope,
    overrides: &std::collections::HashSet<String>,
) -> Vec<RefError> {
    let refs = scan_refs(text);
    let mut errors = Vec::new();
    for r in refs {
        if overrides.contains(&r.name) {
            continue;
        }
        match resolve_in_package(&r.name, scope) {
            RefOutcome::Resolved | RefOutcome::Overridden => {}
            RefOutcome::Unresolved => errors.push(RefError {
                name: r.name,
                offset: r.offset,
            }),
        }
    }
    errors
}

// ---- Scope builders ---------------------------------------------------------

fn operator_token_name(op: &crate::ast::Op) -> Option<String> {
    let crate::ast::OpBody::Normal { pattern, .. } = &op.body;
    let tokens = pattern
        .iter()
        .filter_map(|part| match part {
            crate::ast::OpPart::Token { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    (!tokens.is_empty()).then_some(tokens)
}

/// Query aliases select a declaration without replacing its canonical identity.
pub(super) fn documented_entry_matches_reference(
    entry: &crate::doc_entry::DocEntry<'_>,
    name: &str,
) -> bool {
    entry.name() == name
        || matches!(entry, crate::doc_entry::DocEntry::Op(op, _)
            if operator_token_name(op).as_deref() == Some(name))
}

/// Build a [`ModuleScope`] from a parsed Kio `Surface` module. This is
/// the lightweight scope extraction used by the Kiodoc ref checker — it
/// does not require a full desugar or type-check pass.
pub fn module_scope_from_surface(module: &crate::ast::Module<crate::ast::Surface>) -> ModuleScope {
    module_scope_from_surface_impl(module)
}

/// Package-page names come only from exact public declarations and public roles.
pub fn module_scope_from_surface_for_boundary(
    module: &crate::ast::Module<crate::ast::Surface>,
) -> ModuleScope {
    use crate::doc_entry::DocEntry;
    let mut scope = ModuleScope::default();
    for entry in crate::doc_entry::exported_documented_items_module(module) {
        scope.top_level.push(entry.name().to_owned());
        if let DocEntry::Op(op, _) = &entry
            && let Some(name) = operator_token_name(op)
        {
            scope.top_level.push(name);
        }
        let kind = match &entry {
            DocEntry::TypeAlias(_, _) => Some(TypeLevelKind::TypeAlias),
            DocEntry::Newtype(_, _) | DocEntry::LabelNominal { .. } => Some(TypeLevelKind::Newtype),
            DocEntry::Labels(_, _, _) => Some(TypeLevelKind::Labels),
            DocEntry::LabelForward(_, _) => Some(TypeLevelKind::LabelForward),
            DocEntry::HostType(_) => Some(TypeLevelKind::HostType),
            DocEntry::Fn(_)
            | DocEntry::LiteralAlias(_)
            | DocEntry::Op(_, _)
            | DocEntry::VariadicOperator(_, _)
            | DocEntry::Elaborator(_)
            | DocEntry::HostFn(_) => None,
        };
        if let Some(kind) = kind {
            scope.type_level.push((entry.name().to_owned(), kind));
        }
        match entry {
            DocEntry::Newtype(newtype, _) => {
                if newtype.constructor.vis.is_exported() {
                    scope.top_level.push(newtype.constructor.name.clone());
                }
                if newtype.projector.vis.is_exported() {
                    scope.top_level.push(newtype.projector.name.clone());
                }
            }
            DocEntry::Labels(labels, _, _) => {
                scope.top_level.extend(
                    labels
                        .entries
                        .iter()
                        .filter(|entry| !entry.is_reuse_marker())
                        .map(|entry| entry.name.clone()),
                );
            }
            DocEntry::LabelNominal { labels, entry, .. } if labels.type_alias_name.is_none() => {
                scope.top_level.push(entry.name.clone());
            }
            _ => {}
        }
    }
    scope
}

fn add_label_nominals(scope: &mut ModuleScope, labels: &crate::ast::Labels) {
    for entry in labels
        .entries
        .iter()
        .filter(|entry| !entry.is_reuse_marker())
    {
        let name = crate::ast::mint_label_newtype_name(&entry.name);
        if labels.type_alias_name.as_ref() != Some(&name) {
            scope.top_level.push(name.clone());
            scope.type_level.push((name, TypeLevelKind::Newtype));
        }
    }
}

fn module_scope_from_surface_impl(module: &crate::ast::Module<crate::ast::Surface>) -> ModuleScope {
    let mut scope = ModuleScope {
        intrinsics_in_scope: false,
        ..Default::default()
    };

    // Collect names from import statements.
    for u in &module.imports {
        match &u.kind {
            crate::ast::ImportKind::Intrinsics => {
                scope.intrinsics_in_scope = true;
            }
            crate::ast::ImportKind::Selective { items, .. } => {
                for item in items {
                    if let Some(name) = item.as_name() {
                        scope.imported.push(name.to_owned());
                    } else if let Some((name, _)) = item.as_label() {
                        scope.imported.push(name.to_owned());
                        scope.imported.push(format!("{{{name}}}"));
                    }
                }
            }
            crate::ast::ImportKind::Qualified { alias, .. } => {
                scope.imported.push(alias.clone());
                scope.qualified_aliases.push(alias.clone());
            }
            crate::ast::ImportKind::Comptime => {
                scope.imported.extend(
                    crate::comptime::PUBLIC_COMPTIME_NAMES
                        .iter()
                        .map(|name| (*name).to_owned()),
                );
            }
        }
    }

    // Collect top-level item names.
    for item in &module.items {
        if let crate::ast::Item::RecGroup(group, _ext) = item {
            scope
                .top_level
                .extend(group.members.iter().map(|member| member.name.clone()));
        } else if let crate::ast::Item::TypeRecGroup(group) = item {
            scope
                .top_level
                .extend(group.members.iter().filter_map(|member| match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => Some(alias.name.clone()),
                    crate::ast::TypeRecMember::Newtype(newtype) => Some(newtype.name.clone()),
                    crate::ast::TypeRecMember::Labels(labels, _) => labels.type_alias_name.clone(),
                }));
        } else if let crate::ast::Item::LabelForward(forward, _) = item {
            scope.top_level.push(format!("{{{}}}", forward.name));
        } else {
            let name = surface_item_name(item);
            if !name.is_empty() {
                scope.top_level.push(name.to_owned());
            }
        }
        // Record the type-level names — `type` / `labels` type-alias /
        // `newtype` — so the `@type` directive can reject them. The
        // `op` item's resolvable name is the bound function, a
        // value-level name, so `op` is not type-level.
        match item {
            crate::ast::Item::TypeAlias(a) => scope
                .type_level
                .push((a.name.clone(), TypeLevelKind::TypeAlias)),
            crate::ast::Item::Newtype(nt) => scope
                .type_level
                .push((nt.name.clone(), TypeLevelKind::Newtype)),
            crate::ast::Item::Labels(labels, _ext) => {
                if let Some(alias_name) = &labels.type_alias_name {
                    scope
                        .type_level
                        .push((alias_name.clone(), TypeLevelKind::Labels));
                }
                add_label_nominals(&mut scope, labels);
            }
            crate::ast::Item::LabelForward(forward, _) => scope
                .type_level
                .push((format!("{{{}}}", forward.name), TypeLevelKind::LabelForward)),
            crate::ast::Item::HostType(h) => scope
                .type_level
                .push((h.name.clone(), TypeLevelKind::HostType)),
            crate::ast::Item::TypeRecGroup(group) => {
                for member in &group.members {
                    match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => scope
                            .type_level
                            .push((alias.name.clone(), TypeLevelKind::TypeAlias)),
                        crate::ast::TypeRecMember::Newtype(newtype) => scope
                            .type_level
                            .push((newtype.name.clone(), TypeLevelKind::Newtype)),
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            if let Some(name) = &labels.type_alias_name {
                                scope.type_level.push((name.clone(), TypeLevelKind::Labels));
                            }
                            add_label_nominals(&mut scope, labels);
                        }
                    }
                }
            }
            _ => {}
        }
        // Collect label names from `labels { … }` items — labels are valid
        // intra-doc ref targets.
        if let crate::ast::Item::Labels(labels, _ext) = item {
            // The type-alias name for `labels T = { … }`.
            if let Some(alias_name) = &labels.type_alias_name {
                scope.top_level.push(alias_name.clone());
            }
            // Individual label names from `labels { name: T, … }`.
            for entry in labels
                .entries
                .iter()
                .filter(|entry| !entry.is_reuse_marker())
            {
                scope.top_level.push(entry.name.clone());
            }
        }
        // Newtype constructors and projectors are also in scope.
        if let crate::ast::Item::Newtype(nt) = item {
            scope.top_level.push(nt.constructor.name.clone());
            scope.top_level.push(nt.projector.name.clone());
        }
        if let crate::ast::Item::TypeRecGroup(group) = item {
            for member in &group.members {
                match member {
                    crate::ast::TypeRecMember::Newtype(newtype) => {
                        scope.top_level.push(newtype.constructor.name.clone());
                        scope.top_level.push(newtype.projector.name.clone());
                    }
                    crate::ast::TypeRecMember::Labels(labels, _) => {
                        for entry in labels
                            .entries
                            .iter()
                            .filter(|entry| !entry.is_reuse_marker())
                        {
                            scope.top_level.push(entry.name.clone());
                        }
                    }
                    crate::ast::TypeRecMember::TypeAlias(_) => {}
                }
            }
        }
        if matches!(
            item,
            crate::ast::Item::Op(..) | crate::ast::Item::VariadicOperator(..)
        ) {
            for entry in crate::doc_entry::documented_entries_for_item(item) {
                scope.top_level.push(entry.name().to_owned());
            }
            if let crate::ast::Item::Op(op, _) = item
                && let Some(name) = operator_token_name(op)
            {
                scope.top_level.push(name);
            }
        }
    }

    scope
}

/// Extract the declared name for a `Surface` item. Returns `""` for
/// anonymous items (anonymous `labels`, `equiv`, `op`).
fn surface_item_name(item: &crate::ast::Item<crate::ast::Surface>) -> &str {
    match item {
        crate::ast::Item::FnDef(d) => &d.name,
        crate::ast::Item::TypeAlias(a) => &a.name,
        crate::ast::Item::LiteralAlias(l, _ext) => &l.name,
        crate::ast::Item::Newtype(d) => &d.name,
        crate::ast::Item::Elaborator(s, _ext) => &s.name,
        crate::ast::Item::Labels(t, _ext) => t.type_alias_name.as_deref().unwrap_or(""),
        crate::ast::Item::LabelForward(_, _) => "",
        crate::ast::Item::Equiv(e, _ext) => &e.name,
        crate::ast::Item::RecGroup(g, _ext) => g
            .members
            .first()
            .map(|member| member.name.as_str())
            .unwrap_or(""),
        crate::ast::Item::TypeRecGroup(_) => "",
        crate::ast::Item::Op(o, _ext) => {
            // Op's function path is the resolvable name — the last
            // segment is the local function name.
            match &o.body {
                crate::ast::OpBody::Normal { function, .. } => {
                    function.last().map(|s| s.as_str()).unwrap_or("")
                }
            }
        }
        crate::ast::Item::VariadicOperator(_, _ext) => "",
        crate::ast::Item::HostType(h) => &h.name,
        crate::ast::Item::HostFn(h) => &h.name,
    }
}

/// Build a [`PackageScope`] from a parsed package file. The package
/// file now carries only the `bridge { … }` glob list — it names no
/// declarations — so the only scope it contributes is the package's
/// declared module segments. The host contract surface lives in the
/// bridged modules.
pub fn package_scope_from_package_file(
    _package_file: &crate::ast::PackageFile<crate::ast::Surface>,
    module_path_segments: &[&str],
) -> PackageScope {
    let mut scope = PackageScope::default();

    // Module segments from the package's declared modules.
    for seg in module_path_segments {
        scope.module_segments.push((*seg).to_owned());
    }

    scope
}

// ---- Diagnostics helpers ----------------------------------------------------

/// Format the "unresolved intra-doc reference" message consistent with
/// the spec contract:
/// - Names the file and line of the unresolved reference.
/// - States what the resolver tried and why it failed.
///
/// `file_display` is the human-readable path for the error message.
/// `line` is the 1-based line number of the reference in its source context.
/// `name` is the payload of the unresolved `` [`name`] `` form.
/// `suggestion` is an optional close-name suggestion (edit-distance 1).
pub fn format_unresolved_ref_message(name: &str, suggestion: Option<&str>) -> String {
    let base = format!("unresolved intra-doc reference: `{name}` is not in scope");
    match suggestion {
        Some(s) => format!("{base}; did you mean `{s}`?"),
        None => base,
    }
}

/// Find the closest name in `candidates` to `name` by edit distance,
/// if within threshold 2. Returns `None` if no candidate is within
/// the threshold.
pub fn closest_name<'a>(name: &str, candidates: &'a [String]) -> Option<&'a str> {
    let mut best: Option<(&str, usize)> = None;
    for candidate in candidates {
        let dist = edit_distance(name, candidate.as_str());
        if dist <= 2 {
            match best {
                None => best = Some((candidate.as_str(), dist)),
                Some((_, prev)) if dist < prev => best = Some((candidate.as_str(), dist)),
                _ => {}
            }
        }
    }
    best.map(|(s, _)| s)
}

/// Compute the edit distance between two strings (Levenshtein).
/// Used for "did you mean" suggestions.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    #[allow(clippy::needless_range_loop)]
    for i in 0..=n {
        dp[i][0] = i;
    }
    #[allow(clippy::needless_range_loop)]
    for j in 0..=m {
        dp[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[n][m]
}

// ---- Helpers ----------------------------------------------------------------

/// True if `name` looks like a valid qualified path — a module path
/// whose segments are `/`-separated, reaching an item with `.`
/// (`pkg/util.name`), or an alias-rooted member access (`m.name`).
/// Each segment between separators must be a non-empty string of
/// ASCII letters, digits, or underscores. This is a loose syntactic
/// check — we don't validate paths outside the documented package.
fn looks_like_qualified_path(name: &str) -> bool {
    if let Some((module, selector)) = name.rsplit_once('.')
        && let Some(label) = selector.strip_prefix('{').and_then(|s| s.strip_suffix('}'))
    {
        return !label.is_empty()
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !module.is_empty()
            && module.split(['/', '.']).all(|segment| {
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
    }
    if name.is_empty() {
        return false;
    }
    name.split(['/', '.'])
        .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

/// The intrinsic names brought into scope by `import __intrinsics__;`.
const INTRINSICS: &[&str] = &[
    "__left__",
    "__right__",
    "__either__",
    "__pair__",
    "__fst__",
    "__snd__",
    "__if_then_else__",
    "__absurd__",
];

fn is_intrinsic(name: &str) -> bool {
    INTRINSICS.contains(&name)
}

/// Byte span for a reference, computed from a base offset plus the
/// ref's intra-text offset. Used to map errors back into the source.
pub fn ref_span(base_offset: u32, ref_offset: u32) -> Span {
    Span::new(base_offset + ref_offset, base_offset + ref_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_simple_ref() {
        let refs = scan_refs("See [`foo`] for details.");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "foo");
    }

    #[test]
    fn inline_code_shields_reference_patterns() {
        for literal in [
            "`[` or `]`",
            "`[` but no `]`",
            "`*[` pairs with `]*`",
            "`` [`missing`] ``",
            "``` [`missing`] `` still code ```",
            "`` quoted\n[`missing`] ``",
        ] {
            let text = format!("λ {literal}; see [`present`].");
            assert_eq!(
                scan_refs(&text),
                vec![IntraRef {
                    name: "present".to_owned(),
                    offset: text.find("[`present`]").unwrap() as u32,
                }],
                "{text}"
            );
        }
    }

    #[test]
    fn unmatched_backtick_runs_do_not_hide_references() {
        for text in [
            "`` unmatched [`present`]",
            "`` unmatched\n\n[`present`]; ``",
        ] {
            let refs = scan_refs(text);
            assert_eq!(refs.len(), 1, "{text}: {refs:?}");
            assert_eq!(refs[0].name, "present");
        }
    }

    #[test]
    fn inline_code_stops_before_markdown_blocks() {
        for block in [
            "# [`missing`]",
            "---\n[`missing`]",
            "<aside>\n[`missing`]",
            "> [`missing`]",
            "- [`missing`]",
            "1. [`missing`]",
        ] {
            let text = format!("` unmatched\n{block}\n`");
            let refs = scan_refs(&text);
            assert_eq!(refs.len(), 1, "{text}: {refs:?}");
            assert_eq!(refs[0].name, "missing");
        }
        let refs = scan_refs("# ` unmatched\n[`missing`]\n`");
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert_eq!(refs[0].name, "missing");
        assert!(scan_refs("`` ordinary\n[`literal`] code ``").is_empty());
    }

    #[test]
    fn escaped_backticks_and_brackets_remain_literal() {
        assert_eq!(scan_refs(r"\` prose [`present`]")[0].name, "present");
        assert!(scan_refs(r"\[`missing`]").is_empty());
        assert!(scan_refs(r"\\`[` or `]`").is_empty());
    }

    #[test]
    fn scan_qualified_ref() {
        let refs = scan_refs("Use [`pkg.mod.foo`] here.");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "pkg.mod.foo");
    }

    #[test]
    fn scan_no_backtick_skipped() {
        let refs = scan_refs("This [foo] is plain Markdown.");
        assert!(refs.is_empty(), "plain [name] should not be a ref");
    }

    #[test]
    fn scan_multiple_refs() {
        let refs = scan_refs("See [`foo`] and [`bar`].");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name, "foo");
        assert_eq!(refs[1].name, "bar");
    }

    #[test]
    fn scan_skip_inline_link() {
        // [`name`](url) is an inline Markdown link, not an intra-doc ref.
        let refs = scan_refs("[`foo`](http://example.com)");
        assert!(refs.is_empty());
    }

    #[test]
    fn scan_skip_ref_link_use() {
        // [`name`][key] is a Markdown reference link use, not an intra-doc ref.
        let refs = scan_refs("[`foo`][bar]");
        assert!(refs.is_empty());
    }

    #[test]
    fn scan_skip_directive_at_prefix() {
        // [`@signature foo`] starts with `@` — it's a directive, not an
        // intra-doc ref. The ref scanner must not pick it up.
        let refs = scan_refs("[`@signature foo`]");
        assert!(
            refs.is_empty(),
            "directive patterns must not be picked up by ref scanner"
        );

        let refs = scan_refs("[`@source bar.baz`]");
        assert!(
            refs.is_empty(),
            "directive patterns must not be picked up by ref scanner"
        );

        let refs = scan_refs("[`@type foo`]");
        assert!(
            refs.is_empty(),
            "directive patterns must not be picked up by ref scanner"
        );

        let refs = scan_refs("[`@unknown x`]");
        assert!(
            refs.is_empty(),
            "unknown directive patterns also skipped by ref scanner"
        );
    }

    #[test]
    fn collect_overrides_basic() {
        let overrides = collect_ref_overrides("[foo]: https://example.com\n[bar]: something\n");
        assert!(overrides.contains("foo"));
        assert!(overrides.contains("bar"));
    }

    #[test]
    fn collect_overrides_does_not_collect_backtick_refs() {
        // A `` [`name`] `` at the start of a line is not an override.
        let overrides = collect_ref_overrides("[`foo`] is a reference\n");
        assert!(!overrides.contains("`foo`"));
        assert!(!overrides.contains("foo"));
    }

    #[test]
    fn collect_overrides_ignores_visible_and_hidden_fences() {
        let text = "[prose]: /ok\n```markdown\n[visible]: /no\n```\n<!--text\n[hidden]: /no\n-->\n";
        let overrides = collect_ref_overrides(text);
        assert!(overrides.contains("prose"));
        assert!(!overrides.contains("visible"));
        assert!(!overrides.contains("hidden"));
    }

    #[test]
    fn resolve_in_module_top_level() {
        let scope = ModuleScope {
            top_level: vec!["my_fn".to_owned()],
            ..Default::default()
        };
        assert_eq!(resolve_in_module("my_fn", &scope), RefOutcome::Resolved);
    }

    #[test]
    fn forwarding_selectors_keep_label_visibility_and_nonvalue_identity() {
        let module = crate::pass::parser::parse(concat!(
            "module pkg/api; pub fn field() -> . { () } ",
            "pub type {field} = {original}; type {hidden} = {original};",
        ))
        .unwrap();
        let local = module_scope_from_surface(&module);
        let boundary = module_scope_from_surface_for_boundary(&module);
        for scope in [&local, &boundary] {
            assert_eq!(resolve_in_module("field", scope), RefOutcome::Resolved);
            assert_eq!(resolve_in_module("{field}", scope), RefOutcome::Resolved);
            assert_eq!(resolve_in_module("Field", scope), RefOutcome::Unresolved);
            assert_eq!(type_level_kind_in_module("field", scope), None);
            assert_eq!(
                type_level_kind_in_module("{field}", scope),
                Some(TypeLevelKind::LabelForward)
            );
        }
        assert_eq!(resolve_in_module("{hidden}", &local), RefOutcome::Resolved);
        assert_eq!(
            resolve_in_module("{hidden}", &boundary),
            RefOutcome::Unresolved
        );
        assert_eq!(resolve_in_module("hidden", &local), RefOutcome::Unresolved);
        let consumer =
            crate::pass::parser::parse("module pkg/main; import pkg/api({field});").unwrap();
        assert_eq!(
            resolve_in_module("{field}", &module_scope_from_surface(&consumer)),
            RefOutcome::Resolved
        );
        assert!(looks_like_qualified_path("pkg/api.{field}"));
        for invalid in ["pkg/api.{}", "pkg/api.{a.b}", "pkg/api.{{field}}"] {
            assert!(!looks_like_qualified_path(invalid), "{invalid}");
        }
    }

    #[test]
    fn module_scope_indexes_only_explicit_label_declarations() {
        let module = crate::pass::parser::parse(
            "module x; labels { field: . }; labels Row = { field: _, other: . };",
        )
        .expect("parse labels");
        let scope = module_scope_from_surface(&module);
        assert_eq!(
            scope
                .top_level
                .iter()
                .filter(|name| name.as_str() == "field")
                .count(),
            1
        );
        assert_eq!(
            scope
                .top_level
                .iter()
                .filter(|name| *name == "Field")
                .count(),
            1
        );
        for name in ["Field", "Other"] {
            assert_eq!(resolve_in_module(name, &scope), RefOutcome::Resolved);
            assert_eq!(
                type_level_kind_in_module(name, &scope),
                Some(TypeLevelKind::Newtype)
            );
        }
        assert_eq!(type_level_kind_in_module("field", &scope), None);
        assert_eq!(
            type_level_kind_in_module("Row", &scope),
            Some(TypeLevelKind::Labels)
        );
    }

    #[test]
    fn label_nominal_scope_uses_only_local_declarations_and_exact_visibility() {
        let module = crate::pass::parser::parse(
            "module pkg/main; import pkg/other({imported}); \
             labels { local: . }; pub(pkg) labels { scoped: . }; \
             pub labels Public = { exported: . }; \
             newtype Explicit : . { constructor make; projector read; };",
        )
        .unwrap();
        let local = module_scope_from_surface(&module);
        for name in ["Local", "Scoped", "Exported", "Explicit"] {
            assert_eq!(
                type_level_kind_in_module(name, &local),
                Some(TypeLevelKind::Newtype)
            );
        }
        assert_eq!(resolve_in_module("imported", &local), RefOutcome::Resolved);
        assert_eq!(
            resolve_in_module("Imported", &local),
            RefOutcome::Unresolved
        );

        let boundary = module_scope_from_surface_for_boundary(&module);
        assert_eq!(
            resolve_in_module("Exported", &boundary),
            RefOutcome::Resolved
        );
        for name in ["Local", "Scoped", "Imported"] {
            assert_eq!(resolve_in_module(name, &boundary), RefOutcome::Unresolved);
        }
    }

    #[test]
    fn label_nominal_scope_does_not_duplicate_an_alias_head() {
        // Scope extraction precedes duplicate-declaration validation; it must
        // not manufacture a second kind for the same written alias name.
        let module = crate::pass::parser::parse("module x; labels Field = { field: . };").unwrap();
        let scope = module_scope_from_surface(&module);
        let kinds = scope
            .type_level
            .iter()
            .filter(|(name, _)| name == "Field")
            .collect::<Vec<_>>();
        assert_eq!(kinds, [&("Field".to_owned(), TypeLevelKind::Labels)]);
    }

    #[test]
    fn boundary_roles_require_both_owner_and_role_public_visibility() {
        for outer in ["", "pub(api) ", "pub "] {
            for constructor in ["", "pub(api) ", "pub "] {
                for projector in ["", "pub(api) ", "pub "] {
                    let module = crate::pass::parser::parse(&format!(
                        "module api; {outer}newtype Box : . {{ {constructor}constructor make; {projector}projector read; }};"
                    )).unwrap();
                    let local = module_scope_from_surface(&module);
                    let public = module_scope_from_surface_for_boundary(&module);
                    for name in ["Box", "make", "read"] {
                        assert_eq!(resolve_in_module(name, &local), RefOutcome::Resolved);
                    }
                    for (name, visible) in [
                        ("Box", outer == "pub "),
                        ("make", outer == "pub " && constructor == "pub "),
                        ("read", outer == "pub " && projector == "pub "),
                    ] {
                        assert_eq!(
                            resolve_in_module(name, &public),
                            if visible {
                                RefOutcome::Resolved
                            } else {
                                RefOutcome::Unresolved
                            },
                            "{outer}/{constructor}/{projector}: {name}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn boundary_imports_and_private_recursive_roles_are_not_reexports() {
        let module = crate::pass::parser::parse(
            "module api; import other(imported, {field}); import elsewhere as alias; import __intrinsics__; \
             rec { pub newtype Visible : . | Hidden { constructor hide_make; pub projector read; }; \
             newtype Hidden : . | Visible { pub constructor hidden_make; pub projector hidden_read; }; } \
             pub labels Row = { reused: _, owned: . };"
        ).unwrap();
        let scope = module_scope_from_surface_for_boundary(&module);
        for name in ["Visible", "read", "Row", "owned", "Owned"] {
            assert_eq!(
                resolve_in_module(name, &scope),
                RefOutcome::Resolved,
                "{name}"
            );
        }
        for name in [
            "imported",
            "field",
            "alias",
            "__left__",
            "hide_make",
            "Hidden",
            "hidden_make",
            "hidden_read",
            "reused",
            "Reused",
        ] {
            assert_eq!(
                resolve_in_module(name, &scope),
                RefOutcome::Unresolved,
                "{name}"
            );
        }
        assert!(scope.imported.is_empty());
        assert!(scope.qualified_aliases.is_empty());
        assert!(!scope.intrinsics_in_scope);
    }

    #[test]
    fn resolve_in_module_imported() {
        let scope = ModuleScope {
            imported: vec!["SomeType".to_owned()],
            ..Default::default()
        };
        assert_eq!(resolve_in_module("SomeType", &scope), RefOutcome::Resolved);
    }

    #[test]
    fn resolve_in_module_unresolved() {
        let scope = ModuleScope::default();
        assert_eq!(resolve_in_module("unknown", &scope), RefOutcome::Unresolved);
    }

    #[test]
    fn resolve_qualified_path_accepted() {
        let scope = ModuleScope::default();
        // Qualified paths outside the documented package are accepted optimistically.
        assert_eq!(
            resolve_in_module("pkg.mod.foo", &scope),
            RefOutcome::Resolved
        );
    }

    #[test]
    fn resolve_in_package_exported_name() {
        let scope = PackageScope {
            exported_names: vec!["print".to_owned()],
            ..Default::default()
        };
        assert_eq!(resolve_in_package("print", &scope), RefOutcome::Resolved);
    }

    #[test]
    fn resolve_in_package_unresolved() {
        let scope = PackageScope::default();
        assert_eq!(
            resolve_in_package("unknown", &scope),
            RefOutcome::Unresolved
        );
    }

    #[test]
    fn check_refs_in_module_override_suppresses() {
        let scope = ModuleScope::default();
        let mut overrides = std::collections::HashSet::new();
        overrides.insert("foo".to_owned());
        // `foo` is not in scope, but it's overridden — no error.
        let errors = check_refs_in_module("[`foo`]", &scope, &overrides);
        assert!(errors.is_empty());
    }

    #[test]
    fn edit_distance_same() {
        assert_eq!(edit_distance("foo", "foo"), 0);
    }

    #[test]
    fn edit_distance_one_sub() {
        assert_eq!(edit_distance("foo", "bar"), 3);
        assert_eq!(edit_distance("foo", "fao"), 1);
    }

    #[test]
    fn closest_name_finds_near_match() {
        let candidates: Vec<String> = vec!["my_fn".to_owned(), "other".to_owned()];
        assert_eq!(closest_name("my_fn", &candidates), Some("my_fn"));
        assert_eq!(closest_name("my_fn_typo", &candidates), None); // too far
    }

    #[test]
    fn looks_like_qualified_path_valid() {
        assert!(looks_like_qualified_path("pkg.mod.foo"));
        assert!(looks_like_qualified_path("a.b"));
    }

    #[test]
    fn looks_like_qualified_path_invalid() {
        assert!(!looks_like_qualified_path(""));
        assert!(!looks_like_qualified_path("a..b"));
        assert!(!looks_like_qualified_path(".a"));
    }
}
