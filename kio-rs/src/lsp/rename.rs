//! LSP `textDocument/rename` and `textDocument/prepareRename` handlers.
//!
//! ## `prepareRename`
//!
//! Given a position, return the span of the identifier that would be
//! renamed so the editor can pre-fill the rename popup and validate the
//! user's input. Returns `None` (→ JSON `null`) when the position is on
//! whitespace, a literal, a keyword token, or an unresolved reference —
//! the editor disables rename for those positions.
//!
//! ## `rename`
//!
//! Given a position and a new name:
//!
//! 1. Resolve the binder at the cursor via the position index (same
//!    lookup as goto-definition).
//! 2. Validate the new name: it must lex as a single `Ident` token.
//!    Invalid names (e.g. pure operator strings, reserved-word
//!    collisions like `__intrinsics__`, bare underscores) produce a
//!    JSON-RPC error.
//! 3. Walk references (same scan as `textDocument/references`): collect
//!    every span in the package that resolves to the same binder.
//! 4. Conflict check: for each reference site, ask the completion scope
//!    walker whether the new name is already in scope there *and*
//!    resolves to something other than the binder being renamed. If
//!    any reference site conflicts, return a JSON-RPC error naming the
//!    collision.
//! 5. Build and return a `WorkspaceEdit` with one `TextEdit` per
//!    reference span, grouped by URI.
//!
//! ## Scope of rename
//!
//! Cross-file within the current package. Package-external rename is
//! not attempted: the package boundary is the analysis unit.
//!
//! `pub op` bindings are refused similarly — renaming a `pub op` affects
//! parser behavior in every importer.
//!
//! ## Out of scope
//!
//! - Doc-comment rewriting: `// old_name` in a comment is not a
//!   reference and is not rewritten. Users fix mentions manually.
//! - Sound shadowing-aware rename: the conflict check is conservative
//!   (refuse on collision rather than reorder).
//! - Native label syntax: its import, construction, and access spellings
//!   do not participate in ordinary identifier renaming.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use lsp_types::{Position, PrepareRenameResponse, Range, TextEdit, Uri, WorkspaceEdit};

use crate::ast::{Expr, FnDef, ImportKind, Item, Module, Signature, SignatureParam, Surface};
use crate::cmd::check::LspAnalysis;
use crate::lsp::diagnostics::path_to_uri;
use crate::lsp::live_syntax::{LiveSyntaxSnapshot, snapshot_for};
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::lsp::references::{binder_key_pub, binder_matches_key_pub};
use crate::lsp::util::{smallest_containing_span, uri_to_canonical, workspace_edit_from_changes};
use crate::pass::parser::parse_module_file;
use crate::pass::typecheck_full::ResolvedBinder;
use crate::span::Span;

#[derive(Clone, Copy)]
pub(crate) struct RenameParseContext<'a> {
    pub package_root: &'a std::path::Path,
    pub current_module: Option<&'a Module<Surface>>,
}

/// Handle one `textDocument/prepareRename` request.
///
/// Returns `Some(PrepareRenameResponse::RangeWithPlaceholder)` when the
/// cursor is on a renameable binder. Returns `None` (→ JSON `null`)
/// when the cursor is on a non-renameable position.
pub fn handle_prepare_rename(
    uri: &Uri,
    position: &Position,
    analysis: &LspAnalysis,
    overlay: Option<LiveSyntaxSnapshot<'_>>,
) -> Option<PrepareRenameResponse> {
    handle_prepare_rename_with_module(uri, position, analysis, overlay, None)
}

pub(crate) fn handle_prepare_rename_with_module(
    uri: &Uri,
    position: &Position,
    analysis: &LspAnalysis,
    overlay: Option<LiveSyntaxSnapshot<'_>>,
    current_module: Option<&Module<Surface>>,
) -> Option<PrepareRenameResponse> {
    let canonical = uri_to_canonical(uri)?;
    let module_path = analysis.file_to_module.get(canonical.as_path())?;

    let syntax = snapshot_for(canonical.as_path(), analysis, overlay)?;
    let source = syntax.source;
    let line_index = LineIndex::new(source);

    let lsp_pos = LspPosition {
        line: position.line,
        character: position.character,
    };
    let byte_offset = line_index.position_to_offset(lsp_pos);

    let parsed;
    if let Some((alias, span)) = analysis.import_aliases.at(module_path, byte_offset) {
        if analysis.sources.get(&canonical).map(String::as_str) != Some(source) {
            return None;
        }
        return Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: to_lsp_range(line_index.to_range(span)),
            placeholder: alias.name.clone(),
        });
    }
    let module = if let Some(module) = current_module {
        Some(module)
    } else {
        parsed = parse_surface_module(source);
        parsed.as_ref()
    };
    if let Some(family) = module.and_then(|module| placeholder_family_at(module, byte_offset)) {
        return Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: to_lsp_range(line_index.to_range(family.cursor_stem)),
            placeholder: family.stem.name,
        });
    }

    // Find the binder at the cursor.
    let cursor = smallest_containing_span(
        byte_offset,
        analysis
            .position_index
            .binders_iter()
            .filter(|((mp, _), _)| mp == module_path)
            .map(|((_, span), binder)| (*span, binder)),
    );

    let (cursor_span, cursor_binder) = cursor?;

    // Only renameable binder kinds. Refuse on pub-item and op binders
    // whose rename would cross the package boundary or affect parsing.
    if !is_renameable(cursor_binder, analysis) {
        return None;
    }

    // Extract the source text of the span as the placeholder.
    let placeholder = span_text(source, cursor_span).to_owned();
    // Must be a non-empty identifier-shaped string.
    if placeholder.is_empty() {
        return None;
    }

    let lsp_range = line_index.to_range(cursor_span);
    let range = to_lsp_range(lsp_range);

    Some(PrepareRenameResponse::RangeWithPlaceholder { range, placeholder })
}

/// Handle one `textDocument/rename` request.
///
/// Returns `Ok(Some(WorkspaceEdit))` on success, `Ok(None)` when the
/// cursor is not on a renameable binder. Returns `Err(message)` for
/// validation failures (invalid new name or conflict).
pub fn handle_rename(
    uri: &Uri,
    position: &Position,
    new_name: &str,
    analysis: &LspAnalysis,
    overlay: Option<LiveSyntaxSnapshot<'_>>,
    package_root: &std::path::Path,
) -> Result<Option<WorkspaceEdit>, String> {
    handle_rename_inner(
        uri,
        position,
        new_name,
        analysis,
        overlay,
        RenameParseContext {
            package_root,
            current_module: None,
        },
        None,
    )
}

/// As [`handle_rename`], but returns versioned `documentChanges` using
/// the analysis snapshot's `(URI, version)` map. Editors can reject the
/// edit if an open document moved past the snapshot the edit was based
/// on.
pub fn handle_rename_versioned(
    uri: &Uri,
    position: &Position,
    new_name: &str,
    analysis: &LspAnalysis,
    overlay: Option<LiveSyntaxSnapshot<'_>>,
    package_root: &std::path::Path,
    snapshot_versions: &BTreeMap<Uri, i32>,
) -> Result<Option<WorkspaceEdit>, String> {
    handle_rename_inner(
        uri,
        position,
        new_name,
        analysis,
        overlay,
        RenameParseContext {
            package_root,
            current_module: None,
        },
        Some(snapshot_versions),
    )
}

pub(crate) fn handle_rename_versioned_with_context(
    uri: &Uri,
    position: &Position,
    new_name: &str,
    analysis: &LspAnalysis,
    overlay: Option<LiveSyntaxSnapshot<'_>>,
    parse_context: RenameParseContext<'_>,
    snapshot_versions: &BTreeMap<Uri, i32>,
) -> Result<Option<WorkspaceEdit>, String> {
    handle_rename_inner(
        uri,
        position,
        new_name,
        analysis,
        overlay,
        parse_context,
        Some(snapshot_versions),
    )
}

