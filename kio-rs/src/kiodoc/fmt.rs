//! Markdown Kiodoc snippet formatter.
//!
//! This module formats the snippet bodies that `document` has already
//! classified as Kiodoc snippets. Harnessed snippets are formatted by
//! assembling the same virtual files that validation uses, placing
//! temporary comment markers around the snippet contribution, formatting
//! those files with the normal Kio formatter, and extracting the marked
//! region only when the markers survive unambiguously.

use crate::ast::KioFileKind;
use crate::cmd::fmt as source_fmt;
use crate::error::Error;

use super::document::{DocError, Document, DocumentFence, Snippet};
use super::validate::{self, AssembledFile};

const BEGIN_MARKER: &str = "__KIODOC_FMT_BEGIN__";
const END_MARKER: &str = "__KIODOC_FMT_END__";
const BEGIN_COMMENT: &str = "// __KIODOC_FMT_BEGIN__";
const END_COMMENT: &str = "// __KIODOC_FMT_END__";

#[derive(Debug, Clone)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

pub fn format_document(source: &str, document: &Document) -> Result<Vec<Edit>, Vec<DocError>> {
    let mut edits = Vec::new();
    let mut errors = Vec::new();
    let mut accum_members: std::collections::HashMap<String, Vec<Snippet>> =
        std::collections::HashMap::new();
    for snippet in document.snippets() {
        if let Some(name) = &snippet.harness_ref
            && document
                .harnesses
                .get(name)
                .is_some_and(|harness| harness.accumulate)
        {
            accum_members
                .entry(name.clone())
                .or_default()
                .push(snippet.clone());
        }
    }

    for fence in &document.fences {
        let DocumentFence::Snippet(snippet) = fence else {
            continue;
        };
        if snippet.ignored {
            continue;
        }
        let members = snippet
            .harness_ref
            .as_ref()
            .and_then(|name| accum_members.get(name))
            .map(Vec::as_slice);
        match format_snippet(document, snippet, members) {
            Ok(Some(replacement)) if replacement != snippet.body => {
                let start = snippet.body_offset as usize;
                let end = start + snippet.body.len();
                if end <= source.len() {
                    edits.push(Edit {
                        start,
                        end,
                        replacement,
                    });
                } else {
                    errors.push(DocError {
                        span: snippet.span,
                        message: format!(
                            "snippet at line {} has an invalid source range for formatting",
                            snippet.open_line
                        ),
                    });
                }
            }
            Ok(_) => {}
            Err(message) => errors.push(DocError {
                span: snippet.span,
                message,
            }),
        }
    }

    if errors.is_empty() {
        Ok(edits)
    } else {
        Err(errors)
    }
}

pub fn apply_edits(source: &str, edits: &[Edit]) -> String {
    if edits.is_empty() {
        return source.to_owned();
    }
    let mut out = source.to_owned();
    let mut sorted = edits.to_vec();
    sorted.sort_by_key(|edit| edit.start);
    for edit in sorted.into_iter().rev() {
        out.replace_range(edit.start..edit.end, &edit.replacement);
    }
    out
}

fn format_snippet(
    document: &Document,
    snippet: &Snippet,
    accum_members: Option<&[Snippet]>,
) -> Result<Option<String>, String> {
    let validation_body = validate::validation_body(snippet);
    let formatted = if snippet.variant == KioFileKind::Module {
        format_module_snippet(document, snippet, &validation_body, accum_members)?
    } else {
        format_direct(snippet.variant, &validation_body, snippet)?
    };
    map_formatted_body(snippet, &validation_body, formatted)
}

fn format_module_snippet(
    document: &Document,
    snippet: &Snippet,
    validation_body: &str,
    accum_members: Option<&[Snippet]>,
) -> Result<String, String> {
    if let Some(members) = accum_members {
        return format_accumulating_module_snippet(document, snippet, members);
    }

    if snippet.harness_ref.is_none() && starts_with_module_header(validation_body) {
        return format_direct(KioFileKind::Module, validation_body, snippet);
    }

    if snippet.harness_ref.is_none() && starts_with_package_section(validation_body) {
        return Err(format!(
            "standalone snippet at line {} is not formattable in place: \
             package-section snippets span more than one virtual Kio file",
            snippet.open_line
        ));
    }

    format_via_assembled_marker(document, snippet, validation_body)
}

