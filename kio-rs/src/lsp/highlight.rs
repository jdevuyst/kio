//! LSP `textDocument/documentHighlight` handler.
//!
//! Highlights every occurrence of the binder under the cursor within
//! the current file. Surface label-reuse markers are paired with their
//! earlier explicit local declaration; other names use the structural
//! binder-key matching shared with [`super::references`]
//! ([`binder_key_pub`] / [`binder_matches_key_pub`]). Neither path walks
//! other files, so the handler stays cheap enough for cursor movement.
//!
//! Every occurrence is reported with [`DocumentHighlightKind::TEXT`].
//! The position index does not distinguish read sites from write sites,
//! so claiming `READ` / `WRITE` would be guesswork; `TEXT` is the honest
//! kind and clients render it with the default occurrence styling.

use lsp_types::{DocumentHighlight, DocumentHighlightKind, Position, Range, Uri};

use crate::cmd::check::LspAnalysis;
use crate::lsp::label_reuse::{
    LabelReuseSnapshot, snapshot_for, typed_label_positions_are_current,
};
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::lsp::references::{binder_key_pub, binder_matches_key_pub};
use crate::lsp::util::{smallest_containing_span, uri_to_canonical};
use crate::span::Span;

/// Handle one `textDocument/documentHighlight` request.
///
/// Returns `None` (→ JSON `null`) when the cursor isn't on a
/// resolvable binder; otherwise returns the occurrences of that binder
/// in the current file (always at least the cursor occurrence), sorted
/// by span start.
pub fn handle_document_highlight(
    uri: &Uri,
    position: &Position,
    analysis: &LspAnalysis,
    overlay: Option<LabelReuseSnapshot<'_>>,
) -> Option<Vec<DocumentHighlight>> {
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

    if let Some(binding) = syntax.index.and_then(|index| index.binding_at(byte_offset)) {
        let highlights = binding
            .occurrences
            .into_iter()
            .map(|span| DocumentHighlight {
                range: to_lsp_range(line_index.to_range(span)),
                kind: Some(DocumentHighlightKind::TEXT),
            })
            .collect();
        return Some(highlights);
    }
    if !typed_label_positions_are_current(canonical.as_path(), byte_offset, analysis, syntax) {
        return None;
    }

    // Find the binder at the cursor, restricting to the current module.
    let (_cursor_span, cursor_binder) = smallest_containing_span(
        byte_offset,
        analysis
            .position_index
            .binders_iter()
            .filter(|((mp, _), _)| mp == module_path)
            .map(|((_, span), binder)| (*span, binder)),
    )?;

    let key = binder_key_pub(cursor_binder, module_path);

    // Collect matching spans within the current module only.
    let mut spans: Vec<Span> = Vec::new();
    for ((entry_module_path, entry_span), entry_binder) in analysis.position_index.binders_iter() {
        if entry_module_path != module_path {
            continue;
        }
        if binder_matches_key_pub(entry_binder, entry_module_path, &key) {
            spans.push(*entry_span);
        }
    }

    spans.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.end.cmp(&b.end)));

    let highlights = spans
        .into_iter()
        .map(|span| {
            let lsp_range = line_index.to_range(span);
            DocumentHighlight {
                range: Range {
                    start: Position {
                        line: lsp_range.start.line,
                        character: lsp_range.start.character,
                    },
                    end: Position {
                        line: lsp_range.end.line,
                        character: lsp_range.end.character,
                    },
                },
                kind: Some(DocumentHighlightKind::TEXT),
            }
        })
        .collect();

    Some(highlights)
}

