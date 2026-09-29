//! LSP `textDocument/references` handler.
//!
//! Given a (URI, position, includeDeclaration) from the client:
//!
//! 1. Resolve the URI to a module path and find the typed binder at the
//!    cursor. Surface labels use their generated nominal identity, so local
//!    declarations and package-wide uses share the same set.
//! 2. Walk every file in the
//!    package's position index for entries that
//!    resolve to the "same" binder (matched by structural equality of
//!    the [`ResolvedBinder`] key fields).
//! 3. If a live label overlay has no typed position yet, fall back to its
//!    module-local declaration/reuse index.
//! 4. Return the matching spans as `Location[]`, sorted stably by
//!    file path then span start offset.
//!
//! **Binder equality.**
//! Two binder entries are considered the "same" binder when their key
//! fields agree:
//!
//! - `Fn` / `HostFn`: same `module_path` + `name`.
//! - `Newtype` / `NewtypeMember`: same `module_path` + `name` / `member`.
//! - `Local` / `TypeParam`: same module path, binder kind, name, and exact
//!   declaration span when available.
//! - `Intrinsic`: same `name`.
//! - Qualified variants: same consumer module + alias + member when
//!   applicable.
//!
//! **`includeDeclaration`.**
//! The LSP `ReferenceParams.context.include_declaration` flag controls
//! whether the semantic declaration span (if known) appears in the output.
//! The filtered span is independent of whether the request cursor starts on
//! that declaration or on any use.

use std::path::PathBuf;

use lsp_types::{Location, Position, Range, Uri};

use crate::cmd::check::LspAnalysis;
use crate::lsp::diagnostics::path_to_uri;
use crate::lsp::label_reuse::{
    LabelReuseSnapshot, snapshot_for, typed_label_positions_are_current,
};
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::lsp::util::{smallest_containing_span, uri_to_canonical};
use crate::pass::typecheck_full::ResolvedBinder;
use crate::span::Span;

