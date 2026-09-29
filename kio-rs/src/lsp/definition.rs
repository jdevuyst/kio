//! LSP `textDocument/definition` handler.
//!
//! Given a (URI, position) pair from the client:
//!
//! 1. Resolve the URI to a canonical path and module path.
//! 2. Convert the LSP position to a byte offset.
//! 3. Resolve a surface label-reuse marker to its earlier explicit local
//!    declaration, when applicable.
//! 4. Otherwise find the binder recorded at that position in the
//!    [`crate::pass::typecheck_full::PositionIndex`].
//! 5. Resolve the binder to a declaration-site `(URI, range)` and
//!    return a [`GotoDefinitionResponse`].
//!
//! The declaration site depends on the binder kind:
//!
//! - **Local / TypeParam.** Declares within the cursor's own module.
//!   The position index records the declaration span at each binder-
//!   introduction site (the `let`, the `fn` value parameter, the
//!   `[A]` type-parameter binder). Uses carry that exact declaration
//!   span, so go-to-definition returns the selected declaration and
//!   shadowed same-named binders remain distinct.
//! - **Fn / Newtype / HostFn.** Resolve through the position index's exact
//!   semantic declaration-site map. Focused analysis retains only the remote
//!   declaration identities it needs, without publishing provider occurrences.
//! - **Intrinsic.** No source declaration; return `None`.
//! - **Import aliases.** No source declaration jump target; return `None`.

use lsp_types::{GotoDefinitionResponse, Location, Position, Range, Uri};

use crate::cmd::check::LspAnalysis;
use crate::lsp::diagnostics::path_to_uri;
use crate::lsp::label_reuse::{
    LabelReuseSnapshot, snapshot_for, typed_label_positions_are_current,
};
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::lsp::util::{smallest_containing_span, uri_to_canonical};