fn format_accumulating_module_snippet(
    document: &Document,
    current: &Snippet,
    members: &[Snippet],
) -> Result<String, String> {
    let mut aggregate = String::new();
    for (index, member) in members.iter().enumerate() {
        if index > 0 && !aggregate.ends_with('\n') {
            aggregate.push('\n');
        }
        let body = validate::validation_body(member);
        if same_snippet(member, current) {
            aggregate.push_str(&marked_body(&body));
        } else {
            aggregate.push_str(&body);
        }
    }
    format_marked_assembled_body(document, current, &aggregate)
}

fn format_direct(kind: KioFileKind, body: &str, snippet: &Snippet) -> Result<String, String> {
    source_fmt::format_source_as_kind(kind, body)
        .map(canonical_fence_body)
        .map_err(|err| format_parse_error(snippet, &err))
}

fn format_via_assembled_marker(
    document: &Document,
    snippet: &Snippet,
    validation_body: &str,
) -> Result<String, String> {
    let marked = marked_body(validation_body);
    format_marked_assembled_body(document, snippet, &marked)
}

fn format_marked_assembled_body(
    document: &Document,
    snippet: &Snippet,
    marked_body: &str,
) -> Result<String, String> {
    let assembled = validate::assemble_with_snippet_body(document, snippet, marked_body)
        .map_err(|message| format!("snippet at line {}: {message}", snippet.open_line))?;

    let mut extracted = None;
    for file in assembled.files {
        let kind = assembled_file_kind(&file).ok_or_else(|| {
            format!(
                "snippet at line {} produced an unknown virtual Kio file `{}`",
                snippet.open_line, file.path
            )
        })?;
        let formatted = source_fmt::format_source_as_kind(kind, &file.body)
            .map_err(|err| format_parse_error(snippet, &err))?;
        if !formatted.contains(BEGIN_MARKER) && !formatted.contains(END_MARKER) {
            continue;
        }
        if extracted.is_some() {
            return Err(format!(
                "snippet at line {} is not formattable in place: formatted \
                 harness output retained duplicate snippet markers",
                snippet.open_line
            ));
        }
        let replacement = extract_marked_region(&formatted, snippet.open_line)?;
        let begin = single_marker_offset(&file.body, BEGIN_MARKER, snippet.open_line)?;
        let end = single_marker_offset(&file.body, END_MARKER, snippet.open_line)?;
        let (_, start) = line_bounds(&file.body, begin);
        let (end, _) = line_bounds(&file.body, end);
        let mut remapped = file.body;
        remapped.replace_range(start..end, &format!("{replacement}\n"));
        let remapped = source_fmt::format_source_as_kind(kind, &remapped);
        if !remapped.is_ok_and(|remapped| remapped == formatted) {
            return Err(format!(
                "snippet at line {} is not formattable in place: formatted \
                 snippet cannot be mapped back without changing its virtual file",
                snippet.open_line
            ));
        }
        extracted = Some(replacement);
    }

    extracted.ok_or_else(|| {
        format!(
            "snippet at line {} is not formattable in place: formatted \
             harness output did not retain the snippet markers",
            snippet.open_line
        )
    })
}

fn map_formatted_body(
    snippet: &Snippet,
    validation_body: &str,
    formatted: String,
) -> Result<Option<String>, String> {
    if snippet.placeholders.is_empty() {
        return Ok(Some(formatted));
    }
    if formatted == validation_body {
        return Ok(None);
    }
    if let Some(mapped) = map_deleted_line_placeholders(snippet, &formatted) {
        return Ok(Some(mapped));
    }
    Err(format!(
        "snippet at line {} uses `placeholder=...` and its formatted \
         validation source cannot be mapped back to the visible snippet body",
        snippet.open_line
    ))
}

fn map_deleted_line_placeholders(snippet: &Snippet, formatted: &str) -> Option<String> {
    if snippet
        .placeholders
        .iter()
        .any(|placeholder| !placeholder.to.is_empty())
    {
        return None;
    }

    let lines: Vec<&str> = snippet.body.split('\n').collect();
    let validation_lines: Vec<String> = lines
        .iter()
        .map(|line| validate::apply_placeholders(line, &snippet.placeholders))
        .collect();
    for (original, validation) in lines.iter().zip(&validation_lines) {
        if original != validation && !validation.trim().is_empty() {
            return None;
        }
    }
    let first = validation_lines
        .iter()
        .position(|line| !line.trim().is_empty())?;
    let last = validation_lines
        .iter()
        .rposition(|line| !line.trim().is_empty())?;
    if validation_lines[first..=last]
        .iter()
        .any(|line| line.trim().is_empty())
    {
        return None;
    }
    if lines[first..=last] != validation_lines[first..=last] {
        return None;
    }

    let mut out = String::new();
    if first > 0 {
        out.push_str(&lines[..first].join("\n"));
        if !formatted.is_empty() {
            out.push('\n');
        }
    }
    out.push_str(formatted.trim_end_matches('\n'));
    let suffix = &lines[last + 1..];
    if suffix.iter().any(|line| !line.trim().is_empty()) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&suffix.join("\n"));
    }
    Some(out)
}

