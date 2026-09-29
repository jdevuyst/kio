//! Diagnostic mapping: kio-rs `LocatedError` → LSP `Diagnostic`.
//!
//! The analysis pipeline ([`crate::cmd::check::analyze_workspace_at`])
//! returns one or more [`crate::pass::resolve::LocatedError`] values
//! from the earliest failing phase, paired with the per-file source map
//! collected up to that point. The LSP layer takes each error, picks the right
//! [`crate::lsp::positions::LineIndex`] for the failing file, and
//! converts the `(span, message)` pair into an LSP `Diagnostic`
//! ready to publish.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString,
    Position, Range, Uri,
};

use crate::cmd::check::LspWarning;
use crate::diagnostic::group_secondary_labels;
use crate::error::Applicability;
use crate::error::{Error, Fix, FixReplacementPart, SecondaryLabel};
use crate::lsp::positions::LineIndex;
use crate::pass::resolve::LocatedError;

/// Source string written into every `Diagnostic.source` field so
/// editors group Kio diagnostics under one provider. Editors render
/// this in their problem panel ("kio: type mismatch …").
pub const DIAG_SOURCE: &str = "kio";

/// Map one `LocatedError` to an LSP `Diagnostic` plus the file URI
/// the diagnostic publishes against.
///
/// `file_path` in the `LocatedError` is the path the analysis layer
/// recorded for the failing file (workspace-relative for sources
/// under the workspace root, absolute otherwise — see
/// [`crate::cmd::check::compile_workspace_with_at`]). The LSP server
/// resolves it against the workspace root to recover an absolute
/// path, then renders the `file://` URI.
///
/// `line_index` is the source-file index the LSP server keeps for
/// each open document; the layer that owns the source map looks
/// `file_path` up there and passes it in.
pub fn locate_to_diagnostic(error: &LocatedError, line_index: &LineIndex, uri: &Uri) -> Diagnostic {
    locate_to_diagnostic_inner(error, line_index, uri, None).0
}

/// Map one diagnostic with access to the exact analyzed workspace sources.
/// File-qualified secondary labels use their own line index and URI; missing
/// or invalid related sources are omitted rather than being clamped into the
/// primary document.
pub fn locate_to_diagnostic_with_sources(
    error: &LocatedError,
    line_index: &LineIndex,
    uri: &Uri,
    package_root: &Path,
    sources: &HashMap<PathBuf, String>,
) -> Diagnostic {
    locate_to_diagnostic_inner(error, line_index, uri, Some((package_root, sources))).0
}

pub(crate) fn locate_to_diagnostic_with_sources_counted(
    error: &LocatedError,
    line_index: &LineIndex,
    uri: &Uri,
    package_root: &Path,
    sources: &HashMap<PathBuf, String>,
) -> (Diagnostic, usize) {
    locate_to_diagnostic_inner(error, line_index, uri, Some((package_root, sources)))
}

fn locate_to_diagnostic_inner(
    error: &LocatedError,
    line_index: &LineIndex,
    uri: &Uri,
    related_sources: Option<(&Path, &HashMap<PathBuf, String>)>,
) -> (Diagnostic, usize) {
    let (span, message) = error.error.diag();
    let range_pos = line_index.to_range(span);
    let range = Range {
        start: Position {
            line: range_pos.start.line,
            character: range_pos.start.character,
        },
        end: Position {
            line: range_pos.end.line,
            character: range_pos.end.character,
        },
    };
    let diag_payload = error.error.diagnostic();
    let (related, related_invariant_count) = related_information(
        diag_payload.secondary(),
        &error.file_path,
        line_index,
        uri,
        related_sources,
    );
    let data = diagnostic_data(&error.error, line_index);
    (
        Diagnostic {
            range,
            severity: Some(severity_for(&error.error)),
            code: Some(NumberOrString::Number(error.error.exit_code().as_i32())),
            code_description: None,
            source: Some(DIAG_SOURCE.to_owned()),
            message: message.to_owned(),
            related_information: related,
            tags: None,
            data,
        },
        related_invariant_count,
    )
}