/// Handle one `textDocument/definition` request.
///
/// Returns `None` (→ JSON `null`) when the cursor isn't on a
/// resolvable binder or label reuse (whitespace, intrinsics, unknown
/// positions).
pub fn handle_definition(
    uri: &Uri,
    position: &Position,
    analysis: &LspAnalysis,
    overlay: Option<LabelReuseSnapshot<'_>>,
    package_root: &std::path::Path,
) -> Option<GotoDefinitionResponse> {
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
        let lsp_range = line_index.to_range(binding.declaration_span);
        let location = Location::new(
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
        );
        return Some(GotoDefinitionResponse::Scalar(location));
    }
    if !typed_label_positions_are_current(canonical.as_path(), byte_offset, analysis, syntax) {
        return None;
    }

    // Find the innermost binder at the cursor.
    let (_, binder) = smallest_containing_span(
        byte_offset,
        analysis
            .position_index
            .binders_iter()
            .filter(|((mp, _), _)| mp == module_path)
            .map(|((_, span), binder)| (*span, binder)),
    )?;

    if matches!(
        binder,
        crate::pass::typecheck_full::ResolvedBinder::QualifiedImport { .. }
            | crate::pass::typecheck_full::ResolvedBinder::QualifiedImportMember { .. }
    ) {
        return None;
    }

    let (declaration_module, declaration_span) = analysis
        .position_index
        .semantic_declaration_site(module_path, binder)?;
    let declaration_file = analysis
        .file_to_module
        .iter()
        .find_map(|(path, module)| (module == declaration_module).then_some(path))?;
    let declaration_source = analysis.sources.get(declaration_file)?;
    let declaration_range = LineIndex::new(declaration_source).to_range(declaration_span);
    let location = Location::new(
        path_to_uri(declaration_file, package_root)?,
        Range {
            start: Position {
                line: declaration_range.start.line,
                character: declaration_range.start.character,
            },
            end: Position {
                line: declaration_range.end.line,
                character: declaration_range.end.character,
            },
        },
    );
    Some(GotoDefinitionResponse::Scalar(location))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::check::LspAnalysis;
    use crate::lsp::util::{test_file_path, test_file_uri};
    use crate::pass::typecheck_full::{PositionIndex, ResolvedBinder};
    use crate::span::Span;
    use std::collections::{BTreeMap, HashMap};
    use std::path::PathBuf;

    fn make_analysis_with_binder(
        module_path: &str,
        source: &str,
        span: Span,
        binder: ResolvedBinder,
    ) -> (LspAnalysis, PathBuf) {
        let mut index = PositionIndex::new();
        index.record_binder(module_path, span, binder);
        let canonical = test_file_path("/tmp/testdef.kio");
        let mut file_to_module = BTreeMap::new();
        file_to_module.insert(canonical.clone(), module_path.to_owned());
        let mut sources = HashMap::new();
        sources.insert(canonical.clone(), source.to_owned());
        let analysis = LspAnalysis {
            position_index: index,
            file_to_module,
            label_reuse_indexes: crate::lsp::label_reuse::indexes_from_sources(&sources),
            sources,
            generated_label_nominals: Default::default(),
            // The LSP doesn't read these — supply empty placeholders.
            root_package_lowered: crate::pass::resolve::Package::from_parts(
                std::collections::BTreeMap::new(),
                None,
            ),
            warnings: Vec::new(),
        };
        (analysis, canonical)
    }

    #[test]
    fn definition_returns_none_for_intrinsic() {
        let (analysis, _) = make_analysis_with_binder(
            "pkg/main",
            "module pkg/main;\n",
            Span::new(7, 15),
            ResolvedBinder::Intrinsic {
                name: "__pair__".to_owned(),
            },
        );
        let uri = test_file_uri("/tmp/testdef.kio");
        let pos = Position {
            line: 0,
            character: 7,
        };
        let package_root = test_file_path("/tmp");
        let result = handle_definition(&uri, &pos, &analysis, None, &package_root);
        // Intrinsics → None.
        assert!(result.is_none());
    }

    #[test]
    fn definition_returns_none_outside_any_binder() {
        let (analysis, _) = make_analysis_with_binder(
            "pkg/main",
            "module pkg/main;\n",
            Span::new(20, 30),
            ResolvedBinder::Intrinsic {
                name: "__absurd__".to_owned(),
            },
        );
        let uri = test_file_uri("/tmp/testdef.kio");
        let pos = Position {
            line: 0,
            character: 0,
        };
        let package_root = test_file_path("/tmp");
        let result = handle_definition(&uri, &pos, &analysis, None, &package_root);
        assert!(result.is_none());
    }

    #[test]
    fn definition_resolves_label_reuse_to_explicit_declaration() {
        let source =
            "module pkg/main;\nlabels { field: . };\nlabels Row = { field: _, other: . };\n";
        let (analysis, _) = make_analysis_with_binder(
            "pkg/main",
            source,
            Span::new(0, 1),
            ResolvedBinder::Intrinsic {
                name: "__absurd__".to_owned(),
            },
        );
        let uri = test_file_uri("/tmp/testdef.kio");
        let pos = Position {
            line: 2,
            character: 15,
        };
        let result = handle_definition(&uri, &pos, &analysis, None, &test_file_path("/tmp"))
            .expect("reuse marker resolves");
        let range = scalar_range(&result);
        assert_eq!(range.start.line, 1);
        assert_eq!(range.start.character, 9);
    }

    #[test]
    fn definition_rejects_shifted_expression_label_typed_span() {
        let analyzed_source = "module pkg/main;\nfn f() -> . { {field=()} }\n";
        let live_source = "module pkg/main;\nfn f() -> . { { field=()} }\n";
        let stale_start = analyzed_source.find("field").expect("stale label") as u32;
        let (analysis, _) = make_analysis_with_binder(
            "pkg/main",
            analyzed_source,
            Span::new(stale_start, stale_start + 5),
            ResolvedBinder::Newtype {
                module_path: "pkg/main".to_owned(),
                name: crate::ast::mint_label_newtype_name("field"),
            },
        );
        let parsed = crate::pass::parser::parse(live_source).expect("parse live overlay");
        let overlay_index =
            crate::lsp::label_reuse::LabelReuseIndex::from_module(&parsed, live_source);
        let live_start = live_source.find("field").expect("live label") as u32;
        let cursor = LineIndex::new(live_source).to_position(live_start);
        let uri = test_file_uri("/tmp/testdef.kio");

        assert!(
            handle_definition(
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
                &test_file_path("/tmp"),
            )
            .is_none()
        );
    }

    /// Build an analysis with one file per module path, each carrying
    /// its own source, use-site binder entries, and local declaration
    /// spans. `binders` are `(module_path, use_span, binder)`;
    /// `decls` are `(module_path, name, decl_span)`.
    fn make_analysis_multi(
        files: &[(&str, &str)],
        binders: Vec<(&str, Span, ResolvedBinder)>,
        decls: Vec<(&str, &str, Span)>,
    ) -> LspAnalysis {
        let mut index = PositionIndex::new();
        let mut file_to_module = BTreeMap::new();
        let mut sources = HashMap::new();
        for (module_path, source) in files {
            let file_path = test_file_path(format!("/tmp/{module_path}.kio"));
            file_to_module.insert(file_path.clone(), (*module_path).to_owned());
            sources.insert(file_path, (*source).to_owned());
        }
        for (module_path, span, binder) in binders {
            index.record_binder(module_path, span, binder);
        }
        for (module_path, name, span) in decls {
            index.record_local_decl(module_path, name, span);
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

    fn scalar_range(resp: &GotoDefinitionResponse) -> Range {
        match resp {
            GotoDefinitionResponse::Scalar(loc) => loc.range,
            other => panic!("expected a scalar location; got {other:?}"),
        }
    }

    #[test]
    fn definition_resolves_local_use_to_let_declaration() {
        // line 0: "module pkg/main;\n"            (offsets 0..=16)
        // line 1: "pub fn run() -> . {\n"        (starts at 17)
        // line 2: "  let x = (); x\n"             (starts at 37)
        // The `let x` binder name `x` is at offset 43; the use of `x`
        // in the tail is at offset 51.
        let source = "module pkg/main;\npub fn run() -> . {\n  let x = (); x\n}\n";
        let decl_span = Span::new(37, 50); // the `let x = ();` statement span
        let use_span = Span::new(51, 52); // the trailing `x` use
        let analysis = make_analysis_multi(
            &[("pkg/main", source)],
            vec![(
                "pkg/main",
                use_span,
                ResolvedBinder::Local {
                    name: "x".to_owned(),
                    decl_span: Some(decl_span),
                },
            )],
            vec![("pkg/main", "x", decl_span)],
        );
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let package_root = test_file_path("/tmp");
        // Cursor on the trailing `x` (line 2, char 14).
        let pos = Position {
            line: 2,
            character: 14,
        };
        let result = handle_definition(&uri, &pos, &analysis, None, &package_root)
            .expect("local use resolves to its declaration");
        let range = scalar_range(&result);
        // The declaration span begins at the start of line 2.
        assert_eq!(range.start.line, 2);
        assert_eq!(range.start.character, 0);
    }

    #[test]
    fn definition_resolves_type_param_use_to_binder() {
        // line 0: "module pkg/main;\n"                  (0..=16)
        // line 1: "pub fn id[A](x: A) -> A { x }\n"     (starts at 17)
        // The `[A]` binder run starts at offset 26 (`[`); a use of `A`
        // (e.g. in the return type) is at offset 39.
        let source = "module pkg/main;\npub fn id[A](x: A) -> A { x }\n";
        let decl_span = Span::new(26, 28); // the `[A` binder run
        let use_span = Span::new(39, 40); // a `A` use in the signature
        let analysis = make_analysis_multi(
            &[("pkg/main", source)],
            vec![(
                "pkg/main",
                use_span,
                ResolvedBinder::TypeParam {
                    name: "A".to_owned(),
                    decl_span: Some(decl_span),
                },
            )],
            vec![("pkg/main", "A", decl_span)],
        );
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let package_root = test_file_path("/tmp");
        // Cursor on the `A` use (line 1, char 22).
        let pos = Position {
            line: 1,
            character: 22,
        };
        let result = handle_definition(&uri, &pos, &analysis, None, &package_root)
            .expect("type-param use resolves to its binder");
        let range = scalar_range(&result);
        // The binder span begins at the `[A` binder run on line 1.
        assert_eq!(range.start.line, 1);
        assert_eq!(range.start.character, 9);
    }

    #[test]
    fn definition_does_not_resolve_same_named_local_in_other_module() {
        // Two modules each declare a local `x`. A use of `x` in
        // `pkg/main` must resolve to `pkg/main`'s declaration, never
        // `pkg/other`'s — locals are keyed by declaring module path.
        let main_src = "module pkg/main;\npub fn run() -> . {\n  let x = (); x\n}\n";
        let other_src = "module pkg/other;\npub fn go() -> . {\n  let x = (); x\n}\n";
        let main_use = Span::new(51, 52);
        let main_decl = Span::new(37, 50);
        // `pkg/other` records a declaration for its own `x` at a span
        // that, if wrongly consulted, would land on a different line.
        let other_decl = Span::new(38, 51);
        let analysis = make_analysis_multi(
            &[("pkg/main", main_src), ("pkg/other", other_src)],
            vec![(
                "pkg/main",
                main_use,
                ResolvedBinder::Local {
                    name: "x".to_owned(),
                    decl_span: Some(main_decl),
                },
            )],
            vec![("pkg/main", "x", main_decl), ("pkg/other", "x", other_decl)],
        );
        let uri = test_file_uri("/tmp/pkg/main.kio");
        let package_root = test_file_path("/tmp");
        let pos = Position {
            line: 2,
            character: 14,
        };
        let result = handle_definition(&uri, &pos, &analysis, None, &package_root)
            .expect("local use resolves within its own module");
        let range = scalar_range(&result);
        // Must be `pkg/main`'s declaration (line 2, char 0), not
        // `pkg/other`'s (which begins one byte later in its own file).
        match &result {
            GotoDefinitionResponse::Scalar(loc) => {
                assert!(
                    loc.uri.as_str().ends_with("pkg/main.kio"),
                    "must resolve in pkg/main, got {:?}",
                    loc.uri
                );
            }
            other => panic!("expected scalar; got {other:?}"),
        }
        assert_eq!(range.start.line, 2);
        assert_eq!(range.start.character, 0);
    }
}
