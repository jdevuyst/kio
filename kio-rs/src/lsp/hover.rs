//! LSP `textDocument/hover` handler.
//!
//! Given a (URI, position) pair from the client:
//!
//! 1. Resolve the URI to a canonical path and look up its module path
//!    in the latest stored [`crate::cmd::check::LspAnalysis`].
//! 2. Convert the LSP position to a byte offset via the [`LineIndex`].
//! 3. Prefer declaration, builtin, and surface label-reuse hovers rendered as
//!    Markdown.
//! 4. Otherwise find the smallest span in
//!    [`crate::pass::typecheck_full::PositionIndex`] whose byte range
//!    contains the offset (the "most specific node").
//! 5. Return a [`Hover`] containing Markdown content plus a range so
//!    editors can highlight the hovered token.
//!
//! Returns `None` (→ JSON `null`) for positions that don't hit any
//! recorded type entry — whitespace, comments, keyword tokens that
//! aren't expressions. This matches the LSP spec's nullable result.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position, Range, Uri};

use crate::cmd::check::LspAnalysis;
use crate::lsp::docs::{builtin_hover_at, doc_hover_at};
use crate::lsp::label_reuse::{
    LabelReuseSnapshot, snapshot_for, typed_label_positions_are_current,
};
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::lsp::util::smallest_containing_span;
use crate::pass::typecheck_core::{source_type_spelling_with_bound, write_type};
use crate::span::Span;

/// Handle one `textDocument/hover` request.
///
/// `uri` is the document URI from the request; `position` is the
/// cursor position. The caller resolves the file and feeds the latest
/// analysis here. Returns `None` when no declaration, builtin, or
/// typeable node is at the cursor.
pub fn handle_hover(
    uri: &Uri,
    position: &Position,
    analysis: &LspAnalysis,
    overlay: Option<LabelReuseSnapshot<'_>>,
) -> Option<Hover> {
    let canonical = crate::lsp::util::uri_to_canonical(uri)?;
    let module_path = analysis.file_to_module.get(canonical.as_path())?;

    // Build (or reuse) the line index for this file. Prefer the
    // overlay text (editor buffer) over the analysis snapshot's source
    // map — the analysis may have run slightly before the buffer was
    // updated, but position arithmetic is always against the live text.
    let syntax = snapshot_for(canonical.as_path(), analysis, overlay)?;
    let source = syntax.source;
    let line_index = LineIndex::new(source);

    let lsp_pos = LspPosition {
        line: position.line,
        character: position.character,
    };
    let byte_offset = line_index.position_to_offset(lsp_pos);

    if let Some(hover) = doc_hover_at(source, byte_offset, &line_index) {
        return Some(hover);
    }
    if let Some(hover) = builtin_hover_at(source, byte_offset, &line_index) {
        return Some(hover);
    }
    if let Some(binding) = syntax.index.and_then(|index| index.binding_at(byte_offset))
        && binding.cursor_is_reuse
    {
        let declaration = crate::lsp::label_reuse::source_entry(source, &binding)?.trim();
        let contents = HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!(
                "Reuses the earlier module-local label declaration:\n\n```kio\n{declaration}\n```"
            ),
        });
        return Some(Hover {
            contents,
            range: Some(span_to_lsp_range(binding.cursor_span, &line_index)),
        });
    }
    if !typed_label_positions_are_current(canonical.as_path(), byte_offset, analysis, syntax) {
        return None;
    }

    if let Some((
        span,
        crate::pass::typecheck_full::ResolvedBinder::BlockLabel {
            elaborator,
            name,
            exposure,
            ..
        },
    )) = smallest_containing_span(
        byte_offset,
        analysis
            .position_index
            .binders_iter()
            .filter(|((owner, _), _)| owner == module_path)
            .map(|((_, span), binder)| (*span, binder)),
    ) {
        let kind = match exposure {
            crate::ast::BlockExposure::Product => "product",
            crate::ast::BlockExposure::Thunk => "thunk",
            crate::ast::BlockExposure::Sequence => "sequence",
        };
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!(
                    "```kio\ntrailing {kind} {name}\n```\n\nContinuation label of `{elaborator}!`."
                ),
            }),
            range: Some(span_to_lsp_range(span, &line_index)),
        });
    }

    // Find the smallest span in the type index that contains the cursor.
    let Some((span, ty)) = smallest_containing_span(
        byte_offset,
        analysis
            .position_index
            .types_iter()
            .filter(|((mp, _), _)| mp == module_path)
            .map(|((_, span), ty)| (*span, ty)),
    ) else {
        return nominal_type_hover(byte_offset, module_path, analysis, &line_index);
    };

    // Format the type as a Markdown fenced code block.
    let mut type_str = String::new();
    let empty_binders = std::collections::HashSet::new();
    let binder_names = analysis.position_index.type_binders_at(module_path, span);
    let binders = binder_names.as_ref().unwrap_or(&empty_binders);
    let display_ty = analysis
        .root_package_lowered
        .module(module_path)
        .map(|entry| {
            source_type_spelling_with_bound(
                ty,
                &entry.module,
                analysis
                    .position_index
                    .type_identity_is_canonical(module_path, span),
                binders,
            )
        });
    let display_ty = analysis.position_index.display_position_type(
        module_path,
        span,
        display_ty.as_ref().unwrap_or(ty),
    );
    write_type(&display_ty, &mut type_str);
    let contents = HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value: format!("```kio\n{type_str}\n```"),
    });

    let range = span_to_lsp_range(span, &line_index);

    Some(Hover {
        contents,
        range: Some(range),
    })
}