pub fn warning_to_diagnostic(warning: &LspWarning, line_index: &LineIndex) -> Diagnostic {
    let range = span_to_range(warning.span, line_index);
    let data = fixes_data(&warning.fixes, line_index).map(|fixes| {
        let mut data = serde_json::Map::new();
        data.insert("fixes".to_owned(), fixes);
        serde_json::Value::Object(data)
    });
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::WARNING),
        code: None,
        code_description: None,
        source: Some(DIAG_SOURCE.to_owned()),
        message: warning.message.clone(),
        related_information: None,
        tags: None,
        data,
    }
}

/// Severity for an `Error` variant. v1 maps every analysis-pipeline
/// error to `Error` severity — every variant here is a hard
/// compile-time failure that prevents `kio check` from exiting 0.
/// Warnings (e.g. unused-variable) ride a separate channel and land
/// later.
fn severity_for(_err: &Error) -> DiagnosticSeverity {
    DiagnosticSeverity::ERROR
}

fn related_information(
    secondary: &[SecondaryLabel],
    primary_file: &Path,
    line_index: &LineIndex,
    uri: &Uri,
    related_sources: Option<(&Path, &HashMap<PathBuf, String>)>,
) -> (Option<Vec<DiagnosticRelatedInformation>>, usize) {
    let mut related = Vec::new();
    let display_root = related_sources.map(|(package_root, _)| package_root);
    let grouped = group_secondary_labels(secondary, Some(primary_file), display_root);
    for label in grouped.primary {
        related.push(DiagnosticRelatedInformation {
            location: Location {
                uri: uri.clone(),
                range: span_to_range(label.span, line_index),
            },
            message: label.text.clone(),
        });
    }

    let mut related_invariant_count = 0;
    for group in grouped.foreign {
        let file = group.file;
        let labels = group.labels;
        let Some((package_root, sources)) = related_sources else {
            related_invariant_count += labels.len();
            continue;
        };
        let Some(source) = sources.get(file) else {
            related_invariant_count += labels.len();
            continue;
        };
        let Some(related_uri) = path_to_uri(file, package_root) else {
            related_invariant_count += labels.len();
            continue;
        };
        let related_index = LineIndex::new(source);
        for label in labels {
            if !span_fits_source(source, label.span) {
                related_invariant_count += 1;
                continue;
            }
            related.push(DiagnosticRelatedInformation {
                location: Location {
                    uri: related_uri.clone(),
                    range: span_to_range(label.span, &related_index),
                },
                message: label.text.clone(),
            });
        }
    }
    (
        (!related.is_empty()).then_some(related),
        related_invariant_count,
    )
}

fn span_fits_source(source: &str, span: crate::span::Span) -> bool {
    let start = span.start as usize;
    let end = span.end as usize;
    start <= end
        && end <= source.len()
        && source.is_char_boundary(start)
        && source.is_char_boundary(end)
}

fn diagnostic_data(error: &Error, line_index: &LineIndex) -> Option<serde_json::Value> {
    let diag = error.diagnostic();
    let mut data = serde_json::Map::new();
    if let Some(help) = diag.help() {
        data.insert(
            "help".to_owned(),
            serde_json::Value::String(help.to_owned()),
        );
    }
    if !diag.notes().is_empty() {
        data.insert("notes".to_owned(), serde_json::json!(diag.notes()));
    }
    if let Some(name) = diag.unresolved_name() {
        data.insert(
            "unresolvedName".to_owned(),
            serde_json::Value::String(name.to_owned()),
        );
    }
    if let Some(fixes) = fixes_data(diag.fixes(), line_index) {
        data.insert("fixes".to_owned(), fixes);
    }
    (!data.is_empty()).then_some(serde_json::Value::Object(data))
}