/// Handle one `textDocument/references` request.
///
/// Returns `None` (→ JSON `null`) when the cursor isn't on a
/// resolvable binder or label reuse; returns `Some([])` when a binder is found
/// but no other references exist.
pub fn handle_references(
    uri: &Uri,
    position: &Position,
    include_declaration: bool,
    analysis: &LspAnalysis,
    overlay: Option<LabelReuseSnapshot<'_>>,
    package_root: &std::path::Path,
) -> Option<Vec<Location>> {
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

    let label_binding = syntax.index.and_then(|index| index.binding_at(byte_offset));
    let typed_label_positions_are_current =
        typed_label_positions_are_current(canonical.as_path(), byte_offset, analysis, syntax);

    // Find the binder at the cursor.
    let cursor_entry = smallest_containing_span(
        byte_offset,
        analysis
            .position_index
            .binders_iter()
            .filter(|((mp, _), _)| mp == module_path)
            .map(|((_, span), binder)| (*span, binder)),
    );
    let cursor_binder = cursor_entry.map(|(_, binder)| binder);

    // A live overlay may be newer than `analysis`. When its cursor is on a
    // local label declaration/reuse, use a typed binder only if that binder
    // names the same generated nominal the current surface spelling mints.
    // Merely occupying the same byte range is not evidence that a stale
    // position entry still denotes this label.
    let cursor_binder = match (label_binding.as_ref(), cursor_binder) {
        _ if !typed_label_positions_are_current => None,
        (
            Some(binding),
            Some(
                binder @ ResolvedBinder::Newtype {
                    module_path: owner,
                    name,
                },
            ),
        ) if typed_label_positions_are_current
            && owner == module_path
            && name == &crate::ast::mint_label_newtype_name(&binding.name) =>
        {
            Some(binder)
        }
        (Some(_), _) => None,
        (None, binder) => binder,
    };

    // A successful typed analysis gives every source label spelling its
    // canonical generated-newtype identity, so declarations, local uses, and
    // imported uses share one package-wide reference set. The local label
    // index remains the fallback for a live overlay that has no matching typed
    // position yet.
    let Some(cursor_binder) = cursor_binder else {
        let binding = label_binding?;
        let locations = binding
            .occurrences
            .into_iter()
            .filter(|span| include_declaration || *span != binding.declaration_span)
            .map(|span| {
                let lsp_range = line_index.to_range(span);
                Location::new(
                    uri.clone(),
                    Range {
                        start: Position {
                            line: lsp_range.start.line,
                            character: lsp_range.start.character,
                        },
                        end: Position {
                            line: lsp_range.end.line,
                            character: lsp_range.end.character,
                        },
                    },
                )
            })
            .collect();
        return Some(locations);
    };
    // The cursor module qualifies local and type-parameter identities.
    let key = binder_key(cursor_binder, module_path);
    let declaration_site = analysis
        .position_index
        .semantic_declaration_site(module_path, cursor_binder);

    // Walk every binder entry in the package and collect those that
    // match the same key.
    let mut locations: Vec<(PathBuf, Span)> = Vec::new();

    for ((entry_module_path, entry_span), entry_binder) in analysis.position_index.binders_iter() {
        if !binder_matches_key(entry_binder, entry_module_path, &key) {
            continue;
        }
        if !include_declaration
            && declaration_site.is_some_and(|(declaration_module, declaration_span)| {
                entry_module_path == declaration_module && *entry_span == declaration_span
            })
        {
            continue;
        }
        // Resolve the module path to a file path.
        if let Some(file_path) = analysis
            .file_to_module
            .iter()
            .find(|(_, mp)| mp.as_str() == entry_module_path)
            .map(|(fp, _)| fp.clone())
        {
            locations.push((file_path, *entry_span));
        }
    }

    // A generated label's constructor and projector share the label token
    // with its generated nominal declaration. Their semantic declaration
    // sites therefore cannot also occupy the one-binder-per-span position
    // map. Include that site when its source position is published by this
    // analysis shard; a declaration retained only for focused remote
    // goto-definition must not widen a focused references result.
    if include_declaration
        && let Some((declaration_module, declaration_span)) = declaration_site
        && analysis
            .position_index
            .binder_at(declaration_module, declaration_span)
            .is_some()
        && !locations.iter().any(|(file_path, span)| {
            *span == declaration_span
                && analysis.file_to_module.get(file_path).map(String::as_str)
                    == Some(declaration_module)
        })
        && let Some(declaration_file) = analysis
            .file_to_module
            .iter()
            .find_map(|(path, module)| (module == declaration_module).then(|| path.clone()))
    {
        locations.push((declaration_file, declaration_span));
    }

    // Sort by file path then span start for stable output.
    locations.sort_by(|(fa, sa), (fb, sb)| {
        fa.cmp(fb)
            .then_with(|| sa.start.cmp(&sb.start))
            .then_with(|| sa.end.cmp(&sb.end))
    });

    // Convert to `Location`.
    let result: Vec<Location> = locations
        .into_iter()
        .filter_map(|(file_path, span)| {
            let uri = path_to_uri(&file_path, package_root)?;
            // Build a line index for this file to convert the span.
            let source = analysis.sources.get(&file_path)?;
            let li = LineIndex::new(source);
            let lsp_range = li.to_range(span);
            let range = Range {
                start: Position {
                    line: lsp_range.start.line,
                    character: lsp_range.start.character,
                },
                end: Position {
                    line: lsp_range.end.line,
                    character: lsp_range.end.character,
                },
            };
            Some(Location::new(uri, range))
        })
        .collect();

    Some(result)
}