fn handle_rename_inner(
    uri: &Uri,
    position: &Position,
    new_name: &str,
    analysis: &LspAnalysis,
    overlay: Option<LiveSyntaxSnapshot<'_>>,
    parse_context: RenameParseContext<'_>,
    snapshot_versions: Option<&BTreeMap<Uri, i32>>,
) -> Result<Option<WorkspaceEdit>, String> {
    let RenameParseContext {
        package_root,
        current_module,
    } = parse_context;
    let canonical = uri_to_canonical(uri).ok_or_else(|| "cannot resolve URI".to_owned())?;
    let module_path = analysis
        .file_to_module
        .get(canonical.as_path())
        .ok_or_else(|| "file not part of any analyzed package".to_owned())?
        .clone();

    let syntax = snapshot_for(canonical.as_path(), analysis, overlay)
        .ok_or_else(|| "source text not available".to_owned())?;
    let source = syntax.source;
    let line_index = LineIndex::new(source);

    let lsp_pos = LspPosition {
        line: position.line,
        character: position.character,
    };
    let byte_offset = line_index.position_to_offset(lsp_pos);

    let parsed_modules = parse_analysis_modules(analysis);

    if let Some((alias, _)) = analysis.import_aliases.at(&module_path, byte_offset) {
        if analysis.sources.get(&canonical).map(String::as_str) != Some(source) {
            return Err("selective alias positions are not current".to_owned());
        }
        let local_binder = if crate::naming::is_type_name(&alias.name) {
            ResolvedBinder::TypeParam {
                name: alias.name.clone(),
                decl_span: Some(alias.declaration),
            }
        } else {
            ResolvedBinder::Local {
                name: alias.name.clone(),
                decl_span: Some(alias.declaration),
            }
        };
        validate_new_name_for_binder(new_name, &local_binder)?;
        let module = current_module
            .or_else(|| parsed_modules.get(&canonical))
            .ok_or_else(|| "source module not available".to_owned())?;
        let resolved_scope = analysis
            .root_package_lowered
            .module(&module_path)
            .map(|entry| &entry.scope);
        let lexical_scope = crate::scope_walk::SourceBindingIndex::new(module);
        for span in &alias.occurrences {
            let prospective = crate::tokens::SourceReference {
                path: new_name.to_owned(),
                span: *span,
                declaration: false,
                kind: if crate::naming::is_type_name(&alias.name) {
                    crate::tokens::SourceNameKind::Type
                } else {
                    crate::tokens::SourceNameKind::Value
                },
            };
            if (new_name != alias.name && lexical_scope.origin(&prospective).is_some())
                || name_conflicts_at_offset(
                    module,
                    resolved_scope,
                    new_name,
                    span.start,
                    &local_binder,
                )
            {
                return Err(format!(
                    "rename to `{new_name}` would conflict with an existing binding"
                ));
            }
        }
        let edits = alias
            .occurrences
            .iter()
            .map(|span| TextEdit {
                range: to_lsp_range(line_index.to_range(*span)),
                new_text: new_name.to_owned(),
            })
            .collect();
        return Ok(Some(workspace_edit_from_changes(
            HashMap::from([(uri.clone(), edits)]),
            snapshot_versions,
        )));
    }

    if let Some(family) = current_module
        .or_else(|| parsed_modules.get(canonical.as_path()))
        .and_then(|module| placeholder_family_at(module, byte_offset))
    {
        let edits = family.rename(new_name, &line_index)?;
        let changes = HashMap::from([(uri.clone(), edits)]);
        return Ok(Some(workspace_edit_from_changes(
            changes,
            snapshot_versions,
        )));
    }

    // 1. Resolve the binder at the cursor.
    let cursor = smallest_containing_span(
        byte_offset,
        analysis
            .position_index
            .binders_iter()
            .filter(|((mp, _), _)| mp == module_path.as_str())
            .map(|((_, span), binder)| (*span, binder)),
    );

    let (_, cursor_binder) =
        cursor.ok_or_else(|| "no renameable identifier at cursor".to_owned())?;

    if !is_renameable(cursor_binder, analysis) {
        return Err("the identifier at the cursor cannot be renamed".to_owned());
    }

    // 2. Validate the new name in the resolved binder's namespace.
    validate_new_name_for_binder(new_name, cursor_binder)?;
    if block_label_name_conflicts(cursor_binder, new_name, analysis) {
        return Err(format!(
            "rename to `{new_name}` would conflict with another trailing label of the same macro"
        ));
    }

    // 3. Collect all reference spans in the package.
    let key = binder_key_pub(cursor_binder, module_path.as_str());
    let mut locations: Vec<(PathBuf, Span)> = Vec::new();
    let mut value_path_qualifier_spans: HashMap<String, Vec<Span>> = HashMap::new();
    for ((entry_module_path, entry_span), entry_binder) in analysis.position_index.binders_iter() {
        if matches!(
            entry_binder,
            ResolvedBinder::QualifiedImport { .. } | ResolvedBinder::Newtype { .. }
        ) {
            value_path_qualifier_spans
                .entry(entry_module_path.clone())
                .or_default()
                .push(*entry_span);
        }
        if analysis
            .import_aliases
            .contains(entry_module_path, *entry_span)
            || !binder_matches_key_pub(entry_binder, entry_module_path, &key)
        {
            continue;
        }
        if let Some(file_path) = analysis
            .file_to_module
            .iter()
            .find(|(_, mp)| mp.as_str() == entry_module_path)
            .map(|(fp, _)| fp.clone())
        {
            locations.push((file_path, *entry_span));
        }
    }
    for spans in value_path_qualifier_spans.values_mut() {
        spans.sort_unstable_by_key(|span| span.end);
        spans.dedup();
    }

    // Sort for stable output: file path then span start.
    locations.sort_by(|(fa, sa), (fb, sb)| {
        fa.cmp(fb)
            .then_with(|| sa.start.cmp(&sb.start))
            .then_with(|| sa.end.cmp(&sb.end))
    });
    locations.dedup();

    // 4. Conflict check: for each reference site, verify the new name
    //    is not already in scope there (resolving to a different binder).
    for (file_path, span) in &locations {
        if analysis
            .file_to_module
            .get(file_path)
            .is_some_and(|module| {
                analysis
                    .import_aliases
                    .is_aliased_source_selector(module, *span)
            })
        {
            // This token selects a provider declaration, not a consumer binding.
            // The provider declaration site supplies its namespace check.
            continue;
        }
        let file_source = match analysis.sources.get(file_path.as_path()) {
            Some(s) => s.as_str(),
            None => continue,
        };
        let file_li = LineIndex::new(file_source);
        // Use the span start as the reference byte offset for scope walk.
        let ref_offset = span.start;

        let qualified_value_path_leaf = matches!(
            cursor_binder,
            ResolvedBinder::Fn { .. } | ResolvedBinder::NewtypeMember { .. }
        ) && analysis
            .file_to_module
            .get(file_path.as_path())
            .is_some_and(|module_path| {
                is_qualified_value_path_leaf(
                    file_source,
                    module_path,
                    *span,
                    &value_path_qualifier_spans,
                )
            });

        let resolved_module_scope = analysis
            .file_to_module
            .get(file_path.as_path())
            .and_then(|module_path| analysis.root_package_lowered.module(module_path))
            .map(|entry| &entry.scope);
        if let Some(module) = parsed_modules.get(file_path.as_path())
            && !qualified_value_path_leaf
            && !matches!(cursor_binder, ResolvedBinder::BlockLabel { .. })
            && name_conflicts_at_offset(
                module,
                resolved_module_scope,
                new_name,
                ref_offset,
                cursor_binder,
            )
        {
            let ref_range = file_li.to_range(*span);
            return Err(format!(
                "rename to `{}` would conflict with an existing binding at {}:{}",
                new_name,
                file_path.display(),
                ref_range.start.line + 1,
            ));
        }
    }

    // 5. Build WorkspaceEdit grouped by URI.
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for (file_path, span) in locations {
        let file_source = match analysis.sources.get(file_path.as_path()) {
            Some(s) => s.as_str(),
            None => continue,
        };
        let file_li = LineIndex::new(file_source);
        let lsp_range = file_li.to_range(span);
        let range = to_lsp_range(lsp_range);
        let new_text = new_name.to_owned();
        let edit = TextEdit { range, new_text };
        if let Some(uri) = path_to_uri(&file_path, package_root) {
            changes.entry(uri).or_default().push(edit);
        }
    }

    if changes.is_empty() {
        // Binder found but no editable locations (e.g. intrinsics). Return
        // an empty WorkspaceEdit so the editor gets a clean no-op.
        return Ok(Some(workspace_edit_from_changes(
            changes,
            snapshot_versions,
        )));
    }

    Ok(Some(workspace_edit_from_changes(
        changes,
        snapshot_versions,
    )))
}

/// Whether `member_span` follows a resolved module or nominal qualifier. A
/// local or module-level value cannot capture that path leaf; its declaration
/// remains in the rename location set and is checked in the owning namespace.
fn is_qualified_value_path_leaf(
    source: &str,
    module_path: &str,
    member_span: Span,
    value_path_qualifier_spans: &HashMap<String, Vec<Span>>,
) -> bool {
    let Some(qualifier_spans) = value_path_qualifier_spans.get(module_path) else {
        return false;
    };
    let before = qualifier_spans.partition_point(|span| span.end <= member_span.start);
    let Some(qualifier_span) = before
        .checked_sub(1)
        .and_then(|index| qualifier_spans.get(index))
    else {
        return false;
    };
    let Some(gap) = source.get(qualifier_span.end as usize..member_span.start as usize) else {
        return false;
    };
    let Ok(tokens) = crate::pass::lexer::lex(gap) else {
        return false;
    };
    tokens.len() == 1 && tokens[0].kind.is_sym(".")
}

fn parse_analysis_modules(analysis: &LspAnalysis) -> BTreeMap<PathBuf, Module<Surface>> {
    let mut modules = BTreeMap::new();
    for (file_path, source) in &analysis.sources {
        if let Ok(file) = parse_module_file(source) {
            modules.insert(file_path.clone(), file.module);
        }
    }
    modules
}

/// Validate `new_name` as a lexical Kio identifier.
///
/// The name must lex as a single `Ident` token. This naturally rejects:
/// - Empty strings.
/// - Strings that start with an operator character or digit.
/// - Pure-underscore runs of length ≥ 4 (rejected by the lexer as
///   reserved slot-token extensions).
/// - Anything that produces a non-`Ident` token (operators, literals, …).
///
/// Note: Kio does **not** have hard reserved keywords — `fn`, `let`,
/// `module`, etc. lex as plain `Ident` tokens and are disambiguated by
/// the parser based on position. A rename to `fn` or `let` is
/// syntactically valid (the token is an `Ident`), though it may produce
/// confusing source in practice. The lexer-level check is the right
/// boundary to enforce; stricter keyword rejection would diverge from
/// the language spec.
fn validate_new_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("new name must not be empty".to_owned());
    }
    match crate::pass::lexer::lex(name) {
        Ok(tokens) => {
            // Must produce exactly one token.
            if tokens.len() != 1 {
                return Err(format!(
                    "`{}` is not a valid Kio identifier (lexes as {} tokens)",
                    name,
                    tokens.len()
                ));
            }
            match &tokens[0].kind {
                crate::pass::lexer::TokenKind::Ident(_) => Ok(()),
                other => Err(format!(
                    "`{}` is not a valid identifier (token kind: {:?})",
                    name, other
                )),
            }
        }
        Err(e) => {
            let (_, msg) = e.diag();
            Err(format!("`{}` is not a valid identifier: {}", name, msg))
        }
    }
}