fn fixes_data(fixes: &[Fix], line_index: &LineIndex) -> Option<serde_json::Value> {
    let fixes: Vec<_> = fixes
        .iter()
        .filter(|fix| fix.edits.iter().all(|edit| edit.file.is_none()))
        .map(|fix| {
            let applicability = match fix.applicability {
                Applicability::MachineApplicable => "machineApplicable",
                Applicability::MaybeIncorrect => "maybeIncorrect",
            };
            let mut data = serde_json::json!({
                "title": fix.title,
                "applicability": applicability,
                "scaffold": fix.scaffold,
                "edits": fix.edits.iter().map(|edit| {
                    let mut data = serde_json::json!({
                        "range": span_to_range(edit.span, line_index),
                    });
                    let object = data.as_object_mut().expect("edit data is an object");
                    if edit.replacement_parts.is_empty() {
                        object.insert(
                            "replacement".to_owned(),
                            serde_json::Value::String(edit.replacement.clone()),
                        );
                    } else {
                        object.insert(
                            "replacementParts".to_owned(),
                            serde_json::Value::Array(
                                edit.replacement_parts
                                    .iter()
                                    .map(|part| match part {
                                        FixReplacementPart::Text(text) => {
                                            serde_json::json!({ "text": text })
                                        }
                                        FixReplacementPart::Source(span) => serde_json::json!({
                                            "sourceRange": span_to_range(*span, line_index),
                                        }),
                                    })
                                    .collect(),
                            ),
                        );
                    }
                    if !edit.required_whitespace.is_empty() {
                        object.insert(
                            "requiredWhitespace".to_owned(),
                            serde_json::Value::Array(
                                edit.required_whitespace
                                    .iter()
                                    .map(|span| {
                                        serde_json::to_value(span_to_range(*span, line_index))
                                            .expect("LSP range serializes")
                                    })
                                    .collect(),
                            ),
                        );
                    }
                    data
                }).collect::<Vec<_>>(),
            });
            if let Some(scope) = fix.follow_on_reanalysis_scope {
                data.as_object_mut().expect("fix data is an object").insert(
                    "followOnReanalysisOutside".to_owned(),
                    serde_json::to_value(span_to_range(scope, line_index))
                        .expect("LSP range serializes"),
                );
            }
            data
        })
        .collect();
    (!fixes.is_empty()).then_some(serde_json::json!(fixes))
}

fn span_to_range(span: crate::span::Span, line_index: &LineIndex) -> Range {
    let range_pos = line_index.to_range(span);
    Range {
        start: Position {
            line: range_pos.start.line,
            character: range_pos.start.character,
        },
        end: Position {
            line: range_pos.end.line,
            character: range_pos.end.character,
        },
    }
}

/// Build a `file://` URI from a possibly-relative path, resolving it
/// against `workspace_root` if necessary. The result is the URI the
/// LSP client expects in `publishDiagnostics` (LSP file URIs always
/// carry the `file://` scheme).
///
/// On Windows, paths use `\` separators; the URI form uses `/`. The
/// helper replaces them inline. Drive letters get a leading `/`
/// (`file:///C:/foo`), while UNC paths put the server in the URI authority
/// (`file://server/share/foo`).
///
/// Returns `None` if the path can't be encoded — for example a
/// non-UTF-8 path on a platform whose `OsStr` doesn't round-trip.
pub fn path_to_uri(path: &Path, workspace_root: &Path) -> Option<Uri> {
    let abs: PathBuf = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    let uri = file_uri_string(abs.to_str()?, cfg!(windows));
    Uri::from_str_lossy(&uri)
}