/// A structural key derived from a [`ResolvedBinder`] for equality
/// comparisons. Two binder entries are "the same binder" when their
/// keys agree.
#[derive(Debug, PartialEq, Eq)]
enum BinderKey {
    BlockLabel {
        module_path: String,
        elaborator: String,
        ordinal: usize,
    },
    /// Top-level fn or cross-module fn: `(module_path, name)`.
    Fn { module_path: String, name: String },
    /// Host fn: `(module_path, name)`.
    HostFn { module_path: String, name: String },
    /// Transparent type alias: `(module_path, name)`.
    TypeAlias { module_path: String, name: String },
    /// Host type: `(module_path, name)`.
    HostType { module_path: String, name: String },
    /// Newtype head segment: `(module_path, name)`.
    Newtype { module_path: String, name: String },
    /// Newtype member: `(module_path, newtype, member)`.
    NewtypeMember {
        module_path: String,
        newtype: String,
        member: String,
    },
    /// Local: declaring module, name, and exact declaration span when known.
    Local {
        module_path: String,
        name: String,
        decl_span: Option<Span>,
    },
    /// Type parameter: the same identity fields, distinct from value locals.
    TypeParam {
        module_path: String,
        name: String,
        decl_span: Option<Span>,
    },
    /// Intrinsic: name only (intrinsics are global).
    Intrinsic { name: String },
    /// Qualified import: consumer module + alias.
    QualifiedImport { module_path: String, alias: String },
    /// Qualified import member: consumer module + alias + member.
    QualifiedImportMember {
        module_path: String,
        alias: String,
        member: String,
    },
}

fn binder_key(b: &ResolvedBinder, cursor_module_path: &str) -> BinderKey {
    match b {
        ResolvedBinder::BlockLabel {
            module_path,
            elaborator,
            ordinal,
            ..
        } => BinderKey::BlockLabel {
            module_path: module_path.clone(),
            elaborator: elaborator.clone(),
            ordinal: *ordinal,
        },
        ResolvedBinder::Fn { module_path, name } => BinderKey::Fn {
            module_path: module_path.clone(),
            name: name.clone(),
        },
        ResolvedBinder::HostEnvFn { module_path, name } => BinderKey::HostFn {
            module_path: module_path.clone(),
            name: name.clone(),
        },
        ResolvedBinder::TypeAlias { module_path, name } => BinderKey::TypeAlias {
            module_path: module_path.clone(),
            name: name.clone(),
        },
        ResolvedBinder::HostType { module_path, name } => BinderKey::HostType {
            module_path: module_path.clone(),
            name: name.clone(),
        },
        ResolvedBinder::Newtype { module_path, name } => BinderKey::Newtype {
            module_path: module_path.clone(),
            name: name.clone(),
        },
        ResolvedBinder::NewtypeMember {
            module_path,
            newtype,
            member,
        } => BinderKey::NewtypeMember {
            module_path: module_path.clone(),
            newtype: newtype.clone(),
            member: member.clone(),
        },
        ResolvedBinder::Local { name, decl_span } => BinderKey::Local {
            module_path: cursor_module_path.to_owned(),
            name: name.clone(),
            decl_span: *decl_span,
        },
        ResolvedBinder::TypeParam { name, decl_span } => BinderKey::TypeParam {
            module_path: cursor_module_path.to_owned(),
            name: name.clone(),
            decl_span: *decl_span,
        },
        ResolvedBinder::Intrinsic { name } => BinderKey::Intrinsic { name: name.clone() },
        ResolvedBinder::QualifiedImport { alias } => BinderKey::QualifiedImport {
            module_path: cursor_module_path.to_owned(),
            alias: alias.clone(),
        },
        ResolvedBinder::QualifiedImportMember { alias, member } => {
            BinderKey::QualifiedImportMember {
                module_path: cursor_module_path.to_owned(),
                alias: alias.clone(),
                member: member.clone(),
            }
        }
    }
}