/// Preserve the resolved binder's spelling-defined namespace.
fn validate_new_name_for_binder(name: &str, binder: &ResolvedBinder) -> Result<(), String> {
    validate_new_name(name)?;
    match binder {
        ResolvedBinder::TypeParam { .. }
        | ResolvedBinder::TypeAlias { .. }
        | ResolvedBinder::Newtype { .. } => {
            let role = crate::naming::NameRole::Type;
            crate::naming::validate_user_name(name, role)
                .map_err(|reason| format!("type name `{name}` {}", reason.explanation(role)))
        }
        ResolvedBinder::Local { .. }
        | ResolvedBinder::BlockLabel { .. }
        | ResolvedBinder::Fn { .. }
        | ResolvedBinder::NewtypeMember { .. } => {
            let role = crate::naming::NameRole::Value;
            crate::naming::validate_user_name(name, role)
                .map_err(|reason| format!("value name `{name}` {}", reason.explanation(role)))
        }
        ResolvedBinder::HostEnvFn { .. }
        | ResolvedBinder::HostType { .. }
        | ResolvedBinder::Intrinsic { .. }
        | ResolvedBinder::QualifiedImport { .. }
        | ResolvedBinder::QualifiedImportMember { .. } => {
            Err("the identifier at the cursor cannot be renamed".to_owned())
        }
    }
}

/// Returns `true` when `binder` names a kind of thing that can be
/// safely renamed within a package.
///
/// Refused:
/// - `Intrinsic` — built-in, no source span.
/// - `HostFn` — supplied by the host, not declared in
///   module source.
/// - Nominal members — `mk` and `get` are fixed declaration roles.
/// - Qualified variants — these are module-alias paths; the alias name
///   is local to the import site and renaming it would require
///   re-analyzing import scopes.
fn is_renameable(binder: &ResolvedBinder, _analysis: &LspAnalysis) -> bool {
    match binder {
        ResolvedBinder::BlockLabel { .. } => true,
        ResolvedBinder::Fn { .. } => true,
        ResolvedBinder::Local { .. } => true,
        ResolvedBinder::TypeParam { .. } => true,
        ResolvedBinder::TypeAlias { .. } => true,
        ResolvedBinder::Newtype { .. } => true,
        ResolvedBinder::NewtypeMember { .. } => false,
        // HostFn, Intrinsic, and qualified imports: refuse.
        ResolvedBinder::HostEnvFn { .. } => false,
        ResolvedBinder::HostType { .. } => false,
        ResolvedBinder::Intrinsic { .. } => false,
        ResolvedBinder::QualifiedImport { .. } => false,
        ResolvedBinder::QualifiedImportMember { .. } => false,
    }
}

fn block_label_name_conflicts(
    binder: &ResolvedBinder,
    new_name: &str,
    analysis: &LspAnalysis,
) -> bool {
    let ResolvedBinder::BlockLabel {
        module_path,
        macro_name,
        ordinal,
        ..
    } = binder
    else {
        return false;
    };
    analysis.position_index.binders_iter().any(|(_, other)| {
        matches!(other, ResolvedBinder::BlockLabel {
            module_path: owner, macro_name: head, ordinal: index, name, ..
        } if owner == module_path && head == macro_name && index != ordinal && name == new_name)
    })
}

/// Returns `true` when `new_name` appears in scope at `byte_offset`
/// inside `module` and resolves to something *other* than the binder
/// being renamed (`cursor_binder`).
///
/// The completed analysis's shared [`crate::pass::resolve::TopLevelScope`]
/// is authoritative for the full module namespace, including declarations
/// generated from surface labels. The Surface walk then adds lexical binders
/// and imports at this exact reference site. If the new name already exists,
/// we refuse rather than attempt a shadowing-preserving reorder.
fn name_conflicts_at_offset(
    module: &Module<Surface>,
    resolved_module_scope: Option<&crate::pass::resolve::TopLevelScope>,
    new_name: &str,
    byte_offset: u32,
    cursor_binder: &ResolvedBinder,
) -> bool {
    // Extract the "old name" that the cursor binder carries.
    let old_name = binder_name(cursor_binder);
    if old_name.as_deref() != Some(new_name)
        && resolved_module_scope.is_some_and(|scope| scope.lookup(new_name).is_some())
    {
        return true;
    }

    let mut in_scope: Vec<String> = Vec::new();
    collect_module_scope(module, byte_offset, &mut in_scope);

    for name in &in_scope {
        if name == new_name && Some(name.as_str()) != old_name.as_deref() {
            return true;
        }
    }
    false
}

/// Extract the user-visible single-segment name from a binder, if it
/// has one.
fn binder_name(binder: &ResolvedBinder) -> Option<String> {
    match binder {
        ResolvedBinder::BlockLabel { name, .. } => Some(name.clone()),
        ResolvedBinder::Fn { name, .. } => Some(name.clone()),
        ResolvedBinder::HostEnvFn { name, .. } => Some(name.clone()),
        ResolvedBinder::Local { name, .. } => Some(name.clone()),
        ResolvedBinder::TypeParam { name, .. } => Some(name.clone()),
        ResolvedBinder::TypeAlias { name, .. } | ResolvedBinder::HostType { name, .. } => {
            Some(name.clone())
        }
        ResolvedBinder::Newtype { name, .. } => Some(name.clone()),
        ResolvedBinder::NewtypeMember { member, .. } => Some(member.clone()),
        ResolvedBinder::Intrinsic { name } => Some(name.clone()),
        ResolvedBinder::QualifiedImport { alias } => Some(alias.clone()),
        ResolvedBinder::QualifiedImportMember { .. } => None,
    }
}

/// Collect all names in scope at `byte_offset` inside `module`.
///
/// Uses the same AST walk as the completion handler. The list includes
/// module-level names, imported names, and any fn-parameter /
/// `let`-bound names visible at the offset.
fn collect_module_scope(module: &Module<Surface>, byte_offset: u32, out: &mut Vec<String>) {
    // Inner scopes first (same pattern as completion).
    'items: for item in &module.items {
        match item {
            Item::FnDef(fn_def) => {
                let span = fn_def.meta.span;
                if span.start <= byte_offset && byte_offset <= span.end {
                    collect_fn_scope(fn_def, byte_offset, out);
                    break;
                }
            }
            Item::RecGroup(group, _) => {
                for member in &group.members {
                    let span = member.meta.span;
                    if span.start <= byte_offset && byte_offset <= span.end {
                        collect_fn_scope(member, byte_offset, out);
                        break 'items;
                    }
                }
            }
            _ => {}
        }
    }

    // Module-level names.
    for item in &module.items {
        let name = match item {
            Item::FnDef(f) => f.name.clone(),
            Item::TypeAlias(a) => a.name.clone(),
            Item::LiteralAlias(l, _) => l.name.clone(),
            Item::Newtype(n) => n.name.clone(),
            Item::Labels(t, _) => {
                out.extend(t.entries.iter().map(|entry| entry.name.clone()));
                if let Some(n) = &t.type_alias_name {
                    n.clone()
                } else {
                    continue;
                }
            }
            Item::Equiv(e, _) => e.name.clone(),
            Item::Macro(s, _) => s.name.clone(),
            Item::RecGroup(g, _) => {
                out.extend(g.members.iter().map(|member| member.name.clone()));
                continue;
            }
            Item::TypeRecGroup(group) => {
                for member in &group.members {
                    match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => out.push(alias.name.clone()),
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            out.push(newtype.name.clone())
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            out.extend(labels.type_alias_name.iter().cloned());
                            out.extend(labels.entries.iter().map(|entry| entry.name.clone()));
                        }
                    }
                }
                continue;
            }
            Item::HostType(h) => h.name.clone(),
            Item::HostFn(h) => h.name.clone(),
            Item::Op(_, _) | Item::VariadicOperator(_, _) => continue,
        };
        out.push(name);
    }

    // Imported names.
    for import_decl in &module.imports {
        match &import_decl.kind {
            ImportKind::Selective { items, .. } => {
                for item in items {
                    if let Some(name) = item.as_local_name() {
                        out.push(name.to_owned());
                    }
                }
            }
            ImportKind::Qualified { alias, .. } => {
                out.push(alias.clone());
            }
            ImportKind::Intrinsics => {}
            ImportKind::Comptime => {}
        }
    }
}

/// Collect names introduced by a top-level `FnDef` at `byte_offset`.
fn collect_fn_scope<PurityField>(
    fn_def: &FnDef<Surface, PurityField>,
    byte_offset: u32,
    out: &mut Vec<String>,
) {
    collect_expr_scope(&fn_def.body, byte_offset, out);
    add_sig_params(&fn_def.sig, out);
}

/// Collect names introduced by expressions at `byte_offset`.
fn collect_expr_scope(expr: &Expr<Surface>, byte_offset: u32, out: &mut Vec<String>) {
    match expr {
        Expr::BlockCall { prefix, blocks, .. } => {
            for arg in prefix.iter().flat_map(|stage| &stage.args) {
                if let crate::ast::CallArg::Value(value) = arg {
                    collect_expr_scope(value, byte_offset, out);
                }
            }
            for block in blocks {
                if !(block.open.end <= byte_offset && byte_offset <= block.close.end) {
                    continue;
                }
                for item in &block.items {
                    collect_expr_scope(item.value(), byte_offset, out);
                    if item.span().end > byte_offset {
                        continue;
                    }
                    match item {
                        crate::ast::NeutralItem::Binding { name, pattern, .. }
                        | crate::ast::NeutralItem::ExistentialBinding { name, pattern, .. } => {
                            if let Some(pattern) = pattern {
                                collect_pattern_names(pattern, out);
                            } else if name != "_" {
                                out.push(name.clone());
                            }
                        }
                        crate::ast::NeutralItem::Expression { .. } => {}
                    }
                }
            }
        }
        Expr::Let {
            name,
            value,
            body,
            meta,
            ..
        } => {
            let span = meta.span;
            if !(span.start <= byte_offset && byte_offset <= span.end) {
                return;
            }
            let in_body = body.span().start <= byte_offset && byte_offset <= body.span().end;
            if in_body {
                collect_expr_scope(body, byte_offset, out);
                if name != "_" {
                    out.push(name.clone());
                }
            } else {
                collect_expr_scope(value, byte_offset, out);
            }
        }
        Expr::Seq {
            value, body, meta, ..
        } => {
            let span = meta.span;
            if !(span.start <= byte_offset && byte_offset <= span.end) {
                return;
            }
            let in_body = body.span().start <= byte_offset && byte_offset <= body.span().end;
            if in_body {
                collect_expr_scope(body, byte_offset, out);
            } else {
                collect_expr_scope(value, byte_offset, out);
            }
        }
        Expr::FnExpr {
            sig, body, meta, ..
        } => {
            let span = meta.span;
            if !(span.start <= byte_offset && byte_offset <= span.end) {
                return;
            }
            collect_expr_scope(body, byte_offset, out);
            add_sig_params(sig, out);
        }
        Expr::Call {
            callee, args, meta, ..
        } => {
            let span = meta.span;
            if !(span.start <= byte_offset && byte_offset <= span.end) {
                return;
            }
            if callee.span().start <= byte_offset && byte_offset <= callee.span().end {
                collect_expr_scope(callee, byte_offset, out);
            } else {
                for arg in args {
                    let arg_span = arg.meta().span;
                    if arg_span.start <= byte_offset && byte_offset <= arg_span.end {
                        if let crate::ast::CallArg::Value(e) = arg {
                            collect_expr_scope(e, byte_offset, out);
                        }
                        break;
                    }
                }
            }
        }
        _ => {}
    }
}

