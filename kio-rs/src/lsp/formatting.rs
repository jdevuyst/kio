//! Implementation of `textDocument/formatting`.
//!
//! Strategy: parse the open document's text and its written import grammars,
//! without reading provider modules, and return
//! a single full-file [`lsp_types::TextEdit`] that replaces the entire
//! buffer with the canonical form. Using one whole-file replace
//! is correct by definition — `kio fmt`'s contract is whole-file
//! canonicalization — and avoids the complexity of a minimal-diff
//! algorithm for v1.
//!
//! Three behaviors, per the spec:
//!
//! - **Non-canonical input.** Returns a single `TextEdit` spanning the
//!   entire document, with `newText` set to the canonical form.
//! - **Already-canonical input.** Returns an empty edit list — the
//!   overlay text equals the canonical form, so there is nothing to
//!   apply.
//! - **Parse error.** Returns an empty edit list. The diagnostics
//!   publishing path already reports parse errors; this handler must
//!   not double-report, and a JSON-RPC error response for an invalid
//!   buffer would be surprising to the editor (the LSP convention for
//!   formatting on unparseable input is a silent no-op).
//!
//! **File kind.** The formatter routes by filename suffix
//! (`.pkg.kio`, `.sig.kio`, `.dep.kio`, `.lock.kio`, or plain `.kio`).
//! A real file uses its decoded lexical path's filename; an `untitled:` or
//! other non-file buffer uses the decoded final URI-path component and falls
//! back to plain `.kio` treatment when that component has no known suffix.

use lsp_types::{Position, Range, TextEdit, Uri};

use std::path::Path;

use crate::cmd::fmt::{format_source, format_source_with_file_context};
use crate::error::Error;
use crate::lsp::positions::LineIndex;
use crate::lsp::util::filename_from_uri;

/// Handle one `textDocument/formatting` request.
///
/// - `uri` — the document URI; used to extract the filename for
///   file-kind routing (`.pkg.kio`, `.sig.kio`, `.dep.kio`,
///   `.lock.kio`, or plain `.kio`).
/// - `source` — the overlay text for the document (the editor's
///   in-memory buffer, not disk). The caller is responsible for
///   obtaining this via the overlay store.
///
/// Returns a list of `TextEdit`s to apply. An empty list means either
/// the text is already canonical or a parse error occurred (both are
/// silent no-ops for the editor). A non-empty list contains exactly
/// one full-file replacement edit.
pub fn handle_formatting(uri: &Uri, source: &str) -> Vec<TextEdit> {
    // Extract the filename from the URI for file-kind routing.
    // For `untitled:` / `git:` / other non-file URIs there is no
    // meaningful path component, so fall back to empty-string which
    // `format_source` treats as plain `.kio` module formatting.
    let filename = filename_from_uri(uri);

    let canonical = match format_source(&filename, source) {
        Ok(s) => s,
        // Parse error: return an empty edit list (silent no-op).
        // The diagnostics channel already surfaces parse errors;
        // there's no value in also returning an error response here.
        Err(_) => return Vec::new(),
    };

    edits_for_canonical(source, canonical)
}

pub fn try_handle_formatting(uri: &Uri, source: &str) -> Result<Vec<TextEdit>, Error> {
    let filename = filename_from_uri(uri);
    let canonical = format_source(&filename, source)?;
    Ok(edits_for_canonical(source, canonical))
}

pub(crate) fn handle_formatting_with_file_context(
    path: &Path,
    source: &str,
) -> Result<Vec<TextEdit>, Error> {
    let canonical = format_source_with_file_context(path, source)?;
    Ok(edits_for_canonical(source, canonical))
}