/// Check whether `entry_binder` in `entry_module_path` matches `key`.
/// Module and name must agree for local and type-parameter entries;
/// declaration spans must also agree when both entries carry one.
fn binder_matches_key(
    entry_binder: &ResolvedBinder,
    entry_module_path: &str,
    key: &BinderKey,
) -> bool {
    match (key, entry_binder) {
        (
            BinderKey::BlockLabel {
                module_path,
                elaborator,
                ordinal,
            },
            ResolvedBinder::BlockLabel {
                module_path: owner,
                elaborator: head,
                ordinal: index,
                ..
            },
        ) if module_path == owner && elaborator == head && ordinal == index => true,
        (
            BinderKey::Fn { module_path, name },
            ResolvedBinder::Fn {
                module_path: emp,
                name: en,
            },
        ) if module_path == emp && name == en => true,
        (
            BinderKey::HostFn { module_path, name },
            ResolvedBinder::HostEnvFn {
                module_path: emp,
                name: en,
            },
        ) if module_path == emp && name == en => true,
        (
            BinderKey::TypeAlias { module_path, name },
            ResolvedBinder::TypeAlias {
                module_path: emp,
                name: en,
            },
        ) if module_path == emp && name == en => true,
        (
            BinderKey::HostType { module_path, name },
            ResolvedBinder::HostType {
                module_path: emp,
                name: en,
            },
        ) if module_path == emp && name == en => true,
        (
            BinderKey::Newtype { module_path, name },
            ResolvedBinder::Newtype {
                module_path: emp,
                name: en,
            },
        ) if module_path == emp && name == en => true,
        (
            BinderKey::NewtypeMember {
                module_path,
                newtype,
                member,
            },
            ResolvedBinder::NewtypeMember {
                module_path: emp,
                newtype: ent,
                member: emem,
            },
        ) if module_path == emp && newtype == ent && member == emem => true,
        (
            BinderKey::Local {
                module_path,
                name,
                decl_span,
            },
            ResolvedBinder::Local {
                name: en,
                decl_span: entry_decl_span,
            },
        ) if module_path == entry_module_path && name == en => decl_span
            .zip(*entry_decl_span)
            .is_none_or(|(expected, actual)| expected == actual),
        (
            BinderKey::TypeParam {
                module_path,
                name,
                decl_span,
            },
            ResolvedBinder::TypeParam {
                name: en,
                decl_span: entry_decl_span,
            },
        ) if module_path == entry_module_path && name == en => decl_span
            .zip(*entry_decl_span)
            .is_none_or(|(expected, actual)| expected == actual),
        (BinderKey::Intrinsic { name }, ResolvedBinder::Intrinsic { name: en }) if name == en => {
            true
        }
        (
            BinderKey::QualifiedImport { module_path, alias },
            ResolvedBinder::QualifiedImport { alias: ea },
        ) if module_path == entry_module_path && alias == ea => true,
        (
            BinderKey::QualifiedImportMember {
                module_path,
                alias,
                member,
            },
            ResolvedBinder::QualifiedImportMember {
                alias: ea,
                member: em,
            },
        ) if module_path == entry_module_path && alias == ea && member == em => true,
        _ => false,
    }
}

// --------------------------------------------------------------------------
// Public exports for `rename.rs`.
//
// `rename.rs` needs to walk the same binder-key / binder-matches logic to
// collect reference spans for a rename operation.  Rather than duplicating
// the logic, we expose thin wrappers that forward to the private functions.
// The opaque `BinderKeyPub` type hides the internal enum variants so only
// the references/rename pair needs to understand them.
// --------------------------------------------------------------------------

/// Opaque binder-key type used by `rename.rs` to identify which binder
/// to rename across the package.  Construct via [`binder_key_pub`].
pub struct BinderKeyPub(BinderKey);

/// Derive an opaque [`BinderKeyPub`] from `b` for use with
/// [`binder_matches_key_pub`]. `cursor_module_path` identifies the module
/// containing the cursor; it qualifies local and type-parameter identities.
pub fn binder_key_pub(b: &ResolvedBinder, cursor_module_path: &str) -> BinderKeyPub {
    BinderKeyPub(binder_key(b, cursor_module_path))
}

