//! Highlight displayed fences in the same assembled scope used by Kiodoc.

use std::collections::HashMap;
use std::path::Path;

use crate::kiodoc::document::{Document, Snippet};
use crate::tokens::{ClassifiedToken, TokenKind};

pub(super) fn fences(
    markdown: &str,
    module_source: Option<&str>,
) -> HashMap<u32, Vec<ClassifiedToken>> {
    let mut result = HashMap::new();
    let mut fences = crate::kiodoc::parse::scan(markdown);
    for fence in &mut fences {
        if fence.lang != "kio" {
            continue;
        }
        let self_harness = fence
            .attrs_text
            .as_deref()
            .and_then(|attrs| crate::kiodoc::attrs::parse(attrs).ok())
            .is_some_and(|attrs| attrs.self_ref);
        if self_harness {
            result.insert(
                fence.span.start,
                crate::tokens::dump_doc_code_with_module(&fence.body, true, module_source),
            );
            // Self harnesses are supplied by the enclosing module, not a named
            // harness in the Markdown document model.
            fence.attrs_text = Some("{ignore}".to_owned());
        } else if fence.attrs_text.is_none() {
            // Directive-generated source fences have no validation attributes.
            fence.attrs_text = Some("{ignore}".to_owned());
        }
    }
    let Ok(document) = Document::build(Path::new("highlight.md"), markdown, fences) else {
        return result;
    };
    for snippet in document.snippets().filter(|snippet| !snippet.ignored) {
        if let Some(tokens) = snippet_tokens(&document, snippet) {
            result.insert(snippet.span.start, tokens);
        }
    }
    result
}

fn snippet_tokens(document: &Document, snippet: &Snippet) -> Option<Vec<ClassifiedToken>> {
    let compiled = crate::kiodoc::validate::validation_body(snippet);
    // Markers preserve syntax and identify the exact inserted occurrence even
    // when two fences display identical source in different lexical scopes.
    let mut marker = "// __kiodoc_highlight_start__".to_owned();
    while compiled.contains(&marker)
        || document
            .files
            .values()
            .any(|file| file.body.contains(&marker))
        || document.snippets().any(|part| part.body.contains(&marker))
        || document
            .harnesses
            .values()
            .any(|harness| harness.body.contains(&marker))
    {
        marker.push('_');
    }
    let marked = format!("{marker}\n{compiled}\n");
    let accumulated = snippet
        .harness_ref
        .as_ref()
        .and_then(|name| document.harnesses.get(name))
        .is_some_and(|harness| harness.accumulate);
    let body = if accumulated {
        document
            .snippets()
            .filter(|part| part.harness_ref == snippet.harness_ref && !part.ignored)
            .map(|part| {
                if part.span == snippet.span {
                    marked.clone()
                } else {
                    crate::kiodoc::validate::validation_body(part)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        marked
    };
    let assembled =
        crate::kiodoc::validate::assemble_with_snippet_body(document, snippet, &body).ok()?;
    let file = assembled
        .files
        .iter()
        .find(|file| crate::file_kind::is_module_file(&file.path) && file.body.contains(&marker))?;
    let start = file.body.find(&marker)? + marker.len() + 1;
    let contextual = crate::tokens::dump_range(&file.body, start, compiled.len()).ok()?;
    let mut displayed = crate::tokens::dump(&snippet.body).ok()?;
    let mapping = unchanged_ranges(snippet);
    for token in contextual.iter().filter(|token| {
        matches!(
            token.kind,
            TokenKind::EntityNameMacro | TokenKind::EntityNameMacroReference
        )
    }) {
        let Some((compiled_start, display_start, _)) =
            mapping.iter().find(|(compiled_start, _, len)| {
                token.span.start as usize >= *compiled_start
                    && token.span.end as usize <= compiled_start + len
            })
        else {
            continue;
        };
        let shift = *display_start as i64 - *compiled_start as i64;
        let span = crate::span::Span::new(
            (token.span.start as i64 + shift) as u32,
            (token.span.end as i64 + shift) as u32,
        );
        if let Some(displayed) = displayed
            .iter_mut()
            .find(|displayed| displayed.span == span)
        {
            displayed.kind = token.kind;
        }
    }
    Some(displayed)
}

/// Exact unchanged segments through the validator's simultaneous placeholder
/// replacement. Replacement text never lends a semantic role to display text.
fn unchanged_ranges(snippet: &Snippet) -> Vec<(usize, usize, usize)> {
    let mut ranges = Vec::new();
    let (mut display, mut compiled) = (0, 0);
    let (mut run_display, mut run_compiled) = (0, 0);
    while display < snippet.body.len() {
        if let Some(placeholder) = snippet
            .placeholders
            .iter()
            .find(|placeholder| snippet.body[display..].starts_with(&placeholder.from))
        {
            ranges.push((run_compiled, run_display, display - run_display));
            display += placeholder.from.len();
            compiled += placeholder.to.len();
            run_display = display;
            run_compiled = compiled;
        } else {
            let len = snippet.body[display..].chars().next().unwrap().len_utf8();
            display += len;
            compiled += len;
        }
    }
    ranges.push((run_compiled, run_display, display - run_display));
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn macro_names(source: &str, tokens: &[ClassifiedToken]) -> Vec<String> {
        tokens
            .iter()
            .filter(|token| {
                matches!(
                    token.kind,
                    TokenKind::EntityNameMacro | TokenKind::EntityNameMacroReference
                )
            })
            .map(|token| source[token.span.start as usize..token.span.end as usize].to_owned())
            .collect()
    }

    #[test]
    fn accumulated_harness_classifies_exact_display_ranges_and_retracts_edits() {
        let markdown = "```kio {harness=steps placeholder=\"__INSERT_CODE_HERE__\" accumulate}\nmodule app;\n__INSERT_CODE_HERE__\n```\n\n```kio {@steps}\nimport provider(macro make as selected);\n```\n\n```kio {@steps}\nfn run() { selected(()) }\n```\n";
        let tokens = fences(markdown, None);
        let parsed = crate::kiodoc::parse::scan(markdown);
        assert_eq!(
            macro_names(&parsed[2].body, &tokens[&parsed[2].span.start]),
            ["selected"]
        );
        let edited = markdown.replace("macro make", "make");
        let tokens = fences(&edited, None);
        let parsed = crate::kiodoc::parse::scan(&edited);
        assert!(macro_names(&parsed[2].body, &tokens[&parsed[2].span.start]).is_empty());
    }

    #[test]
    fn self_harness_uses_end_of_module_scope_with_local_shadowing() {
        let module = "module app; fn early() { () } macro later : . -> . { impl implementation; };";
        let markdown =
            "```kio {@}\nlater(()); (.(later: . -> .) { later(()) })(.(x: .) { x })\n```";
        let tokens = fences(markdown, Some(module));
        let parsed = crate::kiodoc::parse::scan(markdown);
        assert_eq!(
            macro_names(&parsed[0].body, &tokens[&parsed[0].span.start]),
            ["later"]
        );
    }

    #[test]
    fn rendered_harness_preserves_importless_macro_refinement() {
        let markdown = "```kio {harness=body placeholder=\"__INSERT_CODE_HERE__\"}\nmodule app; import provider(macro make); fn run() { __INSERT_CODE_HERE__ }\n```\n\n```kio {@body}\nmake(())\n```\n";
        let html = super::super::markdown::render_html(markdown);
        assert!(
            html.contains("<span class=\"kio-entity-name-macro\">make</span>"),
            "{html}"
        );
    }
}