fn file_uri_string(path: &str, windows: bool) -> String {
    let normalized;
    let path = if windows {
        if let Some(rest) = path
            .strip_prefix(r"\\?\UNC\")
            .or_else(|| path.strip_prefix(r"\\?\unc\"))
        {
            normalized = format!("//{}", rest.replace('\\', "/"));
        } else {
            normalized = path
                .strip_prefix(r"\\?\")
                .unwrap_or(path)
                .replace('\\', "/");
        }
        normalized.as_str()
    } else {
        path
    };

    let mut uri = String::with_capacity(path.len() + 12);
    if windows && let Some(unc) = path.strip_prefix("//") {
        let (host, tail) = unc.split_once('/').unwrap_or((unc, ""));
        uri.push_str("file://");
        push_uri_component(&mut uri, host.as_bytes(), false);
        uri.push('/');
        push_uri_component(&mut uri, tail.as_bytes(), true);
        return uri;
    }

    uri.push_str(if windows { "file:///" } else { "file://" });
    push_uri_component(&mut uri, path.as_bytes(), true);
    uri
}

fn push_uri_component(uri: &mut String, bytes: &[u8], allow_colon: bool) {
    // Encode path bytes rather than selected characters so the inverse URI
    // decoder cannot reinterpret a literal `%xx`, `#`, or `?` in a filename
    // as URI syntax. `/` remains the path separator and `:` remains available
    // for a Windows drive prefix.
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in bytes {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
            || (allow_colon && byte == b':')
        {
            uri.push(char::from(byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[(byte >> 4) as usize]));
            uri.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
}

/// Helper trait carving out a forgiving constructor for `Uri`. The
/// lsp-types `Uri::from_str` returns the raw `fluent_uri::ParseError`
/// which doesn't add information at the call site; this wrapper
/// converts to `Option` so the caller can decide what to do on
/// failure (drop the diagnostic, log a warning, …).
trait UriFromStrLossy {
    fn from_str_lossy(s: &str) -> Option<Self>
    where
        Self: Sized;
}

impl UriFromStrLossy for Uri {
    fn from_str_lossy(s: &str) -> Option<Self> {
        use std::str::FromStr;
        Uri::from_str(s).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error as KioError;
    use crate::lsp::util::test_file_uri;
    use crate::span::Span;
    use std::str::FromStr;

    fn uri(s: &str) -> Uri {
        match s.strip_prefix("file:///") {
            Some(path) => test_file_uri(format!("/{path}")),
            None => Uri::from_str(s).expect("uri"),
        }
    }

    #[test]
    fn type_error_maps_to_diagnostic() {
        let source = "fn x() -> . { 1 }";
        let idx = LineIndex::new(source);
        let err = LocatedError {
            file_path: PathBuf::from("foo.kio"),
            error: KioError::type_(Span::new(15, 16), "expected () but found I32".to_owned()),
        };
        let diag = locate_to_diagnostic(&err, &idx, &uri("file:///tmp/foo.kio"));
        assert_eq!(diag.range.start.line, 0);
        assert_eq!(diag.range.start.character, 15);
        assert_eq!(diag.range.end.line, 0);
        assert_eq!(diag.range.end.character, 16);
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.code, Some(NumberOrString::Number(14)));
        assert_eq!(diag.source.as_deref(), Some("kio"));
        assert_eq!(diag.message, "expected () but found I32");
    }

    #[test]
    fn parse_error_maps_to_diagnostic() {
        let source = "fn x() {";
        let idx = LineIndex::new(source);
        let err = LocatedError {
            file_path: PathBuf::from("foo.kio"),
            error: KioError::parse(Span::new(7, 8), "unexpected token".to_owned()),
        };
        let diag = locate_to_diagnostic(&err, &idx, &uri("file:///tmp/foo.kio"));
        assert_eq!(diag.range.start.character, 7);
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.code, Some(NumberOrString::Number(11)));
    }

    #[test]
    fn span_across_lines_maps_correctly() {
        let source = "fn x() -> . {\n  1\n}";
        let idx = LineIndex::new(source);
        let err = LocatedError {
            file_path: PathBuf::from("foo.kio"),
            error: KioError::type_(Span::new(16, 17), "type mismatch".to_owned()), // span is the "1"
        };
        let diag = locate_to_diagnostic(&err, &idx, &uri("file:///tmp/foo.kio"));
        assert_eq!(diag.range.start.line, 1);
        assert_eq!(diag.range.start.character, 2);
    }

    #[test]
    fn secondary_labels_map_to_related_information() {
        let source = "fn f() {}\nfn f() {}\n";
        let idx = LineIndex::new(source);
        let first = Span::new(3, 4);
        let second = Span::new(13, 14);
        let err = LocatedError {
            file_path: PathBuf::from("foo.kio"),
            error: KioError::name_res(second, "`f` already declared")
                .with_secondary(first, "`f` first declared here"),
        };
        let diag_uri = uri("file:///tmp/foo.kio");
        let diag = locate_to_diagnostic(&err, &idx, &diag_uri);
        let related = diag
            .related_information
            .expect("secondary label should map to related information");
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].location.uri, diag_uri);
        assert_eq!(related[0].location.range.start.line, 0);
        assert_eq!(related[0].location.range.start.character, 3);
        assert_eq!(related[0].message, "`f` first declared here");
    }

    #[test]
    fn cross_file_secondary_uses_its_own_uri_and_line_index() {
        let root = std::path::absolute("/tmp/pkg").expect("absolute fixture root");
        let primary_source = "fn use(value: Wrap(_)) { value }\n";
        let provider_source = "module provider;\npub type Wrap[T] = [A] T -> A;\n";
        let primary_index = LineIndex::new(primary_source);
        let primary = primary_source.find('_').unwrap() as u32;
        let binder = provider_source.find("[A]").unwrap() as u32 + 1;
        let provider_path = PathBuf::from("src/provider.kio");
        let err = LocatedError {
            file_path: PathBuf::from("src/caller.kio"),
            error: KioError::type_(Span::new(primary, primary + 1), "invalid placeholder")
                .with_secondary_in_file(
                    provider_path.clone(),
                    Span::new(binder, binder + 1),
                    "provider binder",
                ),
        };
        let sources = HashMap::from([(provider_path, provider_source.to_owned())]);

        let diag = locate_to_diagnostic_with_sources(
            &err,
            &primary_index,
            &uri("file:///tmp/pkg/src/caller.kio"),
            &root,
            &sources,
        );
        let related = diag.related_information.expect("cross-file label");
        assert_eq!(related.len(), 1);
        assert_eq!(
            related[0].location.uri,
            uri("file:///tmp/pkg/src/provider.kio")
        );
        assert_eq!(related[0].location.range.start.line, 1);
        assert_eq!(related[0].location.range.start.character, 20);
        assert_eq!(related[0].message, "provider binder");
    }

    #[test]
    fn cross_file_secondary_with_missing_or_invalid_source_is_omitted() {
        let root = std::path::absolute("/tmp/pkg").expect("absolute fixture root");
        let primary_source = "fn use(value: Wrap(_)) { value }\n";
        let primary_index = LineIndex::new(primary_source);
        let primary = primary_source.find('_').unwrap() as u32;
        let provider_path = PathBuf::from("src/provider.kio");
        let err = LocatedError {
            file_path: PathBuf::from("src/caller.kio"),
            error: KioError::type_(Span::new(primary, primary + 1), "invalid placeholder")
                .with_secondary_in_file(
                    provider_path.clone(),
                    Span::new(99, 100),
                    "provider binder",
                ),
        };
        let sources = HashMap::from([(provider_path, "short".to_owned())]);

        let (invalid, invalid_count) = locate_to_diagnostic_with_sources_counted(
            &err,
            &primary_index,
            &uri("file:///tmp/pkg/src/caller.kio"),
            &root,
            &sources,
        );
        assert!(invalid.related_information.is_none());
        assert_eq!(invalid_count, 1);
        let (missing, missing_count) = locate_to_diagnostic_with_sources_counted(
            &err,
            &primary_index,
            &uri("file:///tmp/pkg/src/caller.kio"),
            &root,
            &HashMap::new(),
        );
        assert!(missing.related_information.is_none());
        assert_eq!(missing_count, 1);
    }

    #[test]
    fn primary_only_wrapper_never_maps_a_foreign_span_to_the_primary_uri() {
        let source = "fn use(value: Wrap(_)) { value }\n";
        let index = LineIndex::new(source);
        let primary = source.find('_').unwrap() as u32;
        let err = LocatedError {
            file_path: PathBuf::from("src/caller.kio"),
            error: KioError::type_(Span::new(primary, primary + 1), "invalid placeholder")
                .with_secondary_in_file("src/provider.kio", Span::new(0, 1), "provider binder"),
        };

        let diag = locate_to_diagnostic(&err, &index, &uri("file:///tmp/pkg/src/caller.kio"));
        assert!(diag.related_information.is_none());
    }

    #[test]
    fn related_information_keeps_primary_then_stable_file_and_span_order() {
        let root = std::path::absolute("/workspace").expect("absolute fixture root");
        let primary_source = "fn caller() { bad }\n";
        let primary_index = LineIndex::new(primary_source);
        let alpha_path = PathBuf::from("src/alpha.kio");
        let beta_path = PathBuf::from("src/beta.kio");
        let err = LocatedError {
            file_path: PathBuf::from("src/caller.kio"),
            error: KioError::type_(Span::new(14, 17), "invalid placeholder")
                .with_secondary(Span::new(3, 9), "primary label")
                .with_secondary_in_file(beta_path.clone(), Span::new(0, 4), "beta label")
                .with_secondary_in_file(alpha_path.clone(), Span::new(12, 18), "second alpha label")
                .with_secondary_in_file(alpha_path.clone(), Span::new(0, 5), "first alpha label"),
        };
        let sources = HashMap::from([
            (beta_path, "only beta\n".to_owned()),
            (alpha_path, "first alpha\nsecond alpha\n".to_owned()),
        ]);

        let diag = locate_to_diagnostic_with_sources(
            &err,
            &primary_index,
            &uri("file:///workspace/src/caller.kio"),
            &root,
            &sources,
        );
        let related = diag.related_information.expect("related information");
        let messages = related
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            [
                "primary label",
                "first alpha label",
                "second alpha label",
                "beta label",
            ]
        );
        assert_eq!(
            related[1].location.uri,
            uri("file:///workspace/src/alpha.kio")
        );
        assert_eq!(
            related[3].location.uri,
            uri("file:///workspace/src/beta.kio")
        );
    }

    #[test]
    fn fix_maps_to_diagnostic_data() {
        let source = "fn f() { hepler }\n";
        let idx = LineIndex::new(source);
        let typo = Span::new(9, 15);
        let err = LocatedError {
            file_path: PathBuf::from("foo.kio"),
            error: KioError::name_res(typo, "unknown name `hepler`")
                .with_help("did you mean `helper`?")
                .with_note("names are case-sensitive")
                .with_suggestion(typo, "helper"),
        };
        let diag = locate_to_diagnostic(&err, &idx, &uri("file:///tmp/foo.kio"));
        let data = diag.data.expect("fix should map to data");
        assert_eq!(
            data.get("help").and_then(serde_json::Value::as_str),
            Some("did you mean `helper`?")
        );
        assert_eq!(
            data.get("notes")
                .and_then(serde_json::Value::as_array)
                .and_then(|notes| notes.first())
                .and_then(serde_json::Value::as_str),
            Some("names are case-sensitive")
        );
        let fix = data
            .get("fixes")
            .and_then(serde_json::Value::as_array)
            .and_then(|fixes| fixes.first())
            .expect("fix object");
        assert_eq!(
            fix.get("applicability").and_then(serde_json::Value::as_str),
            Some("machineApplicable")
        );
        assert_eq!(
            fix.get("edits")
                .and_then(serde_json::Value::as_array)
                .and_then(|edits| edits.first())
                .and_then(|edit| edit.get("replacement"))
                .and_then(serde_json::Value::as_str),
            Some("helper")
        );
    }

    #[test]
    fn empty_ufcs_tail_diagnostic_exposes_both_repairs() {
        let source = "module main; fn run() -> . { value.>call() }";
        let error = crate::pass::parser::parse(source)
            .expect_err("the full parser must reject a written empty UFCS tail");
        let located = LocatedError {
            file_path: PathBuf::from("main.kio"),
            error,
        };
        let diagnostic =
            locate_to_diagnostic(&located, &LineIndex::new(source), &uri("file:///main.kio"));
        assert_eq!(
            diagnostic.message,
            "an explicitly empty UFCS argument list is ambiguous"
        );
        let data = diagnostic.data.expect("diagnostic data");
        assert_eq!(
            data.get("help").and_then(serde_json::Value::as_str),
            Some(
                "remove this argument list to write the bare UFCS form, or replace it with `(())` to pass Unit"
            )
        );
        let fixes = data
            .get("fixes")
            .and_then(serde_json::Value::as_array)
            .expect("both parser repairs must reach LSP code actions");
        assert_eq!(fixes.len(), 2);
        assert_eq!(fixes[0]["title"], "Pass Unit explicitly");
        assert_eq!(fixes[0]["applicability"], "maybeIncorrect");
        assert_eq!(fixes[0]["edits"][0]["replacement"], "(())");
        assert_eq!(fixes[1]["title"], "Use the bare UFCS form");
        assert_eq!(fixes[1]["applicability"], "maybeIncorrect");
        assert_eq!(fixes[1]["edits"][0]["replacement"], "");
    }

    #[test]
    fn empty_ufcs_tail_trivia_repair_preserves_comment_and_comma() {
        let source = "module main; fn run() -> . { value.>call(\n// keep this\n,) }";
        let error = crate::pass::parser::parse(source)
            .expect_err("the full parser must reject a written empty UFCS tail");
        let located = LocatedError {
            file_path: PathBuf::from("main.kio"),
            error,
        };
        let line_index = LineIndex::new(source);
        let diagnostic = locate_to_diagnostic(&located, &line_index, &uri("file:///main.kio"));
        let data = diagnostic.data.expect("diagnostic data");
        let fixes = data
            .get("fixes")
            .and_then(serde_json::Value::as_array)
            .expect("the trivia-preserving Unit repair must reach LSP code actions");
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0]["title"], "Pass Unit explicitly");
        let edit = &fixes[0]["edits"][0];
        assert_eq!(edit["replacement"], "()");
        let range: Range = serde_json::from_value(edit["range"].clone()).expect("LSP range");
        let start = line_index.position_to_offset(crate::lsp::positions::LspPosition {
            line: range.start.line,
            character: range.start.character,
        });
        let end = line_index.position_to_offset(crate::lsp::positions::LspPosition {
            line: range.end.line,
            character: range.end.character,
        });
        let mut repaired = source.to_owned();
        repaired.replace_range(start as usize..end as usize, "()");
        assert!(repaired.contains("// keep this"));
        assert_eq!(repaired.matches(',').count(), source.matches(',').count());
        crate::pass::parser::parse(&repaired)
            .expect("the applied LSP Unit repair must produce valid source");
    }

    #[cfg(unix)]
    #[test]
    fn path_to_uri_absolute() {
        let uri = path_to_uri(Path::new("/tmp/foo.kio"), Path::new("/tmp"));
        assert_eq!(uri.unwrap().as_str(), "file:///tmp/foo.kio");
    }

    #[cfg(unix)]
    #[test]
    fn path_to_uri_relative_resolves_against_workspace_root() {
        let uri = path_to_uri(Path::new("src/foo.kio"), Path::new("/tmp/pkg"));
        assert_eq!(uri.unwrap().as_str(), "file:///tmp/pkg/src/foo.kio");
    }

    #[cfg(unix)]
    #[test]
    fn path_to_uri_encodes_spaces() {
        let uri = path_to_uri(Path::new("/tmp/with space/foo.kio"), Path::new("/tmp"));
        assert_eq!(uri.unwrap().as_str(), "file:///tmp/with%20space/foo.kio");
    }

    #[cfg(unix)]
    #[test]
    fn path_to_uri_round_trips_reserved_percent_and_unicode_bytes() {
        let path = Path::new("/tmp/a b/%2F/#hash/?query/é.kio");
        let uri = path_to_uri(path, Path::new("/tmp")).expect("encoded file URI");
        assert!(uri.as_str().contains("%252F"));
        assert!(uri.as_str().contains("%23hash"));
        assert!(uri.as_str().contains("%3Fquery"));
        assert!(uri.as_str().contains("%C3%A9.kio"));
        assert_eq!(crate::lsp::util::uri_to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn windows_file_uri_string_distinguishes_drive_and_unc_roots() {
        assert_eq!(
            file_uri_string(r"C:\workspace\main.kio", true),
            "file:///C:/workspace/main.kio"
        );
        assert_eq!(
            file_uri_string(r"\\server\share\main.kio", true),
            "file://server/share/main.kio"
        );
        assert_eq!(
            file_uri_string(r"\\?\UNC\server\share\main.kio", true),
            "file://server/share/main.kio"
        );
    }

    #[test]
    fn windows_unc_file_uri_round_trips_an_encoded_server_name() {
        let encoded = file_uri_string(r"\\sér ver%\share\main.kio", true);
        assert_eq!(encoded, "file://s%C3%A9r%20ver%25/share/main.kio");
        let uri = Uri::from_str_lossy(&encoded).expect("encoded UNC URI");
        let decoded = crate::lsp::util::uri_to_path(&uri).expect("decoded UNC path");
        let rendered = decoded.to_string_lossy();
        assert!(rendered.contains("sér ver%"));
        assert!(rendered.contains("share"));
        assert!(rendered.contains("main.kio"));
    }
}