fn nominal_type_hover(
    byte_offset: u32,
    module_path: &str,
    analysis: &LspAnalysis,
    line_index: &LineIndex,
) -> Option<Hover> {
    let (span, binder) = smallest_containing_span(
        byte_offset,
        analysis
            .position_index
            .binders_iter()
            .filter(|((owner, _), _)| owner == module_path)
            .map(|((_, span), binder)| (*span, binder)),
    )?;
    let (kind, declaring_module, name) = match binder {
        crate::pass::typecheck_full::ResolvedBinder::TypeAlias { module_path, name } => {
            ("type", module_path, name)
        }
        crate::pass::typecheck_full::ResolvedBinder::Newtype { module_path, name } => {
            ("newtype", module_path, name)
        }
        crate::pass::typecheck_full::ResolvedBinder::HostType { module_path, name } => {
            ("host type", module_path, name)
        }
        _ => return None,
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!(
                "```kio\n{kind} {name}\n```\n\nDeclared in module `{declaring_module}`."
            ),
        }),
        range: Some(span_to_lsp_range(span, line_index)),
    })
}

/// Convert a [`Span`] (byte offsets) to an LSP [`Range`]
/// (line + UTF-16 character positions).
fn span_to_lsp_range(span: Span, line_index: &LineIndex) -> Range {
    let lsp_range = line_index.to_range(span);
    Range {
        start: Position {
            line: lsp_range.start.line,
            character: lsp_range.start.character,
        },
        end: Position {
            line: lsp_range.end.line,
            character: lsp_range.end.character,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Lowered, Meta, Type};
    use crate::cmd::check::LspAnalysis;
    use crate::lsp::util::{test_file_path, test_file_uri};
    use crate::pass::resolve::{ModuleEntry, Package, TopLevelScope};
    use crate::pass::typecheck_core::InternedType;
    use crate::pass::typecheck_full::PositionIndex;
    use crate::span::Span;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::path::Path;
    use std::path::PathBuf;

    /// Build a minimal `LspAnalysis` with a single type entry at `span`.
    /// Uses `Type::Unit` as a stand-in for any type; the tests care
    /// about span containment, not the type's string representation.
    fn make_analysis(module_path: &str, source: &str, span: Span) -> LspAnalysis {
        let ty = crate::ast::Type::<Lowered>::Unit {
            meta: Meta::new(Span::new(0, 0)),
        };
        let mut index = PositionIndex::new();
        index.record_type(module_path, span, ty);
        let file_path = test_file_path("/tmp/test.kio");
        let mut file_to_module = BTreeMap::new();
        file_to_module.insert(file_path.clone(), module_path.to_owned());
        let mut sources = HashMap::new();
        sources.insert(file_path, source.to_owned());
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

    fn lowered_module(source: &str) -> crate::ast::Module<Lowered> {
        let surface = crate::pass::parser::parse(source).expect("parse module");
        let desugared = crate::pass::desugar::desugar_module(surface).expect("desugar module");
        let (mut modules, _) = crate::pass::label_elab::elaborate_package(
            vec![(PathBuf::from("test.kio"), desugared)],
            None,
        )
        .expect("elaborate labels");
        modules.pop().expect("one module").1
    }

    fn lowered_package(sources: &[&str]) -> Package<Lowered> {
        let mut modules = BTreeMap::new();
        for source in sources {
            let module = lowered_module(source);
            let module_path = module
                .path
                .segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            let scope = TopLevelScope::build(&module).expect("top-level scope");
            modules.insert(
                module_path.clone(),
                ModuleEntry {
                    file_path: Path::new(&module_path).with_extension("kio"),
                    module,
                    scope,
                },
            );
        }
        Package::from_parts(modules, None)
    }

    fn hover_markdown(hover: Hover) -> String {
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("hover should use Markdown markup");
        };
        markup.value
    }

    #[test]
    fn hover_returns_none_outside_any_span() {
        let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
        // Place a type entry at bytes 30..40 — hover at byte 0 misses it.
        let analysis = make_analysis("pkg/main", source, Span::new(30, 40));
        let uri = test_file_uri("/tmp/test.kio");
        let pos = Position {
            line: 0,
            character: 0,
        };
        assert!(handle_hover(&uri, &pos, &analysis, None).is_none());
    }

    #[test]
    fn hover_returns_some_inside_span() {
        // `source` has the recorded span at bytes 7..8 ("p" in "pkg/main").
        let source = "module pkg/main;\n";
        let analysis = make_analysis("pkg/main", source, Span::new(7, 15));
        let uri = test_file_uri("/tmp/test.kio");
        // line 0, char 7 → byte offset 7, inside span 7..15.
        let pos = Position {
            line: 0,
            character: 7,
        };
        let result = handle_hover(&uri, &pos, &analysis, None);
        assert!(
            result.is_some(),
            "cursor inside recorded span should yield hover"
        );
    }

    #[test]
    fn hover_returns_none_for_unknown_uri() {
        let source = "x";
        let analysis = make_analysis("pkg/main", source, Span::new(0, 1));
        let uri = test_file_uri("/no/such/path.kio");
        let pos = Position {
            line: 0,
            character: 0,
        };
        assert!(handle_hover(&uri, &pos, &analysis, None).is_none());
    }

    #[test]
    fn hover_preserves_type_identity_under_qualified_alias_collision() {
        let source = concat!(
            "module main;\n",
            "import other as types;\n",
            "import types as real;\n",
            "fn run() -> . { () }\n",
        );
        let unit_start = u32::try_from(source.rfind("()").expect("unit expression"))
            .expect("test source fits in a span");
        let span = Span::new(unit_start, unit_start + 2);
        let mut analysis = make_analysis("main", source, span);
        analysis.root_package_lowered = lowered_package(&[
            source,
            "module other; pub host type Actual role(i32);",
            "module types; pub host type Actual role(bool);",
        ]);
        let canonical_type = Type::synth_path(
            vec!["types".to_owned(), "Actual".to_owned()],
            Vec::new(),
            span,
        );
        analysis.position_index.record_type_with_identity(
            "main",
            span,
            InternedType::fresh_canonical(canonical_type.clone()),
            HashSet::new(),
        );
        let uri = test_file_uri("/tmp/test.kio");
        let position = Position {
            line: 3,
            character: 16,
        };

        let canonical_hover = handle_hover(&uri, &position, &analysis, None)
            .map(hover_markdown)
            .expect("canonical hover");
        assert!(canonical_hover.contains("real.Actual"), "{canonical_hover}");
        assert!(
            !canonical_hover.contains("types.Actual"),
            "{canonical_hover}"
        );

        analysis
            .position_index
            .record_type("main", span, canonical_type);
        let source_hover = handle_hover(&uri, &position, &analysis, None)
            .map(hover_markdown)
            .expect("source hover");
        assert!(source_hover.contains("types.Actual"), "{source_hover}");
        assert!(!source_hover.contains("real.Actual"), "{source_hover}");
    }

    #[test]
    fn hover_on_label_reuse_shows_explicit_declaration() {
        let source =
            "module pkg/main;\nlabels { field: . };\nlabels Row = { field: _, other: . };\n";
        let analysis = make_analysis("pkg/main", source, Span::new(0, 1));
        let uri = test_file_uri("/tmp/test.kio");
        let pos = Position {
            line: 2,
            character: 15,
        };
        let hover = handle_hover(&uri, &pos, &analysis, None).expect("reuse hover");
        let HoverContents::Markup(contents) = hover.contents else {
            panic!("expected markdown hover")
        };
        assert!(contents.value.contains("field: ."));
        assert_eq!(hover.range.expect("marker range").start.line, 2);
    }

    #[test]
    fn hover_rejects_shifted_expression_label_typed_span() {
        let analyzed_source = "module pkg/main;\nfn f() -> . { {field=()} }\n";
        let live_source = "module pkg/main;\nfn f() -> . { { field=()} }\n";
        let stale_start = analyzed_source.find("field").expect("stale label") as u32;
        let analysis = make_analysis(
            "pkg/main",
            analyzed_source,
            Span::new(stale_start, stale_start + 5),
        );
        let parsed = crate::pass::parser::parse(live_source).expect("parse live overlay");
        let overlay_index =
            crate::lsp::label_reuse::LabelReuseIndex::from_module(&parsed, live_source);
        let live_start = live_source.find("field").expect("live label") as u32;
        let cursor = LineIndex::new(live_source).to_position(live_start);
        let uri = test_file_uri("/tmp/test.kio");

        assert!(
            handle_hover(
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
}