fn collect_pattern_names(pattern: &crate::ast::ParamPattern, out: &mut Vec<String>) {
    for elem in &pattern.elems {
        match elem {
            crate::ast::ParamPatternElem::Bind { name, .. } => {
                if name != "_" {
                    out.push(name.clone());
                }
            }
            crate::ast::ParamPatternElem::Tuple(inner) => collect_pattern_names(inner, out),
            crate::ast::ParamPatternElem::BindTuple { name, inner, .. } => {
                if name != "_" {
                    out.push(name.clone());
                }
                collect_pattern_names(inner, out);
            }
        }
    }
}

/// Add value-parameter names from `sig` into `out`.
fn add_sig_params(sig: &Signature<Surface>, out: &mut Vec<String>) {
    for param in &sig.params {
        if let SignatureParam::Value(p) = param
            && p.name != "_"
        {
            out.push(p.name.clone());
        }
    }
}

/// A family rename changes only the written stem, preserving every slot index.
/// Reclassifying the edited source tree checks both directions of capture:
/// renamed references must remain owned and ordinary references must stay ordinary.
struct PlaceholderRename {
    stem: crate::ast::PathSegment,
    cursor_stem: Span,
    owner: Span,
    body: Expr<Surface>,
    references: Vec<(Span, u32)>,
}

impl PlaceholderRename {
    fn rename(mut self, new_name: &str, line_index: &LineIndex) -> Result<Vec<TextEdit>, String> {
        validate_new_name_for_binder(
            new_name,
            &ResolvedBinder::Local {
                name: self.stem.name.clone(),
                decl_span: Some(self.stem.span),
            },
        )?;
        if !new_name
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        {
            return Err("a placeholder stem must end in a letter".to_owned());
        }
        let owned = crate::pass::placeholder::classify(&self.stem.name, &mut self.body, self.owner)
            .map_err(|_| "cannot rename an invalid placeholder family".to_owned())?;
        for (reference, slot) in owned.occurrences {
            reference.name = format!("{new_name}{slot}");
        }
        let proposed = crate::pass::placeholder::classify(new_name, &mut self.body, self.owner)
            .map(|owned| {
                owned
                    .occurrences
                    .into_iter()
                    .map(|(reference, slot)| (reference.span, slot))
                    .collect::<Vec<_>>()
            });
        if proposed.as_ref().ok() != Some(&self.references) {
            return Err(format!(
                "rename to `{new_name}` would change placeholder ownership or arity"
            ));
        }
        let mut edits = vec![TextEdit {
            range: to_lsp_range(line_index.to_range(self.stem.span)),
            new_text: new_name.to_owned(),
        }];
        edits.extend(self.references.iter().map(|(span, _)| TextEdit {
            range: to_lsp_range(line_index.to_range(Span::new(
                span.start,
                span.start + self.stem.name.len() as u32,
            ))),
            new_text: new_name.to_owned(),
        }));
        Ok(edits)
    }
}

fn placeholder_family_at(module: &Module<Surface>, offset: u32) -> Option<PlaceholderRename> {
    let mut owners = Vec::new();
    let mut visit = |expr: &Expr<Surface>| {
        if let Expr::FnPlaceholder {
            stem, body, meta, ..
        } = expr
            && meta.span.start <= offset
            && offset <= meta.span.end
        {
            owners.push((stem.clone(), body.as_ref().clone(), meta.span));
        }
        if let Expr::Let {
            pattern: Some(pattern),
            body,
            ..
        } = expr
            && let Some(numbered) = &pattern.numbered
            && ((pattern.span.start <= offset && offset <= pattern.span.end)
                || (body.span().start <= offset && offset <= body.span().end))
        {
            owners.push((numbered.stem.clone(), body.as_ref().clone(), pattern.span));
        }
        if let Expr::BlockCall { blocks, .. } = expr {
            for block in blocks {
                for (index, item) in block.items.iter().enumerate() {
                    if let crate::ast::NeutralItem::Binding {
                        pattern: Some(pattern),
                        meta,
                        ..
                    } = item
                        && let Some(numbered) = &pattern.numbered
                        && ((pattern.span.start <= offset && offset <= pattern.span.end)
                            || (meta.span.end <= offset && offset <= block.close.start))
                    {
                        // Reuse the neutral-block ownership walk with this owner's
                        // exact lexical suffix; its initializer is outside scope.
                        let mut continuation = expr.clone();
                        let Expr::BlockCall { prefix, blocks, .. } = &mut continuation else {
                            unreachable!("cloned neutral block call")
                        };
                        prefix.clear();
                        let mut tail = block.clone();
                        tail.items = block.items[index + 1..].to_vec();
                        *blocks = vec![tail];
                        owners.push((numbered.stem.clone(), continuation, pattern.span));
                    }
                }
            }
        }
    };
    for item in &module.items {
        match item {
            Item::FnDef(def) => walk_rename_expr(&def.body, &mut visit),
            Item::RecGroup(group, _) => {
                for member in &group.members {
                    walk_rename_expr(&member.body, &mut visit);
                }
            }
            Item::Equiv(equiv, _) => {
                for arm in &equiv.terms {
                    walk_rename_expr(&arm.body, &mut visit);
                }
            }
            _ => {}
        }
    }
    owners
        .into_iter()
        .rev()
        .find_map(|(stem, mut body, owner)| {
            let references = crate::pass::placeholder::classify(&stem.name, &mut body, owner)
                .ok()?
                .occurrences
                .into_iter()
                .map(|(reference, slot)| (reference.span, slot))
                .collect::<Vec<_>>();
            let contains = |span: Span| span.start <= offset && offset <= span.end;
            let cursor_stem = if contains(stem.span) {
                stem.span
            } else {
                let (span, _) = references.iter().find(|(span, _)| contains(*span))?;
                Span::new(span.start, span.start + stem.name.len() as u32)
            };
            Some(PlaceholderRename {
                stem,
                cursor_stem,
                body,
                owner,
                references,
            })
        })
}

fn parse_surface_module(source: &str) -> Option<Module<Surface>> {
    crate::pass::parser::parse_module_file(source)
        .map(|module_file| module_file.module)
        .ok()
}

fn walk_rename_expr<'a>(expr: &'a Expr<Surface>, visit: &mut impl FnMut(&'a Expr<Surface>)) {
    visit(expr);
    match expr {
        Expr::BlockCall { prefix, blocks, .. } => {
            for arg in prefix.iter().flat_map(|stage| &stage.args) {
                if let crate::ast::CallArg::Value(value) = arg {
                    walk_rename_expr(value, visit);
                }
            }
            for block in blocks {
                for item in &block.items {
                    walk_rename_expr(item.value(), visit);
                }
            }
        }
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            walk_rename_expr(value, visit);
            walk_rename_expr(body, visit);
        }
        Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => {
            walk_rename_expr(body, visit);
        }
        Expr::Call { callee, args, .. } => {
            walk_rename_expr(callee, visit);
            for arg in args {
                if let crate::ast::CallArg::Value(value) = arg {
                    walk_rename_expr(value, visit);
                }
            }
        }
        Expr::Tuple { items, .. } => {
            for item in items {
                walk_rename_expr(item, visit);
            }
        }
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserMacro { stages, .. } | Expr::RecCall { stages, .. } => {
            for arg in stages.iter().flat_map(|stage| &stage.args) {
                if let crate::ast::CallArg::Value(value) = arg {
                    walk_rename_expr(value, visit);
                }
            }
        }
        Expr::Ufcs {
            receiver,
            callee,
            args,
            ..
        } => {
            walk_rename_expr(receiver, visit);
            walk_rename_expr(callee, visit);
            for arg in args {
                if let crate::ast::CallArg::Value(value) = arg {
                    walk_rename_expr(value, visit);
                }
            }
        }
        Expr::OpChain { kind, .. } => match kind {
            crate::ast::OpChainKind::Normal { slots, .. } => {
                for slot in slots {
                    walk_rename_expr(slot, visit);
                }
            }
            crate::ast::OpChainKind::Variadic { elements, .. } => {
                for element in elements {
                    walk_rename_expr(element, visit);
                }
            }
        },
        _ => {}
    }
}

/// Extract the source text covered by `span`.
fn span_text(source: &str, span: Span) -> &str {
    let start = span.start as usize;
    let end = span.end as usize;
    source.get(start..end).unwrap_or("")
}