fn edits_for_canonical(source: &str, canonical: String) -> Vec<TextEdit> {
    // Idempotence: if the canonical form equals the overlay text,
    // there are no edits to apply.
    if canonical == source {
        return Vec::new();
    }

    // Build one full-file replacement TextEdit. The range spans from
    // (0, 0) to the position after the very last character.
    let line_index = LineIndex::new(source);
    let end_pos = line_index.to_position(source.len() as u32);

    let full_range = Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: end_pos.line,
            character: end_pos.character,
        },
    };

    vec![TextEdit {
        range: full_range,
        new_text: canonical,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn file_uri(path: &str) -> Uri {
        Uri::from_str(&format!("file://{path}")).unwrap()
    }

    #[test]
    fn already_canonical_returns_empty() {
        // `kio fmt` is idempotent; running it on already-canonical
        // source should produce no edits.
        let source = "module a;\n\nfn id[A](x: A) -> A {\n    x\n}\n";
        // Run format once to get the canonical form.
        let canonical = format_source("a.kio", source).expect("parse ok");
        let uri = file_uri("/tmp/a.kio");
        let edits = handle_formatting(&uri, &canonical);
        assert!(
            edits.is_empty(),
            "already-canonical source must produce no edits; got: {edits:?}"
        );
    }

    #[test]
    fn non_canonical_returns_single_full_file_edit() {
        // A module body where fn spacing is wrong: the canonical form
        // has blank-line separation between items, so a compacted
        // source will produce one replacement edit.
        let source = "module a;\nfn id[A](x: A) -> A { x }\n";
        let uri = file_uri("/tmp/a.kio");
        let edits = handle_formatting(&uri, source);
        assert_eq!(
            edits.len(),
            1,
            "non-canonical source must produce exactly one edit; got: {edits:?}"
        );
        let edit = &edits[0];
        // The range must start at (0, 0).
        assert_eq!(edit.range.start.line, 0);
        assert_eq!(edit.range.start.character, 0);
        // Applying the edit gives the canonical form.
        assert_eq!(
            edit.new_text,
            format_source("a.kio", source).expect("parse ok"),
            "edit new_text must be the canonical form"
        );
    }

    #[test]
    fn parse_error_returns_empty() {
        // Malformed source: the formatter encounters a parse error and
        // must return an empty edit list (no JSON-RPC error).
        let source = "module a;\n\nfn ())(\n";
        let uri = file_uri("/tmp/a.kio");
        let edits = handle_formatting(&uri, source);
        assert!(
            edits.is_empty(),
            "parse-broken source must produce no edits; got: {edits:?}"
        );
    }

    #[test]
    fn routes_package_file_by_uri_suffix() {
        let source = "package pkg; bridge { pkg/main; }";
        let uri = file_uri("/tmp/pkg.pkg.kio");
        let edits = handle_formatting(&uri, source);
        assert_eq!(edits.len(), 1, "a valid noncanonical package must edit");
        assert_eq!(
            edits[0].new_text,
            "package pkg;\n\nbridge {\n  pkg/main\n}\n"
        );
        assert!(handle_formatting(&uri, &edits[0].new_text).is_empty());
    }

    #[test]
    fn sig_file_uri_routes_to_signature_formatter() {
        let source = "signature   app   v(1) ;\n";
        let uri = file_uri("/tmp/app.sig.kio");
        let edits = handle_formatting(&uri, source);
        assert_eq!(edits.len(), 1, "non-canonical .sig.kio must edit");
        assert_eq!(edits[0].new_text, "signature app v(1);\n");
    }

    #[test]
    fn dep_file_uri_routes_to_dependency_formatter() {
        let source = "dependency lib ;\n\nsource { path \"../lib/lib.pkg.kio\" ; }\n";
        let uri = file_uri("/tmp/lib.dep.kio");
        let edits = handle_formatting(&uri, source);
        assert_eq!(edits.len(), 1, "non-canonical .dep.kio must edit");
        assert_eq!(
            edits[0].new_text,
            "dependency lib;\n\nsource {\n  path \"../lib/lib.pkg.kio\"\n}\n"
        );
    }

    #[test]
    fn lock_file_uri_routes_to_lock_formatter() {
        let source =
            "lock lib ;\n\nresolved { git \"u\" ; ref \"main\" ; commit \"abc\" ; sig \"d\" ; }\n";
        let uri = file_uri("/tmp/lib.lock.kio");
        let edits = handle_formatting(&uri, source);
        assert_eq!(edits.len(), 1, "non-canonical .lock.kio must edit");
        assert_eq!(
            edits[0].new_text,
            "lock lib;\n\nresolved {\n  git \"u\";\n  ref \"main\";\n  commit \"abc\";\n  sig \"d\"\n}\n"
        );
    }

    #[test]
    fn filename_from_uri_extracts_last_segment() {
        let uri = file_uri("/path/to/foo.pkg.kio");
        assert_eq!(filename_from_uri(&uri), "foo.pkg.kio");
    }

    #[test]
    fn filename_from_uri_handles_no_slash() {
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        assert_eq!(filename_from_uri(&uri), "Untitled-1");
    }

    #[test]
    fn untitled_imported_variadic_grammar_formats_without_provider_context() {
        let source = "module main;\nimport syntax(op _ => _, varop [% %]);\nfn pairs(k:A,v:A)->A{[% k=>v %]}\n";
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        let edits = try_handle_formatting(&uri, source).expect("consumer grammar is complete");
        assert_eq!(edits.len(), 1);
        assert!(edits[0].new_text.contains("[% k => v %]"));
        assert!(
            try_handle_formatting(&uri, &edits[0].new_text)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn file_imported_variadic_grammar_ignores_provider_state() {
        let temp = tempfile::tempdir().expect("source tree");
        let consumer = temp.path().join("a/b/c.kio");
        let provider = temp.path().join("a/d/e.kio");
        std::fs::create_dir_all(provider.parent().unwrap()).unwrap();
        let source = "module a/b/c;\nimport a/d/e(op _ => _, varop [% %]);\nfn pairs(k:A,v:A)->A{[% k=>v %]}\n";
        let mut canonical = None;
        for provider_source in [
            None,
            Some("module a/d/e; fn broken( {"),
            Some(
                "module a/d/e; pub op _ => _ { impl entry; }; pub varop [% %] { foldl pair empty; };",
            ),
        ] {
            if let Some(text) = provider_source {
                std::fs::write(&provider, text).unwrap();
            }
            let (edits, attempts) = crate::package_collection::import_grammar_with_denied_reads(
                [provider.clone()],
                || handle_formatting_with_file_context(&consumer, source).unwrap(),
            );
            assert!(attempts.is_empty());
            assert_eq!(edits.len(), 1);
            assert!(edits[0].new_text.contains("[% k => v %]"));
            if let Some(previous) = &canonical {
                assert_eq!(&edits[0].new_text, previous);
            }
            canonical = Some(edits[0].new_text.clone());
        }
    }

    #[test]
    fn formatting_parses_repeated_grammar_without_semantic_origin_validation() {
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        for second_provider in ["binary", "other"] {
            let source = format!(
                "module main; import binary(op _ ? _); import {second_provider}(op _ ? _); fn choose(a: ., b: .) -> . {{ a ? b }}"
            );
            let edits = try_handle_formatting(&uri, &source).expect("formatting is syntax-only");
            assert_eq!(edits.len(), 1);
            assert!(edits[0].new_text.contains("a ? b"));
        }

        let broken = "module main; import binary(op _ ? _); fn broken() -> . { ( }";
        assert!(matches!(
            try_handle_formatting(&uri, broken),
            Err(Error::Parse(_))
        ));
    }
}