fn to_lsp_range(range: crate::lsp::positions::LspRange) -> Range {
    Range {
        start: Position {
            line: range.start.line,
            character: range.start.character,
        },
        end: Position {
            line: range.end.line,
            character: range.end.character,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::util::{test_file_path, test_file_uri};
    use crate::pass::typecheck_full::{PositionIndex, ResolvedBinder};
    use std::collections::{BTreeMap, HashMap};

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
    fn highlight_returns_none_outside_any_binder() {
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
        let result = handle_document_highlight(&uri, &pos, &analysis, None);
        assert!(result.is_none());
    }

    #[test]
    fn highlight_finds_two_uses_in_current_file() {
        let source = "module pkg/main;\n  run  run  ";
        let binder1 = ResolvedBinder::Fn {
            module_path: "pkg/main".to_owned(),
            name: "run".to_owned(),
        };
        let binder2 = ResolvedBinder::Fn {
            module_path: "pkg/main".to_owned(),
            name: "run".to_owned(),
        };
        let analysis = make_analysis(vec![
            ("pkg/main", Span::new(19, 22), binder1, source),
            ("pkg/main", Span::new(24, 27), binder2, source),
        ]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        // Click on first `run` (byte offset 19 = line 1, character 2).
        let pos = Position {
            line: 1,
            character: 2,
        };
        let result = handle_document_highlight(&uri, &pos, &analysis, None).expect("some");
        assert_eq!(result.len(), 2, "both occurrences highlighted");
        assert_eq!(result[0].range.start.line, 1);
    }

    #[test]
    fn highlight_connects_label_declaration_and_reuse() {
        let source =
            "module pkg/main;\nlabels { field: . };\nlabels Row = { field: _, other: . };\n";
        let analysis = make_analysis(vec![(
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
        let result = handle_document_highlight(&uri, &pos, &analysis, None).expect("binding");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].range.start.line, 1);
        assert_eq!(result[1].range.start.line, 2);
    }

    #[test]
    fn highlight_rejects_shifted_expression_label_typed_span() {
        let analyzed_source = "module pkg/main;\nfn f() -> . { {field=()} }\n";
        let live_source = "module pkg/main;\nfn f() -> . { { field=()} }\n";
        let stale_start = analyzed_source.find("field").expect("stale label") as u32;
        let analysis = make_analysis(vec![(
            "pkg/main",
            Span::new(stale_start, stale_start + 5),
            ResolvedBinder::Newtype {
                module_path: "pkg/main".to_owned(),
                name: crate::ast::mint_label_newtype_name("field"),
            },
            analyzed_source,
        )]);
        let parsed = crate::pass::parser::parse(live_source).expect("parse live overlay");
        let overlay_index =
            crate::lsp::label_reuse::LabelReuseIndex::from_module(&parsed, live_source);
        let live_start = live_source.find("field").expect("live label") as u32;
        let cursor = LineIndex::new(live_source).to_position(live_start);
        let uri = test_file_uri("/tmp/pkg/main.kio");

        assert!(
            handle_document_highlight(
                &uri,
                &Position {
                    line: cursor.line,
                    character: cursor.character,
                },
                &analysis,
                Some(LabelReuseSnapshot {
                    source: live_source,
                    index: Some(&overlay_index),
                }),
            )
            .is_none()
        );
    }

    #[test]
    fn highlight_excludes_same_named_binder_in_other_module() {
        // `pkg/main.run` at the cursor must not pull in `pkg/other.run`.
        let source_main = "module pkg/main;\n  run  ";
        let source_other = "module pkg/other;\n  run  ";
        let analysis = make_analysis(vec![
            (
                "pkg/main",
                Span::new(19, 22),
                ResolvedBinder::Fn {
                    module_path: "pkg/main".to_owned(),
                    name: "run".to_owned(),
                },
                source_main,
            ),
            (
                "pkg/other",
                Span::new(20, 23),
                ResolvedBinder::Fn {
                    module_path: "pkg/other".to_owned(),
                    name: "run".to_owned(),
                },
                source_other,
            ),
        ]);
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let pos = Position {
            line: 1,
            character: 2,
        };
        let result = handle_document_highlight(&uri, &pos, &analysis, None).expect("some");
        assert_eq!(result.len(), 1, "only the current-module occurrence");
    }
}