/// Convert a `crate::lsp::positions::LspRange` to an `lsp_types::Range`.
fn to_lsp_range(r: crate::lsp::positions::LspRange) -> Range {
    Range {
        start: Position {
            line: r.start.line,
            character: r.start.character,
        },
        end: Position {
            line: r.end.line,
            character: r.end.character,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::check::LspAnalysis;
    use crate::lsp::util::{test_file_path, test_file_uri};
    use crate::pass::typecheck_full::{PositionIndex, ResolvedBinder};
    use crate::span::Span;
    use std::collections::BTreeMap;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn selective_alias_tooling_keeps_local_and_provider_identities_in_full_and_focused_analysis() {
        let root = test_file_path("/virtual/selective-alias-tooling");
        let provider = "module provider; import __comptime__; pub fn foo() { () } host fn external(); host type Int role(i32); pub type { Box: . { pub mk; pub get }; } pub type { Foo: .; }; pub literal token = 1; pure fn identity_impl(_ct: __Comptime__, _source: __Type__, value: __Checked_term__) -> __Checked_term__ { value } pub macro identity : [Source] Source -> Source { impl identity_impl }";
        let source = "module main; import provider(foo as foo, foo as local, external as invoke, Box as Parcel, Int as Number, token as count, macro identity as preserve); type Capture[B] = Number; fn bar() { () } pub fn run() { local(); foo(); invoke(); preserve(()); Parcel.get(Parcel.mk(())) } fn numeral() -> Number { count[Number] }";
        let main_file = root.join("main.kio");
        let provider_file = root.join("provider.kio");
        let overlay = crate::package_collection::SourceOverlay::complete(
            root.clone(),
            BTreeMap::from([
                (
                    root.join("demo.pkg.kio"),
                    "package demo; bridge { main; provider; }".to_owned(),
                ),
                (main_file.clone(), source.to_owned()),
                (provider_file.clone(), provider.to_owned()),
            ]),
        );
        let full = crate::cmd::check::analyze_workspace_at_with_overlay_lsp(&root, &overlay)
            .expect("full aliases");
        let focused =
            crate::cmd::check::analyze_module_at_with_overlay_lsp(&root, &overlay, &main_file)
                .expect("focused aliases");
        let uri = path_to_uri(&main_file, &root).unwrap();
        let provider_uri = path_to_uri(&provider_file, &root).unwrap();
        for analysis in [&full, &focused] {
            for (use_site, new_name, count) in [
                ("local()", "renamed", 2),
                ("foo()", "renamed", 2),
                ("invoke()", "call", 2),
                ("Parcel.get", "Package", 3),
                ("preserve", "retain", 2),
            ] {
                let position = position_of(source, use_site);
                let references = crate::lsp::references::handle_references(
                    &uri, &position, true, analysis, None, &root,
                )
                .unwrap();
                assert_eq!(references.len(), count, "{use_site}");
                assert!(references.iter().all(|location| location.uri == uri));
                let edit = handle_rename(&uri, &position, new_name, analysis, None, &root)
                    .unwrap()
                    .unwrap();
                let changes = edit.changes.unwrap();
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[&uri].len(), count, "{use_site}");
                let definition = crate::lsp::definition::handle_definition(
                    &uri, &position, analysis, None, &root,
                )
                .unwrap();
                let lsp_types::GotoDefinitionResponse::Scalar(location) = definition else {
                    panic!("one provider definition")
                };
                assert_eq!(location.uri, provider_uri);
            }
            for (source_site, destination_site, use_site) in [
                ("foo as foo", "foo, foo as local", "foo()"),
                ("foo as local", "local,", "local()"),
                ("external as invoke", "invoke,", "invoke()"),
                ("Box as Parcel", "Parcel,", "Parcel.get"),
                ("Int as Number", "Number,", "Number;"),
                ("token as count", "count,", "count[Number]"),
                ("identity as preserve", "preserve)", "preserve"),
            ] {
                let definitions: Vec<_> = [source_site, destination_site, use_site]
                    .into_iter()
                    .map(|site| {
                        let definition = crate::lsp::definition::handle_definition(
                            &uri,
                            &position_of(source, site),
                            analysis,
                            None,
                            &root,
                        )
                        .unwrap();
                        let lsp_types::GotoDefinitionResponse::Scalar(location) = definition else {
                            panic!("one provider definition for {site}")
                        };
                        assert_eq!(location.uri, provider_uri);
                        location
                    })
                    .collect();
                assert_eq!(definitions[0], definitions[1]);
                assert_eq!(definitions[0], definitions[2]);
                if source_site.starts_with("foo as ") {
                    assert_eq!(
                        definitions[0].range.start.character,
                        (provider.find("fn foo").unwrap() + "fn ".len()) as u32
                    );
                }
            }
            assert!(
                handle_rename(
                    &uri,
                    &position_of(source, "Number;"),
                    "B",
                    analysis,
                    None,
                    &root
                )
                .unwrap_err()
                .contains("conflict")
            );
            let literal_position = position_of(source, "count[Number]");
            let definition = crate::lsp::definition::handle_definition(
                &uri,
                &literal_position,
                analysis,
                None,
                &root,
            )
            .unwrap();
            let lsp_types::GotoDefinitionResponse::Scalar(location) = definition else {
                panic!("literal definition")
            };
            assert_eq!(location.uri, provider_uri);
            assert_eq!(
                location.range.start.character,
                (provider.find("literal token").unwrap() + "literal ".len()) as u32
            );
        }
        let edit = handle_rename(
            &provider_uri,
            &position_of(provider, "foo()"),
            "bar",
            &full,
            None,
            &root,
        )
        .expect("consumer bar does not collide with a provider selector")
        .unwrap();
        let changes = edit.changes.unwrap();
        assert_eq!(changes[&provider_uri].len(), 1);
        assert_eq!(changes[&uri].len(), 2);
        assert!(
            changes[&uri].iter().all(
                |edit| edit.range.start.character < source.find("type Capture").unwrap() as u32
            )
        );
    }

    #[test]
    fn placeholder_family_rename_from_intro_or_reference_preserves_indices_and_controls() {
        let source = "module main; fn run() { .x. { (x2, x1, m.x1, .x. { x1 }, \
            .(x1) { x1 }, .() { let x1 = (); x1 }, .() { x1 }) } }";
        let file = test_file_path("/virtual/main.kio");
        let uri = test_file_uri("/virtual/main.kio");
        let analysis = make_package_analysis(vec![(file, "main".into(), source.into())], vec![]);
        let module = parse_surface_module(source).unwrap();
        for offset in [
            source.find(".x.").unwrap() + 1,
            source.find("x2").unwrap() + 1,
        ] {
            let position = Position {
                line: 0,
                character: offset as u32,
            };
            let prepared =
                handle_prepare_rename_with_module(&uri, &position, &analysis, None, Some(&module))
                    .unwrap();
            let PrepareRenameResponse::RangeWithPlaceholder { range, placeholder } = prepared
            else {
                panic!("stem range")
            };
            assert_eq!(placeholder, "x");
            assert_eq!(range.end.character - range.start.character, 1);
            let edit = handle_rename(
                &uri,
                &position,
                "arg",
                &analysis,
                None,
                &test_file_path("/virtual"),
            )
            .unwrap()
            .unwrap();
            let mut edits = edit.changes.unwrap().remove(&uri).unwrap();
            assert_eq!(edits.len(), 4, "intro plus exactly three owned occurrences");
            edits.sort_by_key(|edit| std::cmp::Reverse(edit.range.start.character));
            let mut changed = source.to_owned();
            for edit in edits {
                changed.replace_range(
                    edit.range.start.character as usize..edit.range.end.character as usize,
                    &edit.new_text,
                );
            }
            assert_eq!(
                changed,
                "module main; fn run() { .arg. { (arg2, arg1, m.x1, .x. { x1 }, \
                .(x1) { x1 }, .() { let x1 = (); x1 }, .() { arg1 }) } }"
            );
            assert!(parse_surface_module(&changed).is_some());
        }
    }

    #[test]
    fn placeholder_family_rename_refuses_capture_or_invalid_stem() {
        for source in [
            "module main; fn run() { .x. { .(arg1) { x1 } } }",
            "module main; fn run() { .x. { (x1, arg2) } }",
            "module main; fn run() { .x. { (x1, arg01) } }",
        ] {
            let module = parse_surface_module(source).unwrap();
            let family =
                placeholder_family_at(&module, source.find(".x.").unwrap() as u32 + 1).unwrap();
            let error = family.rename("arg", &LineIndex::new(source)).unwrap_err();
            assert!(error.contains("ownership or arity"), "{error}");
        }
        let source = "module main; fn run() { .x. { x1 } }";
        let module = parse_surface_module(source).unwrap();
        for name in ["arg1", "arg_", "Arg", "__arg"] {
            let family = placeholder_family_at(&module, source.find("x1").unwrap() as u32).unwrap();
            assert!(
                family.rename(name, &LineIndex::new(source)).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn placeholder_typed_navigation_keeps_numbered_slots_distinct() {
        let root = test_file_path("/virtual/placeholder-tooling");
        for (source, slots) in [
            (
                "module main; pub fn run() -> ((. -> .) & .) -> . { \
                .x. { let keep = x1; x2 } }",
                2,
            ),
            (
                "module main; pub fn run(value: (. -> .) & .) -> . { \
                let .x. = value; let keep = x1; x2 }",
                2,
            ),
            (
                "module main; import sequence(macro do); fn bind[A][B](value: A, next: A -> B) -> B { next(value) } \
                pub fn run(value: (. -> .) & .) -> . { do(bind) { let .x. <- value; let keep = x1; x2 } }",
                2,
            ),
            (
                "module main; pub fn run(value: (. -> .) & .) -> . { \
                .() { let .x. = value; let keep = x1; x2 }() }",
                2,
            ),
            (
                "module main; pub fn run(value: . -> .) -> . { \
                .() { let .x. = value; x1() }() }",
                1,
            ),
        ] {
            let files = BTreeMap::from([
                (
                    root.join("demo.pkg.kio"),
                    "package demo; bridge { main; }".to_owned(),
                ),
                (root.join("main.kio"), source.to_owned()),
                (
                    root.join("sequence.kio"),
                    include_str!("../../../test-data/poc/macros/workdir/sequence.kio").to_owned(),
                ),
            ]);
            let overlay = crate::package_collection::SourceOverlay::complete(root.clone(), files);
            let analysis =
                crate::cmd::check::analyze_workspace_at_with_overlay_lsp(&root, &overlay)
                    .expect("typed placeholder source");
            let uri = path_to_uri(&root.join("main.kio"), &root).unwrap();
            let stem = source.find(".x.").unwrap() as u32 + 1;
            let stem_position = Position::new(0, stem);
            let stem_span = Span::new(stem, stem + 1);
            assert!(
                analysis
                    .position_index
                    .binder_at("main", stem_span)
                    .is_none()
            );
            assert!(analysis.position_index.type_at("main", stem_span).is_none());
            assert!(
                crate::lsp::definition::handle_definition(
                    &uri,
                    &stem_position,
                    &analysis,
                    None,
                    &root
                )
                .is_none()
            );
            assert!(
                crate::lsp::references::handle_references(
                    &uri,
                    &stem_position,
                    true,
                    &analysis,
                    None,
                    &root
                )
                .is_none()
            );
            let stem_hover = crate::lsp::hover::handle_hover(&uri, &stem_position, &analysis, None);
            assert!(
                stem_hover.is_none(),
                "the family stem has no individual slot type: {source}: {stem_hover:?}"
            );
            let renamed = handle_rename(&uri, &stem_position, "arg", &analysis, None, &root)
                .unwrap()
                .unwrap();
            assert_eq!(renamed.changes.unwrap()[&uri].len(), slots + 1);
            for name in &["x1", "x2"][..slots] {
                let position = position_of(source, name);
                let definition = crate::lsp::definition::handle_definition(
                    &uri, &position, &analysis, None, &root,
                )
                .unwrap();
                let lsp_types::GotoDefinitionResponse::Scalar(location) = definition else {
                    panic!("one stem declaration")
                };
                assert_eq!(location.range.start.character, stem);
                assert_eq!(location.range.end.character, stem + 1);
                let references = crate::lsp::references::handle_references(
                    &uri, &position, false, &analysis, None, &root,
                )
                .unwrap();
                assert_eq!(
                    references.len(),
                    1,
                    "numbered slots retain separate binding identities"
                );
                assert_eq!(references[0].range.start, position);
                let with_declaration = crate::lsp::references::handle_references(
                    &uri, &position, true, &analysis, None, &root,
                )
                .unwrap();
                assert_eq!(
                    with_declaration, references,
                    "the family stem is not a declaration occurrence of one numbered slot"
                );
                let hover =
                    crate::lsp::hover::handle_hover(&uri, &position, &analysis, None).unwrap();
                let lsp_types::HoverContents::Markup(content) = hover.contents else {
                    panic!("type hover")
                };
                assert_eq!(
                    content.value.contains("->"),
                    *name == "x1",
                    "{name}: {}",
                    content.value
                );
                let renamed = handle_rename(&uri, &position, "arg", &analysis, None, &root)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    renamed.changes.unwrap()[&uri].len(),
                    slots + 1,
                    "typed rename still targets the family"
                );
            }
        }
    }

    fn make_analysis(entries: Vec<(&str, Span, ResolvedBinder, &str)>) -> LspAnalysis {
        let mut index = PositionIndex::new();
        let mut file_to_module = BTreeMap::new();
        let mut sources = HashMap::new();
        for (module_path, span, binder, source) in entries {
            index.record_binder(module_path, span, binder);
            let file_path = test_file_path(format!("/tmp/{module_path}.kio"));
            file_to_module
                .entry(file_path.clone())
                .or_insert_with(|| module_path.to_owned());
            sources
                .entry(file_path)
                .or_insert_with(|| source.to_owned());
        }
        LspAnalysis {
            position_index: index,
            file_to_module,
            import_aliases: Default::default(),
            sources,
            root_package_lowered: crate::pass::resolve::Package::from_parts(
                std::collections::BTreeMap::new(),
                None,
            ),
            warnings: Vec::new(),
        }
    }

    fn make_package_analysis(
        files: Vec<(PathBuf, String, String)>,
        binders: Vec<(String, Span, ResolvedBinder)>,
    ) -> LspAnalysis {
        let mut position_index = PositionIndex::new();
        for (module_path, span, binder) in binders {
            position_index.record_binder(&module_path, span, binder);
        }

        let mut file_to_module = BTreeMap::new();
        let mut sources = HashMap::new();
        for (file_path, module_path, source) in files {
            let file_path = crate::package_collection::canonicalize_with_missing_suffix(&file_path)
                .unwrap_or(file_path);
            file_to_module.insert(file_path.clone(), module_path);
            sources.insert(file_path, source);
        }

        LspAnalysis {
            position_index,
            file_to_module,
            import_aliases: Default::default(),
            sources,
            root_package_lowered: crate::pass::resolve::Package::from_parts(BTreeMap::new(), None),
            warnings: Vec::new(),
        }
    }

    fn span_of(source: &str, needle: &str) -> Span {
        let start = source.find(needle).expect("fixture contains span") as u32;
        Span::new(start, start + needle.len() as u32)
    }

    fn span_of_nth(source: &str, needle: &str, occurrence: usize) -> Span {
        let start = source
            .match_indices(needle)
            .nth(occurrence)
            .expect("fixture contains requested span")
            .0 as u32;
        Span::new(start, start + needle.len() as u32)
    }

    fn position_of(source: &str, needle: &str) -> Position {
        let offset = source.find(needle).expect("fixture contains position") as u32;
        let position = LineIndex::new(source).to_position(offset);
        Position {
            line: position.line,
            character: position.character,
        }
    }

    // --- validate_new_name tests ---

    #[test]
    fn validate_name_accepts_simple_identifier() {
        assert!(validate_new_name("foo").is_ok());
        assert!(validate_new_name("bar_baz").is_ok());
        assert!(validate_new_name("_private").is_ok());
        assert!(validate_new_name("CamelCase").is_ok());
    }

    #[test]
    fn rename_validation_preserves_the_resolved_namespace() {
        for type_binder in [
            ResolvedBinder::TypeParam {
                name: "A".to_owned(),
                decl_span: None,
            },
            ResolvedBinder::Newtype {
                module_path: "m".to_owned(),
                name: "Box".to_owned(),
            },
        ] {
            for name in ["A", "_A", "Foo", "_Foo_bar"] {
                assert!(
                    validate_new_name_for_binder(name, &type_binder).is_ok(),
                    "{name}"
                );
            }
            for name in ["_value", "_1Foo", "FooBar", "_FooBar", "__A", "_"] {
                assert!(
                    validate_new_name_for_binder(name, &type_binder).is_err(),
                    "{name}"
                );
            }
        }

        let value_binder = ResolvedBinder::Local {
            name: "value".to_owned(),
            decl_span: None,
        };
        assert!(validate_new_name_for_binder("value", &value_binder).is_ok());
        assert!(validate_new_name_for_binder("_value", &value_binder).is_ok());
        assert!(validate_new_name_for_binder("_Value", &value_binder).is_err());
    }

    #[test]
    fn validate_name_rejects_empty() {
        assert!(validate_new_name("").is_err());
    }

    #[test]
    fn validate_name_rejects_operator_string() {
        assert!(validate_new_name("+").is_err());
        assert!(validate_new_name("->").is_err());
    }

    #[test]
    fn validate_name_rejects_number() {
        assert!(validate_new_name("42").is_err());
    }

    #[test]
    fn validate_name_rejects_reserved_slot_token() {
        // Pure underscore runs of length 4+ are rejected by the lexer.
        assert!(validate_new_name("____").is_err());
    }

    #[test]
    fn validate_name_accepts_contextual_keywords() {
        // `fn`, `let`, `module` etc. lex as Ident in Kio — they're
        // contextual keywords, not hard reserved words.
        assert!(validate_new_name("fn").is_ok());
        assert!(validate_new_name("let").is_ok());
        assert!(validate_new_name("module").is_ok());
    }

    #[test]
    fn rename_conflict_scope_includes_all_rec_members_and_current_params() {
        let source = "module pkg/main;\n\
rec(loop) {\n\
  fn first(x: .) -> . { recur second(x) };\n\
  fn second(y: .) -> . { recur first(y) }\n\
}\n";
        let module = crate::pass::parser::parse(source).expect("fixture parses");
        let offset = source.find("recur first(").expect("marker") as u32;
        let mut names = Vec::new();
        collect_module_scope(&module, offset, &mut names);

        for expected in ["first", "second", "y"] {
            assert!(names.iter().any(|name| name == expected), "got: {names:?}");
        }
    }

    // --- prepare_rename tests ---

    #[test]
    fn ordinary_rename_conflict_scope_includes_nominal_and_alias_names() {
        let source = "module source; type { Foo: .; }; type Field = Foo; fn keep() -> . { () }";
        let module = crate::pass::parser::parse(source).expect("alias source");
        let mut names = Vec::new();
        collect_module_scope(&module, source.len() as u32, &mut names);
        assert_eq!(names, ["Foo", "Field", "keep"]);

        let source = format!("{source} fn field() -> . {{ () }}");
        let module = crate::pass::parser::parse(&source).expect("a distinct ordinary value");
        let mut names = Vec::new();
        collect_module_scope(&module, source.len() as u32, &mut names);
        assert_eq!(names, ["Foo", "Field", "keep", "field"]);
    }

    #[test]
    fn nominal_names_are_renameable_and_fixed_members_are_not() {
        let analysis = make_analysis(Vec::new());
        let ordinary_head = ResolvedBinder::Newtype {
            module_path: "main".to_owned(),
            name: "Box".to_owned(),
        };
        let ordinary_member = ResolvedBinder::NewtypeMember {
            module_path: "main".to_owned(),
            newtype: "Box".to_owned(),
            member: "mk".to_owned(),
        };
        assert!(is_renameable(&ordinary_head, &analysis));
        assert!(!is_renameable(&ordinary_member, &analysis));
    }

    #[test]
    fn prepare_rename_returns_none_outside_binder() {
        let source = "module pkg/main;\n";
        let analysis = make_analysis(vec![(
            "pkg/main",
            Span::new(20, 30),
            ResolvedBinder::Fn {
                module_path: "pkg/main".to_owned(),
                name: "run".to_owned(),
            },
            source,
        )]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let pos = Position {
            line: 0,
            character: 0,
        };
        let result = handle_prepare_rename(&uri, &pos, &analysis, None);
        assert!(result.is_none(), "no binder at cursor → None");
    }

    #[test]
    fn prepare_rename_returns_range_on_fn_binder() {
        // Source: "module pkg/main;\n  run  "
        // Byte offsets:  "module pkg/main;\n" = 17 bytes (indices 0..16, \n at 16).
        // Then: space=17, space=18, r=19, u=20, n=21 → "run" is span 19..22.
        let source = "module pkg/main;\n  run  ";
        let analysis = make_analysis(vec![(
            "pkg/main",
            Span::new(19, 22),
            ResolvedBinder::Fn {
                module_path: "pkg/main".to_owned(),
                name: "run".to_owned(),
            },
            source,
        )]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        // Cursor on line 1, char 2 → byte 19, inside span 19..22.
        let pos = Position {
            line: 1,
            character: 2,
        };
        let result = handle_prepare_rename(&uri, &pos, &analysis, None);
        assert!(result.is_some(), "cursor on Fn binder → Some");
        if let Some(PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. }) = result {
            assert_eq!(placeholder, "run");
        }
    }

    #[test]
    fn prepare_rename_returns_none_for_intrinsic() {
        // "module pkg/main;\n" = 17 bytes, then "  " = 2 bytes → __pair__ starts at 19.
        // "__pair__" is 11 chars → span 19..30.
        let source = "module pkg/main;\n  __pair__  ";
        let analysis = make_analysis(vec![(
            "pkg/main",
            Span::new(19, 30),
            ResolvedBinder::Intrinsic {
                name: "__pair__".to_owned(),
            },
            source,
        )]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let pos = Position {
            line: 1,
            character: 3,
        };
        let result = handle_prepare_rename(&uri, &pos, &analysis, None);
        assert!(result.is_none(), "intrinsic is not renameable → None");
    }

    // --- handle_rename tests ---

    #[test]
    fn rename_returns_none_outside_binder() {
        let source = "module pkg/main;\n";
        let analysis = make_analysis(vec![(
            "pkg/main",
            Span::new(20, 30),
            ResolvedBinder::Fn {
                module_path: "pkg/main".to_owned(),
                name: "run".to_owned(),
            },
            source,
        )]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let pos = Position {
            line: 0,
            character: 0,
        };
        let result = handle_rename(
            &uri,
            &pos,
            "new_name",
            &analysis,
            None,
            &test_file_path("/tmp"),
        );
        // Outside any binder → error (no renameable identifier at cursor).
        assert!(result.is_err());
    }

    #[test]
    fn rename_invalid_name_returns_error() {
        // "module pkg/main;\n" = 17 bytes, "  " = 2 → run starts at 19.
        let source = "module pkg/main;\n  run  ";
        let analysis = make_analysis(vec![(
            "pkg/main",
            Span::new(19, 22),
            ResolvedBinder::Fn {
                module_path: "pkg/main".to_owned(),
                name: "run".to_owned(),
            },
            source,
        )]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let pos = Position {
            line: 1,
            character: 2,
        };
        let result = handle_rename(&uri, &pos, "____", &analysis, None, &test_file_path("/tmp"));
        assert!(result.is_err(), "____` is reserved → error");
    }

    #[test]
    fn rename_fn_binder_produces_workspace_edit() {
        // "module pkg/main;\n" = 17 bytes, "  " = 2 → run starts at 19.
        // "run" = 3 bytes → first span 19..22.
        // "  " = 2 bytes → second run starts at 24 → span 24..27.
        let source = "module pkg/main;\n  run  run  ";
        let analysis = make_analysis(vec![
            (
                "pkg/main",
                Span::new(19, 22),
                ResolvedBinder::Fn {
                    module_path: "pkg/main".to_owned(),
                    name: "run".to_owned(),
                },
                source,
            ),
            (
                "pkg/main",
                Span::new(24, 27),
                ResolvedBinder::Fn {
                    module_path: "pkg/main".to_owned(),
                    name: "run".to_owned(),
                },
                source,
            ),
        ]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let pos = Position {
            line: 1,
            character: 2,
        };
        let result = handle_rename(&uri, &pos, "exec", &analysis, None, &test_file_path("/tmp"));
        assert!(result.is_ok(), "rename should succeed: {:?}", result);
        let ws_edit = result.unwrap().expect("should return a WorkspaceEdit");
        let changes = ws_edit.changes.expect("changes must be present");
        // Should have edits for the pkg/main file.
        let total_edits: usize = changes.values().map(|v| v.len()).sum();
        assert_eq!(total_edits, 2, "both occurrences must be renamed");
    }

    #[test]
    fn qualified_function_rename_ignores_unrelated_local_binding() {
        let package_root = test_file_path("/workspace");
        let provider_path = package_root.join("provider.kio");
        let consumer_path = package_root.join("consumer.kio");
        let provider = "module provider;\npub fn combine(value: .) -> . { value }\n";
        let consumer = concat!(
            "module consumer;\n",
            "import provider as p;\n",
            "fn run(combined: .) -> . { p.combine(combined) }\n",
        );
        let target = ResolvedBinder::Fn {
            module_path: "provider".to_owned(),
            name: "combine".to_owned(),
        };
        let alias_start = consumer.find("p.combine").expect("qualified call");
        let member_start = alias_start + "p.".len();
        let analysis = make_package_analysis(
            vec![
                (provider_path, "provider".to_owned(), provider.to_owned()),
                (
                    consumer_path.clone(),
                    "consumer".to_owned(),
                    consumer.to_owned(),
                ),
            ],
            vec![
                (
                    "provider".to_owned(),
                    span_of(provider, "combine"),
                    target.clone(),
                ),
                (
                    "consumer".to_owned(),
                    Span::new(alias_start as u32, alias_start as u32 + 1),
                    ResolvedBinder::QualifiedImport {
                        alias: "p".to_owned(),
                    },
                ),
                (
                    "consumer".to_owned(),
                    Span::new(member_start as u32, (member_start + "combine".len()) as u32),
                    target,
                ),
            ],
        );
        let cursor = LineIndex::new(consumer).to_position(member_start as u32);
        let uri = path_to_uri(&consumer_path, &package_root).expect("consumer URI");

        let edit = handle_rename(
            &uri,
            &Position {
                line: cursor.line,
                character: cursor.character,
            },
            "combined",
            &analysis,
            None,
            &package_root,
        )
        .expect("a local cannot capture a qualified member")
        .expect("rename produces edits");

        assert_eq!(
            edit.changes
                .expect("plain workspace changes")
                .values()
                .map(Vec::len)
                .sum::<usize>(),
            2,
            "the provider declaration and qualified use are both renamed"
        );
    }

    #[test]
    fn fixed_nominal_member_refuses_rename_in_an_operator_binding() {
        let package_root = test_file_path("/workspace");
        let provider_path = package_root.join("provider.kio");
        let consumer_path = package_root.join("consumer.kio");
        let provider = concat!(
            "module provider;\n",
            "pub type { Wrap: . { pub mk; pub get; }; };\n",
        );
        let consumer = concat!(
            "module consumer;\n",
            "import provider(Wrap);\n",
            "fn wrapped(value: .) -> . { value }\n",
            "op + _ { impl Wrap.mk; };\n",
        );
        let target = ResolvedBinder::NewtypeMember {
            module_path: "provider".to_owned(),
            newtype: "Wrap".to_owned(),
            member: "mk".to_owned(),
        };
        let type_start = consumer.rfind("Wrap.mk").expect("member call");
        let member_start = type_start + "Wrap.".len();
        let analysis = make_package_analysis(
            vec![
                (provider_path, "provider".to_owned(), provider.to_owned()),
                (
                    consumer_path.clone(),
                    "consumer".to_owned(),
                    consumer.to_owned(),
                ),
            ],
            vec![
                (
                    "provider".to_owned(),
                    span_of(provider, "mk"),
                    target.clone(),
                ),
                (
                    "consumer".to_owned(),
                    Span::new(type_start as u32, (type_start + "Wrap".len()) as u32),
                    ResolvedBinder::Newtype {
                        module_path: "provider".to_owned(),
                        name: "Wrap".to_owned(),
                    },
                ),
                (
                    "consumer".to_owned(),
                    Span::new(member_start as u32, (member_start + "mk".len()) as u32),
                    target,
                ),
            ],
        );
        let cursor = LineIndex::new(consumer).to_position(member_start as u32);
        let uri = path_to_uri(&consumer_path, &package_root).expect("consumer URI");

        assert!(
            handle_prepare_rename(
                &uri,
                &Position::new(cursor.line, cursor.character),
                &analysis,
                None,
            )
            .is_none()
        );
        let error = handle_rename(
            &uri,
            &Position {
                line: cursor.line,
                character: cursor.character,
            },
            "wrapped",
            &analysis,
            None,
            &package_root,
        )
        .expect_err("a fixed nominal role cannot be renamed");
        assert_eq!(error, "the identifier at the cursor cannot be renamed");
    }

    #[test]
    fn unqualified_function_rename_keeps_local_binding_conflict() {
        let package_root = test_file_path("/workspace");
        let provider_path = package_root.join("provider.kio");
        let consumer_path = package_root.join("consumer.kio");
        let provider = "module provider;\npub fn combine(value: .) -> . { value }\n";
        let consumer = concat!(
            "module consumer;\n",
            "import provider(combine);\n",
            "fn run(combined: .) -> . { combine(combined) }\n",
        );
        let target = ResolvedBinder::Fn {
            module_path: "provider".to_owned(),
            name: "combine".to_owned(),
        };
        let import_start = consumer
            .find("import provider(combine")
            .expect("selective import")
            + "import provider(".len();
        let call_start = consumer.rfind("combine(").expect("unqualified call");
        let analysis = make_package_analysis(
            vec![
                (provider_path, "provider".to_owned(), provider.to_owned()),
                (
                    consumer_path.clone(),
                    "consumer".to_owned(),
                    consumer.to_owned(),
                ),
            ],
            vec![
                (
                    "provider".to_owned(),
                    span_of(provider, "combine"),
                    target.clone(),
                ),
                (
                    "consumer".to_owned(),
                    Span::new(import_start as u32, (import_start + "combine".len()) as u32),
                    target.clone(),
                ),
                (
                    "consumer".to_owned(),
                    Span::new(call_start as u32, (call_start + "combine".len()) as u32),
                    target,
                ),
            ],
        );
        let cursor = LineIndex::new(consumer).to_position(call_start as u32);
        let uri = path_to_uri(&consumer_path, &package_root).expect("consumer URI");

        let error = handle_rename(
            &uri,
            &Position {
                line: cursor.line,
                character: cursor.character,
            },
            "combined",
            &analysis,
            None,
            &package_root,
        )
        .expect_err("an unqualified reference would be captured by the local");
        assert!(error.contains("would conflict"), "got: {error}");
    }

    #[test]
    fn qualified_function_rename_keeps_provider_declaration_collision() {
        let package_root = test_file_path("/workspace");
        let provider_path = package_root.join("provider.kio");
        let consumer_path = package_root.join("consumer.kio");
        let provider = concat!(
            "module provider;\n",
            "pub fn combine(value: .) -> . { value }\n",
            "pub fn combined(value: .) -> . { value }\n",
        );
        let consumer = "module consumer;\nimport provider as p;\nfn run() -> . { p.combine(()) }\n";
        let target = ResolvedBinder::Fn {
            module_path: "provider".to_owned(),
            name: "combine".to_owned(),
        };
        let alias_start = consumer.find("p.combine").expect("qualified call");
        let member_start = alias_start + "p.".len();
        let analysis = make_package_analysis(
            vec![
                (provider_path, "provider".to_owned(), provider.to_owned()),
                (
                    consumer_path.clone(),
                    "consumer".to_owned(),
                    consumer.to_owned(),
                ),
            ],
            vec![
                (
                    "provider".to_owned(),
                    span_of(provider, "combine"),
                    target.clone(),
                ),
                (
                    "consumer".to_owned(),
                    Span::new(alias_start as u32, alias_start as u32 + 1),
                    ResolvedBinder::QualifiedImport {
                        alias: "p".to_owned(),
                    },
                ),
                (
                    "consumer".to_owned(),
                    Span::new(member_start as u32, (member_start + "combine".len()) as u32),
                    target,
                ),
            ],
        );
        let cursor = LineIndex::new(consumer).to_position(member_start as u32);
        let uri = path_to_uri(&consumer_path, &package_root).expect("consumer URI");

        let error = handle_rename(
            &uri,
            &Position {
                line: cursor.line,
                character: cursor.character,
            },
            "combined",
            &analysis,
            None,
            &package_root,
        )
        .expect_err("the provider already declares the requested name");
        assert!(error.contains("would conflict"), "got: {error}");
    }

    #[test]
    fn indexed_rename_survives_an_unparseable_live_overlay() {
        let analyzed_source = "module pkg/main;\n  run  run  ";
        let overlay_source = "module pkg/main;\n  run  run  \"";
        assert!(crate::pass::parser::parse_module_file(overlay_source).is_err());
        let binder = ResolvedBinder::Fn {
            module_path: "pkg/main".to_owned(),
            name: "run".to_owned(),
        };
        let analysis = make_analysis(vec![
            (
                "pkg/main",
                Span::new(19, 22),
                binder.clone(),
                analyzed_source,
            ),
            ("pkg/main", Span::new(24, 27), binder, analyzed_source),
        ]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let position = Position {
            line: 1,
            character: 2,
        };
        let overlay = Some(LiveSyntaxSnapshot {
            source: overlay_source,
            parsed: false,
        });

        assert!(
            handle_prepare_rename_with_module(&uri, &position, &analysis, overlay, None).is_some(),
            "indexed prepareRename does not require a fresh Surface AST",
        );
        let edit = handle_rename_versioned_with_context(
            &uri,
            &position,
            "exec",
            &analysis,
            overlay,
            RenameParseContext {
                package_root: &test_file_path("/tmp"),
                current_module: None,
            },
            &BTreeMap::new(),
        )
        .expect("indexed rename does not require a fresh Surface AST")
        .expect("indexed rename returns an edit");
        assert!(edit.document_changes.is_some());
    }

    #[test]
    fn rename_conflict_uses_written_grammar_from_analysis_snapshot() {
        let package_root = test_file_path("/workspace");
        let main_path = package_root.join("pkg/main.kio");
        let use_path = package_root.join("pkg/use.kio");
        let syntax_path = package_root.join("pkg/syntax.kio");
        let main_source = "module pkg/main;\nfn target() -> A { target() }\n";
        let use_source = concat!(
            "module pkg/use;\n",
            "import pkg/main(target);\n",
            "import pkg/syntax(op ? _ : _);\n",
            "fn run(taken: A) -> A { ? target() : taken }\n",
        );
        let syntax_source = concat!(
            "module pkg/syntax;\n",
            "fn choose(left: A, right: A) -> A { left }\n",
            "pub op ? _ : _ { impl choose; };\n",
        );

        assert!(
            crate::pass::parser::parse_module_file(use_source).is_ok(),
            "the written import grammar makes the fixed tail independently parseable",
        );

        let target_binder = ResolvedBinder::Fn {
            module_path: "pkg/main".to_owned(),
            name: "target".to_owned(),
        };
        let analysis = make_package_analysis(
            vec![
                (
                    main_path.clone(),
                    "pkg/main".to_owned(),
                    main_source.to_owned(),
                ),
                (use_path, "pkg/use".to_owned(), use_source.to_owned()),
                (
                    syntax_path.clone(),
                    "pkg/syntax".to_owned(),
                    syntax_source.to_owned(),
                ),
            ],
            vec![
                (
                    "pkg/main".to_owned(),
                    span_of(main_source, "target"),
                    target_binder.clone(),
                ),
                (
                    "pkg/use".to_owned(),
                    span_of_nth(use_source, "target", 1),
                    target_binder,
                ),
            ],
        );
        let current_module = crate::pass::parser::parse_module_file(main_source)
            .expect("cursor module parses")
            .module;
        let uri = test_file_uri("/workspace/pkg/main.kio");

        let error = handle_rename_versioned_with_context(
            &uri,
            &position_of(main_source, "target"),
            "taken",
            &analysis,
            None,
            RenameParseContext {
                package_root: &package_root,
                current_module: Some(&current_module),
            },
            &BTreeMap::new(),
        )
        .expect_err("the target reference is in a scope that already binds `taken`");
        assert!(error.contains("would conflict"), "got: {error}");
    }

    #[test]
    fn rename_conflict_ignores_provider_changes_after_analysis() {
        let directory = tempfile::tempdir().expect("temporary package root");
        let package_root = directory.path().to_path_buf();
        let package_dir = package_root.join("pkg");
        fs::create_dir_all(&package_dir).expect("package module directory");
        let main_path = package_dir.join("main.kio");
        let use_path = package_dir.join("use.kio");
        let syntax_path = package_dir.join("syntax.kio");
        let main_source = "module pkg/main;\nfn target() -> A { target() }\n";
        let use_source = concat!(
            "module pkg/use;\n",
            "import pkg/main(target);\n",
            "import pkg/syntax(op ? _ : _);\n",
            "fn run(taken: A) -> A { ? target() : taken }\n",
        );
        let analyzed_syntax_source = concat!(
            "module pkg/syntax;\n",
            "fn choose(left: A, right: A) -> A { left }\n",
            "pub op ? _ : _ { impl choose; };\n",
        );
        let changed_syntax_source = concat!(
            "module pkg/syntax;\n",
            "fn identity(value: A) -> A { value }\n",
            "pub op ? _ { impl identity; };\n",
        );
        fs::write(&syntax_path, changed_syntax_source).expect("post-analysis provider edit");

        let target_binder = ResolvedBinder::Fn {
            module_path: "pkg/main".to_owned(),
            name: "target".to_owned(),
        };
        let analysis = make_package_analysis(
            vec![
                (
                    main_path.clone(),
                    "pkg/main".to_owned(),
                    main_source.to_owned(),
                ),
                (use_path, "pkg/use".to_owned(), use_source.to_owned()),
                (
                    syntax_path,
                    "pkg/syntax".to_owned(),
                    analyzed_syntax_source.to_owned(),
                ),
            ],
            vec![
                (
                    "pkg/main".to_owned(),
                    span_of(main_source, "target"),
                    target_binder.clone(),
                ),
                (
                    "pkg/use".to_owned(),
                    span_of_nth(use_source, "target", 1),
                    target_binder,
                ),
            ],
        );
        let current_module = crate::pass::parser::parse_module_file(main_source)
            .expect("cursor module parses")
            .module;
        let uri = path_to_uri(&main_path, &package_root).expect("cursor module URI");

        let error = handle_rename_versioned_with_context(
            &uri,
            &position_of(main_source, "target"),
            "taken",
            &analysis,
            None,
            RenameParseContext {
                package_root: &package_root,
                current_module: Some(&current_module),
            },
            &BTreeMap::new(),
        )
        .expect_err("post-analysis provider edits cannot change a versioned rename decision");
        assert!(error.contains("would conflict"), "got: {error}");
    }

    #[test]
    fn numbered_family_rename_observes_whole_family_shadowing() {
        for body in [
            "let .x. = a; print(x1); let .y. = b; pair(x3, y1)",
            "action { let .x. <- a; print(x1); let .y. <- b; pair(x3, y1) }",
        ] {
            let source = format!("module main; fn run() {{ {body} }}");
            let module = parse_surface_module(&source).unwrap();
            for offset in [source.find(".x.").unwrap() + 1, source.find("x3").unwrap()] {
                let family = placeholder_family_at(&module, offset as u32).unwrap();
                assert!(
                    family.rename("y", &LineIndex::new(&source)).is_err(),
                    "{source}"
                );
                let family = placeholder_family_at(&module, offset as u32).unwrap();
                let edits = family.rename("arg", &LineIndex::new(&source)).unwrap();
                assert_eq!(edits.len(), 3, "stem and both owned references");
            }
        }
    }
}