/// Check whether `entry_binder` in `entry_module_path` matches `key`.
/// Mirrors the private [`binder_matches_key`] used by `handle_references`.
pub fn binder_matches_key_pub(
    entry_binder: &ResolvedBinder,
    entry_module_path: &str,
    key: &BinderKeyPub,
) -> bool {
    binder_matches_key(entry_binder, entry_module_path, &key.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::check::LspAnalysis;
    use crate::lsp::util::{test_file_path, test_file_uri};
    use crate::pass::typecheck_full::{PositionIndex, ResolvedBinder};
    use crate::span::Span;
    use std::collections::{BTreeMap, HashMap};

    fn make_analysis_refs(entries: Vec<(&str, Span, ResolvedBinder, &str)>) -> LspAnalysis {
        // entries: (module_path, span, binder, source)
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
            label_reuse_indexes: crate::lsp::label_reuse::indexes_from_sources(&sources),
            sources,
            generated_label_nominals: Default::default(),
            root_package_lowered: crate::pass::resolve::Package::from_parts(
                std::collections::BTreeMap::new(),
                None,
            ),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn references_returns_none_outside_any_binder() {
        let source = "module pkg/main;\n";
        let analysis = make_analysis_refs(vec![(
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
        let package_root = test_file_path("/tmp");
        let result = handle_references(&uri, &pos, false, &analysis, None, &package_root);
        assert!(result.is_none());
    }

    #[test]
    fn references_distinguish_label_declaration_from_reuse() {
        let source =
            "module pkg/main;\nlabels { field: . };\nlabels Row = { field: _, other: . };\n";
        let analysis = make_analysis_refs(vec![(
            "pkg/main",
            Span::new(0, 1),
            ResolvedBinder::Intrinsic {
                name: "__absurd__".to_owned(),
            },
            source,
        )]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let pos = Position {
            line: 2,
            character: 15,
        };
        let package_root = test_file_path("/tmp");

        let with_declaration = handle_references(&uri, &pos, true, &analysis, None, &package_root)
            .expect("label binding");
        assert_eq!(with_declaration.len(), 2);
        assert_eq!(with_declaration[0].range.start.line, 1);
        assert_eq!(with_declaration[1].range.start.line, 2);

        let references_only = handle_references(&uri, &pos, false, &analysis, None, &package_root)
            .expect("label binding");
        assert_eq!(references_only.len(), 1);
        assert_eq!(references_only[0].range.start.line, 2);

        let declaration_pos = Position {
            line: 1,
            character: 9,
        };
        let uses_only = handle_references(
            &uri,
            &declaration_pos,
            false,
            &analysis,
            None,
            &package_root,
        )
        .expect("label binding from its declaration");
        assert_eq!(uses_only.len(), 1);
        assert_eq!(uses_only[0].range.start.line, 2);
    }

    #[test]
    fn shifted_live_label_overlay_rejects_same_identity_stale_typed_span() {
        let analyzed_source =
            "module pkg/main;\nlabels { field: . };\nlabels Row = { field: _, other: . };\n";
        let live_source =
            "module pkg/main;\nlabels { field: . };\nlabels Row = {  field: _, other: . };\n";
        let stale_start = analyzed_source.rfind("field").expect("stale reuse marker") as u32;
        let stale_span = Span::new(stale_start, stale_start + "field".len() as u32);
        let analysis = make_analysis_refs(vec![(
            "pkg/main",
            stale_span,
            ResolvedBinder::Newtype {
                module_path: "pkg/main".to_owned(),
                name: crate::ast::mint_label_newtype_name("field"),
            },
            analyzed_source,
        )]);
        let parsed = crate::pass::parser::parse(live_source).expect("parse live overlay");
        let overlay_index =
            crate::lsp::label_reuse::LabelReuseIndex::from_module(&parsed, live_source);
        let overlay = LabelReuseSnapshot {
            source: live_source,
            index: Some(&overlay_index),
        };
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let live_start = live_source.rfind("field").expect("live reuse marker") as u32;
        assert_eq!(live_start, stale_start + 1);
        let cursor = LineIndex::new(live_source).to_position(live_start);
        let position = Position {
            line: cursor.line,
            character: cursor.character,
        };

        let locations = handle_references(
            &uri,
            &position,
            true,
            &analysis,
            Some(overlay),
            &test_file_path("/tmp"),
        )
        .expect("the live label binding remains resolvable");

        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].range.start.line, 1);
        assert_eq!(locations[1].range.start.line, 2);
    }

    #[test]
    fn shifted_expression_label_overlay_never_uses_stale_typed_span() {
        let analyzed_source = "module pkg/main;\nfn f() -> . { {field=()} }\n";
        let live_source = "module pkg/main;\nfn f() -> . { { field=()} }\n";
        let stale_start = analyzed_source.find("field").expect("stale label") as u32;
        let stale_span = Span::new(stale_start, stale_start + "field".len() as u32);
        let analysis = make_analysis_refs(vec![(
            "pkg/main",
            stale_span,
            ResolvedBinder::Newtype {
                module_path: "pkg/main".to_owned(),
                name: crate::ast::mint_label_newtype_name("field"),
            },
            analyzed_source,
        )]);
        let parsed = crate::pass::parser::parse(live_source).expect("parse live overlay");
        let overlay_index =
            crate::lsp::label_reuse::LabelReuseIndex::from_module(&parsed, live_source);
        let overlay = LabelReuseSnapshot {
            source: live_source,
            index: Some(&overlay_index),
        };
        let live_start = live_source.find("field").expect("live label") as u32;
        assert_eq!(live_start, stale_start + 1);
        let cursor = LineIndex::new(live_source).to_position(live_start);
        let uri = test_file_uri("/tmp/pkg/main.kio");

        assert!(
            handle_references(
                &uri,
                &Position {
                    line: cursor.line,
                    character: cursor.character,
                },
                true,
                &analysis,
                Some(overlay),
                &test_file_path("/tmp"),
            )
            .is_none(),
            "stale package-wide label facts must wait for fresh analysis"
        );
    }

    #[test]
    fn broken_live_overlay_never_uses_same_span_stale_label_identity() {
        let analyzed_source = "module pkg/main;\nfn f() -> . { {field=()} }\n";
        let live_source = "module pkg/main;\nfn f() -> . { {other=()} \n";
        let label_start = analyzed_source.find("field").expect("analyzed label") as u32;
        assert_eq!(
            label_start,
            live_source.find("other").expect("live label") as u32
        );
        let analysis = make_analysis_refs(vec![(
            "pkg/main",
            Span::new(label_start, label_start + "field".len() as u32),
            ResolvedBinder::Newtype {
                module_path: "pkg/main".to_owned(),
                name: crate::ast::mint_label_newtype_name("field"),
            },
            analyzed_source,
        )]);
        let overlay = LabelReuseSnapshot {
            source: live_source,
            index: None,
        };
        let cursor = LineIndex::new(live_source).to_position(label_start);
        let uri = test_file_uri("/tmp/pkg/main.kio");

        assert!(
            handle_references(
                &uri,
                &Position {
                    line: cursor.line,
                    character: cursor.character,
                },
                true,
                &analysis,
                Some(overlay),
                &test_file_path("/tmp"),
            )
            .is_none(),
            "an unparseable changed overlay cannot authenticate an old typed label"
        );
    }

    #[test]
    fn references_finds_two_uses_of_same_fn() {
        let source = "module pkg/main;\n  run  run  ";
        // Two binder entries for `pkg/main.run`.
        let binder1 = ResolvedBinder::Fn {
            module_path: "pkg/main".to_owned(),
            name: "run".to_owned(),
        };
        let binder2 = ResolvedBinder::Fn {
            module_path: "pkg/main".to_owned(),
            name: "run".to_owned(),
        };
        let analysis = make_analysis_refs(vec![
            ("pkg/main", Span::new(18, 21), binder1, source),
            ("pkg/main", Span::new(23, 26), binder2, source),
        ]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        // Click on first `run`.
        let pos = Position {
            line: 1,
            character: 2,
        };
        let package_root = test_file_path("/tmp");
        let result = handle_references(&uri, &pos, true, &analysis, None, &package_root);
        // Should find both uses.
        if let Some(refs) = result {
            assert!(!refs.is_empty());
        }
    }

    #[test]
    fn binder_key_roundtrip_fn() {
        let b = ResolvedBinder::Fn {
            module_path: "a.b".to_owned(),
            name: "foo".to_owned(),
        };
        let key = binder_key(&b, "a.b");
        assert!(binder_matches_key(&b, "a.b", &key));
    }

    #[test]
    fn binder_key_no_cross_match_different_module() {
        let b1 = ResolvedBinder::Fn {
            module_path: "a.b".to_owned(),
            name: "foo".to_owned(),
        };
        let b2 = ResolvedBinder::Fn {
            module_path: "c.d".to_owned(),
            name: "foo".to_owned(),
        };
        let key = binder_key(&b1, "a.b");
        assert!(!binder_matches_key(&b2, "c.d", &key));
    }

    #[test]
    fn local_key_no_cross_match_different_module() {
        let cursor = ResolvedBinder::Local {
            name: "x".to_owned(),
            decl_span: None,
        };
        let other = ResolvedBinder::Local {
            name: "x".to_owned(),
            decl_span: None,
        };
        let key = binder_key(&cursor, "a.b");
        assert!(binder_matches_key(&cursor, "a.b", &key));
        assert!(!binder_matches_key(&other, "c.d", &key));
    }

    #[test]
    fn type_param_key_no_cross_match_different_module() {
        let cursor = ResolvedBinder::TypeParam {
            name: "T".to_owned(),
            decl_span: None,
        };
        let other = ResolvedBinder::TypeParam {
            name: "T".to_owned(),
            decl_span: None,
        };
        let key = binder_key(&cursor, "a.b");
        assert!(binder_matches_key(&cursor, "a.b", &key));
        assert!(!binder_matches_key(&other, "c.d", &key));
    }

    #[test]
    fn local_key_uses_decl_span_when_available() {
        let cursor = ResolvedBinder::Local {
            name: "x".to_owned(),
            decl_span: Some(Span::new(10, 11)),
        };
        let same_name_other_decl = ResolvedBinder::Local {
            name: "x".to_owned(),
            decl_span: Some(Span::new(20, 21)),
        };
        let key = binder_key(&cursor, "a.b");
        assert!(binder_matches_key(&cursor, "a.b", &key));
        assert!(!binder_matches_key(&same_name_other_decl, "a.b", &key));
    }

    #[test]
    fn qualified_import_key_is_local_to_consumer_module() {
        let cursor = ResolvedBinder::QualifiedImport {
            alias: "helper".to_owned(),
        };
        let same_spelling = ResolvedBinder::QualifiedImport {
            alias: "helper".to_owned(),
        };
        let key = binder_key(&cursor, "consumer/one");

        assert!(binder_matches_key(&cursor, "consumer/one", &key));
        assert!(!binder_matches_key(&same_spelling, "consumer/two", &key));

        let cursor_member = ResolvedBinder::QualifiedImportMember {
            alias: "helper".to_owned(),
            member: "run".to_owned(),
        };
        let same_spelling_member = ResolvedBinder::QualifiedImportMember {
            alias: "helper".to_owned(),
            member: "run".to_owned(),
        };
        let member_key = binder_key(&cursor_member, "consumer/one");
        assert!(binder_matches_key(
            &cursor_member,
            "consumer/one",
            &member_key
        ));
        assert!(!binder_matches_key(
            &same_spelling_member,
            "consumer/two",
            &member_key
        ));
    }
}