fn same_snippet(a: &Snippet, b: &Snippet) -> bool {
    a.body_offset == b.body_offset && a.span == b.span && a.open_line == b.open_line
}

fn marked_body(body: &str) -> String {
    if body.is_empty() {
        format!("{BEGIN_COMMENT}\n{END_COMMENT}")
    } else if body.ends_with('\n') {
        format!("{BEGIN_COMMENT}\n{body}{END_COMMENT}")
    } else {
        format!("{BEGIN_COMMENT}\n{body}\n{END_COMMENT}")
    }
}

fn extract_marked_region(source: &str, open_line: usize) -> Result<String, String> {
    let begin = single_marker_offset(source, BEGIN_MARKER, open_line)?;
    let end = single_marker_offset(source, END_MARKER, open_line)?;
    if begin >= end {
        return Err(format!(
            "snippet at line {open_line} is not formattable in place: \
             formatted snippet markers are out of order"
        ));
    }

    let (begin_line_start, begin_line_end) = line_bounds(source, begin);
    let (end_line_start, _) = line_bounds(source, end);
    let indent = source[begin_line_start..begin]
        .strip_suffix("// ")
        .or_else(|| source[begin_line_start..begin].strip_suffix("//"))
        .unwrap_or(&source[begin_line_start..begin]);
    let content = &source[begin_line_end..end_line_start];
    let content = content.trim_end_matches('\n');
    deindent(content, indent, open_line)
}

fn single_marker_offset(source: &str, marker: &str, open_line: usize) -> Result<usize, String> {
    let mut hits = source.match_indices(marker);
    let Some((first, _)) = hits.next() else {
        return Err(format!(
            "snippet at line {open_line} is not formattable in place: \
             formatted harness output lost marker `{marker}`"
        ));
    };
    if hits.next().is_some() {
        return Err(format!(
            "snippet at line {open_line} is not formattable in place: \
             formatted harness output retained marker `{marker}` more than once"
        ));
    }
    Ok(first)
}

fn line_bounds(source: &str, offset: usize) -> (usize, usize) {
    let start = source[..offset].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    let end = source[offset..]
        .find('\n')
        .map(|idx| offset + idx + 1)
        .unwrap_or(source.len());
    (start, end)
}

fn deindent(content: &str, indent: &str, open_line: usize) -> Result<String, String> {
    if indent.is_empty() || content.is_empty() {
        return Ok(content.to_owned());
    }
    let mut out = String::with_capacity(content.len());
    for (idx, line) in content.split('\n').enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix(indent) {
            out.push_str(rest);
        } else if line.trim().is_empty() {
            out.push_str(line.trim_start());
        } else {
            return Err(format!(
                "snippet at line {open_line} is not formattable in place: \
                 formatted snippet indentation no longer matches its harness"
            ));
        }
    }
    Ok(out)
}

fn canonical_fence_body(mut formatted: String) -> String {
    if formatted.ends_with('\n') {
        formatted.pop();
    }
    formatted
}

fn assembled_file_kind(file: &AssembledFile) -> Option<KioFileKind> {
    let name = std::path::Path::new(&file.path).file_name()?.to_str()?;
    if crate::file_kind::is_package_file(name) {
        Some(KioFileKind::Package)
    } else if crate::file_kind::is_sig_file(name) {
        Some(KioFileKind::Signature)
    } else if crate::file_kind::is_dep_file(name) {
        Some(KioFileKind::Dependency)
    } else if crate::file_kind::is_lock_file(name) {
        Some(KioFileKind::Lock)
    } else if crate::file_kind::is_module_file(name) {
        Some(KioFileKind::Module)
    } else {
        None
    }
}

fn starts_with_module_header(source: &str) -> bool {
    super::validate::body_declares_module(source)
}

fn starts_with_package_section(source: &str) -> bool {
    let source = source.trim_start();
    source.starts_with("build ")
        || source.starts_with("build{")
        || source.starts_with("bridge ")
        || source.starts_with("bridge{")
}

fn format_parse_error(snippet: &Snippet, err: &Error) -> String {
    let (_span, message) = err.diag();
    format!(
        "snippet at line {} cannot be formatted: {}",
        snippet.open_line, message
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiodoc::document::Document;
    use crate::kiodoc::parse;
    use std::path::Path;

    fn format_md(source: &str) -> Result<String, Vec<DocError>> {
        let doc = Document::build(Path::new("input.md"), source, parse::scan(source))
            .map_err(|err| vec![err])?;
        let edits = format_document(source, &doc)?;
        Ok(apply_edits(source, &edits))
    }

    #[test]
    fn formats_harnessed_module_snippet() {
        let source = "```kio {harness=main placeholder=\"__INSERT__\"}\nbridge {\n  kiodoc;\n}\n\nmodule kiodoc;\n\nhost type S role(str);\nhost fn print(p0: S) -> .;\n\n__INSERT__\n```\n\n```kio {@main}\npub fn greet()->.{print(\"hi\\n\")}\n```\n";
        let out = format_md(source).expect("format document");
        assert!(out.contains("pub fn greet() -> . { print(\"hi\\n\") }"));
    }

    #[test]
    fn rejects_harnessed_import_loss() {
        let source = r#"<!--kio {harness=consumer file placeholder="__SNIPPET__"}
module consumer;

__SNIPPET__
-->

```kio {@consumer}
import names(Name);
import host(String);

fn show(value: Name) -> String { value }
```
"#;
        let errors = format_md(source).expect_err("reordered imports cross the snippet marker");
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("cannot be mapped back"))
        );
    }

    #[test]
    fn formats_canonical_import_boundaries() {
        let source = r#"<!--kio {harness=consumer file placeholder="__SNIPPET__"}
module consumer;

__SNIPPET__
-->

```kio {@consumer}
import host(String);
import names(Name);

fn show(value: Name)->String{value}
```
"#;
        let out = format_md(source).expect("format sorted imports and their body");
        assert!(out.contains("import host(String);\nimport names(Name);"));
        assert!(out.contains("fn show(value: Name) -> String { value }"));
        assert_eq!(format_md(&out).expect("format again"), out);
    }

    #[test]
    fn formats_accumulating_module_snippets() {
        let source = r#"<!--kio {harness=consumer placeholder="__SNIPPET__" accumulate}
module consumer;

__SNIPPET__
-->

```kio {@consumer}
import host(String);
import names(Name);
```

```kio {@consumer}
fn show(value: Name)->String{value}
```
"#;
        let out = format_md(source).expect("map each accumulating contribution");
        assert!(out.contains("import host(String);\nimport names(Name);\n```"));
        assert!(out.contains("fn show(value: Name) -> String { value }\n```"));
        assert_eq!(format_md(&out).expect("format again"), out);
    }

    #[test]
    fn formats_variant_package_snippet() {
        let source = "```kio {variant=package}\npackage pkg;\nbridge{pkg;}\n```\n";
        let out = format_md(source).expect("format document");
        assert!(out.contains("bridge {\n  pkg\n}"));
    }

    #[test]
    fn skips_ignored_snippet() {
        let source = "```kio {ignore}\nfn bad(}\n```\n";
        let out = format_md(source).expect("format document");
        assert_eq!(out, source);
    }

    #[test]
    fn deletion_placeholders_preserve_visible_prelude_without_trailing_blanks() {
        let source = "```kio {harness=main placeholder=\"__INSERT__\"}\nbridge {\n  kiodoc;\n}\n\nmodule kiodoc;\n\nhost type S role(str);\nhost fn print(p0: S) -> .;\n\n__INSERT__\n```\n\n```kio {@main placeholder={\"module kiodoc;\":\"\",\"import kiodoc(S, print);\":\"\"}}\nmodule kiodoc;\n\nimport kiodoc(S, print);\n\npub fn greet()->.{print(\"hi\\n\")}\n```\n";
        let out = format_md(source).expect("format document");
        let again = format_md(&out).expect("format document again");
        assert_eq!(again, out);
        assert!(
            out.contains(
                "import kiodoc(S, print);\n\npub fn greet() -> . { print(\"hi\\n\") }\n```"
            )
        );
    }
}
