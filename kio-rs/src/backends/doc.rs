//! Presentation-only rendering of retained boundary documentation.
//!
//! Kiodoc references remain Kio spellings, never host identities. Directive
//! operands are shown as code without resolution or source expansion. Native
//! framing is separate from this shared, source-preserving prose policy.

use crate::ast::DocComment;
use crate::doc_markup::blocks::{self, InlineKind, Kind};
use crate::doc_markup::rewrite::{is_indented_code, rewrite_inert};

#[derive(Clone, Copy)]
pub(crate) enum Dialect {
    Markdown,
    PlainText,
    Godoc,
    JavadocHtml,
    Haddock,
    Rustdoc,
    SwiftDoc,
}

/// Render a doc body without comment delimiters or host-language string quoting.
pub(crate) fn render(doc: &DocComment, dialect: Dialect) -> String {
    let prose = rewrite_inert(&normalize_newlines(&doc.lines.join("\n")));
    // Framing must not create trailing whitespace, even from authored blank lines.
    let body = render_body(&prose, dialect, 0);
    let body = match dialect {
        Dialect::SwiftDoc => body.replace('\0', "\\0"),
        Dialect::Godoc => body.replace('\0', "\\0").replace('\u{feff}', "\\u{feff}"),
        _ => body,
    };
    body.split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_body(prose: &str, dialect: Dialect, depth: usize) -> String {
    if depth >= crate::doc_markup::MAX_RENDER_DEPTH {
        if matches!(dialect, Dialect::Rustdoc | Dialect::SwiftDoc) {
            return literal_fence(prose, "text", line_ending(prose));
        }
        return escape_text(prose, dialect);
    }
    let mut out = String::new();
    for block in blocks::parse(prose) {
        if matches!(dialect, Dialect::Markdown) {
            out.push_str(block.source.text);
            continue;
        }
        // Native Markdown also recognizes indentation as code. This leaf rule
        // does not add an indented-code production to Kiodoc's shared grammar.
        if matches!(dialect, Dialect::Rustdoc | Dialect::SwiftDoc)
            && is_indented_code(block.source.text)
        {
            let body = block
                .source
                .text
                .split_inclusive('\n')
                .map(|line| {
                    line.strip_prefix("    ")
                        .or_else(|| line.strip_prefix('\t'))
                        .unwrap_or(line)
                })
                .collect::<String>();
            out.push_str(&literal_fence(
                &body,
                "text",
                line_ending(block.source.text),
            ));
            continue;
        }
        match block.kind {
            Kind::Blank => out.push_str(block.source.text),
            Kind::Heading { level, text } => {
                if matches!(dialect, Dialect::Rustdoc | Dialect::SwiftDoc) {
                    out.push_str(&render_inline(block.source.text, dialect));
                    continue;
                }
                let heading = render_inline(text.text, dialect);
                match dialect {
                    Dialect::Godoc => out.push_str(&format!("# {heading}")),
                    Dialect::JavadocHtml => out.push_str(&format!(
                        "<p><strong>{} {heading}</strong></p>",
                        "#".repeat(level)
                    )),
                    Dialect::Haddock => out.push_str(&format!("{} {heading}", "=".repeat(level))),
                    _ => out.push_str(&heading),
                }
                out.push_str(line_ending(block.source.text));
            }
            Kind::Code { language, lines } => {
                if matches!(dialect, Dialect::Rustdoc | Dialect::SwiftDoc) {
                    let mut body = joined(&lines);
                    if lines
                        .last()
                        .is_some_and(|line| prose[line.span.end as usize..].starts_with('\n'))
                    {
                        body.push('\n');
                    }
                    let language = if matches!(dialect, Dialect::Rustdoc) || language.contains('`')
                    {
                        "text"
                    } else {
                        language
                    };
                    out.push_str(&literal_fence(
                        &body,
                        language,
                        line_ending(block.source.text),
                    ));
                    continue;
                }
                if matches!(dialect, Dialect::JavadocHtml) {
                    out.push_str(&format!(
                        "<pre><code class=\"language-{}\">",
                        escape_text(language, dialect)
                    ));
                }
                for line in lines {
                    out.push_str(match dialect {
                        Dialect::Godoc => "\t",
                        Dialect::Haddock => "> ",
                        Dialect::JavadocHtml => "",
                        _ => "    ",
                    });
                    if matches!(dialect, Dialect::JavadocHtml) {
                        out.push_str(&escape_text(line.text, dialect));
                    } else {
                        out.push_str(line.text);
                    }
                    if prose[line.span.end as usize..].starts_with('\n') {
                        out.push('\n');
                    }
                }
                if matches!(dialect, Dialect::JavadocHtml) {
                    out.push_str("</code></pre>\n");
                }
            }
            Kind::Html => out.push_str(&escape_text(block.source.text, dialect)),
            Kind::Break => {
                if matches!(dialect, Dialect::JavadocHtml) {
                    out.push_str("<hr>\n");
                } else {
                    out.push_str(&escape_text(block.source.text, dialect));
                }
            }
            Kind::Quote(lines) => {
                let quoted = joined(&lines);
                let rendered = render_body(&quoted, dialect, depth + 1);
                if matches!(dialect, Dialect::JavadocHtml) {
                    out.push_str("<blockquote>\n");
                    out.push_str(&rendered);
                    out.push_str("</blockquote>\n");
                } else {
                    for line in rendered.split_inclusive('\n') {
                        out.push_str(if matches!(dialect, Dialect::Haddock) {
                            "&#62; "
                        } else {
                            "> "
                        });
                        out.push_str(line);
                    }
                    if rendered.is_empty() {
                        out.push_str(&escape_text(">", dialect));
                    }
                    if !rendered.ends_with('\n') {
                        out.push_str(line_ending(block.source.text));
                    }
                }
            }
            Kind::List { ordered, items } => {
                let tag = if ordered { "ol" } else { "ul" };
                if matches!(dialect, Dialect::JavadocHtml) {
                    out.push_str(&format!("<{tag}>\n"));
                }
                for (index, lines) in items.iter().enumerate() {
                    let body = if matches!(dialect, Dialect::Rustdoc | Dialect::SwiftDoc) {
                        joined(&blocks::native_list_payloads(prose, lines))
                    } else {
                        joined(lines)
                    };
                    let rendered = render_body(&body, dialect, depth + 1);
                    if matches!(dialect, Dialect::JavadocHtml) {
                        out.push_str("<li>");
                        out.push_str(&rendered);
                        out.push_str("</li>\n");
                    } else {
                        let marker = if ordered {
                            format!("{}. ", index + 1)
                        } else {
                            "- ".to_owned()
                        };
                        if rendered.is_empty() {
                            out.push_str(marker.trim_end());
                            out.push('\n');
                        }
                        for (line_index, line) in rendered.split_terminator('\n').enumerate() {
                            if line_index == 0 {
                                out.push_str(&marker);
                            } else {
                                out.push_str(&" ".repeat(marker.len()));
                            }
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                }
                if matches!(dialect, Dialect::JavadocHtml) {
                    out.push_str(&format!("</{tag}>\n"));
                } else if !block.source.text.ends_with('\n') {
                    out.pop();
                }
            }
            Kind::Paragraph(lines) => {
                let paragraph = if matches!(dialect, Dialect::JavadocHtml) {
                    joined(&lines)
                } else {
                    block.source.text.to_owned()
                };
                if matches!(dialect, Dialect::JavadocHtml) {
                    out.push_str("<p>");
                }
                out.push_str(&render_inline(&paragraph, dialect));
                if matches!(dialect, Dialect::JavadocHtml) {
                    out.push_str("</p>");
                }
                if matches!(dialect, Dialect::JavadocHtml) {
                    out.push_str(line_ending(block.source.text));
                }
            }
        }
    }
    out
}

fn joined(lines: &[crate::doc_markup::lexical::SourceSlice<'_>]) -> String {
    lines
        .iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_ending(line: &str) -> &'static str {
    if line.ends_with('\n') { "\n" } else { "" }
}

/// A longer delimiter keeps authored fence text inside the literal block. Rust
/// uses `text` so neither language tags nor doctest flags create host tests.
fn literal_fence(body: &str, language: &str, ending: &str) -> String {
    let width = body.split(|ch| ch != '`').map(str::len).max().unwrap_or(0) + 1;
    let fence = "`".repeat(width.max(3));
    format!(
        "{fence}{language}\n{body}{}{fence}{ending}",
        if body.is_empty() || body.ends_with('\n') {
            ""
        } else {
            "\n"
        }
    )
}

fn render_inline(text: &str, dialect: Dialect) -> String {
    if matches!(dialect, Dialect::Rustdoc | Dialect::SwiftDoc) {
        return native_markdown_inline(text, dialect);
    }
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        if let Some(inline) = blocks::inline_at(text, i) {
            match inline.kind {
                InlineKind::Code(inner) => {
                    if matches!(dialect, Dialect::JavadocHtml) {
                        out.push_str(&format!(
                            "<code>{}</code>",
                            escape_text(inner.text, dialect)
                        ));
                    } else {
                        out.push_str(&escape_text(inline.source.text, dialect));
                    }
                }
                InlineKind::Strong(inner) | InlineKind::Emphasis(inner) => {
                    let tag = if matches!(inline.kind, InlineKind::Strong(_)) {
                        "strong"
                    } else {
                        "em"
                    };
                    if matches!(dialect, Dialect::JavadocHtml) {
                        out.push_str(&format!("<{tag}>"));
                    }
                    out.push_str(&render_inline(inner.text, dialect));
                    if matches!(dialect, Dialect::JavadocHtml) {
                        out.push_str(&format!("</{tag}>"));
                    }
                }
                InlineKind::Link { label, destination } => {
                    if matches!(dialect, Dialect::JavadocHtml) {
                        out.push_str(&format!(
                            "<a href=\"{}\">{}</a>",
                            escape_text(destination.text, dialect),
                            render_inline(label.text, dialect)
                        ));
                    } else {
                        out.push_str(&render_inline(label.text, dialect));
                        out.push_str(" (");
                        out.push_str(&escape_text(destination.text, dialect));
                        out.push(')');
                    }
                }
            }
            i = inline.source.span.end as usize;
        } else {
            let ch = text[i..].chars().next().unwrap();
            out.push_str(&escape_text(&text[i..i + ch.len_utf8()], dialect));
            i += ch.len_utf8();
        }
    }
    out
}

fn native_markdown_inline(text: &str, dialect: Dialect) -> String {
    let mut out = String::new();
    let mut at = 0;
    while at < text.len() {
        if text.as_bytes()[at] == b'`' {
            let width = text[at..].bytes().take_while(|&byte| byte == b'`').count();
            let end = crate::doc_markup::lexical::inline_literal_end(text, at).unwrap();
            if end > at + width {
                let body = &text[at + width..end - width];
                if width == 2 && matches!(dialect, Dialect::SwiftDoc) {
                    // DocC reserves double backticks for symbol lookup. Choose
                    // an ordinary delimiter absent from the literal payload.
                    let width = body.split(|ch| ch != '`').map(str::len).max().unwrap_or(0) + 1;
                    let fence = "`".repeat(if width == 2 { 3 } else { width });
                    let pad = body.starts_with('`') || body.ends_with('`');
                    out.push_str(&fence);
                    if pad {
                        out.push(' ');
                    }
                    out.push_str(body);
                    if pad {
                        out.push(' ');
                    }
                    out.push_str(&fence);
                } else {
                    out.push_str(&text[at..end]);
                }
            } else if matches!(dialect, Dialect::SwiftDoc) {
                out.push_str(&"&#96;".repeat(width));
            } else {
                out.push_str(&text[at..end]);
            }
            at = end;
        } else if matches!(dialect, Dialect::Rustdoc) && text.as_bytes()[at] == b'[' {
            if let Some(link) = crate::doc_markup::lexical::inline_link_at(text, at) {
                // Even an explicit Markdown destination can trigger rustdoc's
                // symbol lookup. An HTML anchor preserves the exact URL.
                out.push_str(&format!(
                    "<a href=\"{}\">{}</a>",
                    escape_text(link.destination.text, Dialect::JavadocHtml),
                    render_inline(link.label.text, Dialect::JavadocHtml)
                ));
                at = link.source.span.end as usize;
            } else {
                // Bare brackets become rustdoc symbol queries. Explicit authored
                // links and literal code retain their existing meaning.
                out.push_str("&#91;");
                at += 1;
            }
        } else {
            let ch = text[at..].chars().next().unwrap();
            match ch {
                '@' if matches!(dialect, Dialect::SwiftDoc) => out.push_str("&#64;"),
                '\\' if matches!(dialect, Dialect::SwiftDoc) => out.push_str("&#92;"),
                _ => out.push(ch),
            }
            at += ch.len_utf8();
        }
    }
    out
}

fn escape_text(text: &str, dialect: Dialect) -> String {
    match dialect {
        Dialect::Markdown | Dialect::PlainText => text.to_owned(),
        Dialect::Rustdoc | Dialect::SwiftDoc => native_markdown_inline(text, dialect),
        Dialect::Godoc => text.replace(']', "\\]"),
        Dialect::Haddock => {
            let mut out = String::new();
            for ch in text.chars() {
                if matches!(
                    ch,
                    '&' | '@'
                        | '/'
                        | '\''
                        | '"'
                        | '_'
                        | '<'
                        | '>'
                        | '#'
                        | '['
                        | ']'
                        | '\\'
                        | '='
                        | '*'
                        | '-'
                ) {
                    out.push_str(&format!("&#{};", ch as u32));
                } else {
                    out.push(ch);
                }
            }
            out
        }
        Dialect::JavadocHtml => {
            let mut out = String::new();
            for ch in text.chars() {
                out.push_str(match ch {
                    '&' => "&amp;",
                    '<' => "&lt;",
                    '>' => "&gt;",
                    '"' => "&quot;",
                    '\'' => "&#39;",
                    _ => {
                        out.push(ch);
                        continue;
                    }
                });
            }
            out
        }
    }
}

/// Frame inert prose as JSDoc. Escaping every at-sign also covers inline native
/// tags; source prose must not become a host type, visibility, or linking directive.
pub(crate) fn jsdoc(body: &str) -> String {
    if body.trim().is_empty() {
        return String::new();
    }
    let body = normalize_newlines(body)
        .replace("*/", "*\\/")
        .replace('@', "\\@");
    block_comment(&body)
}

/// Javadoc must remain inert before Java's Unicode-escape preprocessing as well
/// as during the documentation tool's tag parsing.
pub(crate) fn javadoc(body: &str) -> String {
    let body = normalize_newlines(body)
        .replace('\\', "&#92;")
        .replace("*/", "*&#47;")
        .replace('@', "&#64;");
    block_comment(&body)
}

fn block_comment(body: &str) -> String {
    if body.trim().is_empty() {
        return String::new();
    }
    let mut out = String::from("/**\n");
    for line in body.lines() {
        let line = line.trim_end();
        out.push_str(" *");
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
        out.push('\n');
    }
    out.push_str(" */\n");
    out
}

/// Haddock attaches the first `-- |` line to the following declaration.
pub(crate) fn haddock(out: &mut String, body: &str, indent: &str) {
    if body.trim().is_empty() {
        return;
    }
    for (index, line) in normalize_newlines(body).lines().enumerate() {
        out.push_str(indent);
        out.push_str(if index == 0 { "-- |" } else { "--" });
        let line = line.trim_end();
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
        out.push('\n');
    }
}

/// A quoted Python string expression, suitable as the first statement of a
/// documented declaration. String serialization escapes physical newlines,
/// quotes and control characters without interpreting authored backslashes.
pub(crate) fn python_docstring(body: &str) -> String {
    serde_json::to_string(body).expect("serializing a documentation string is infallible")
}

/// Attach a rendered body with a native line-comment prefix at the declaration's
/// indentation. Dialect-specific interpretation remains separate from framing.
pub(crate) fn push_line_comments(out: &mut String, body: &str, indent: &str, prefix: &str) {
    if body.trim().is_empty() {
        return;
    }
    for line in normalize_newlines(body).lines() {
        out.push_str(indent);
        out.push_str(prefix);
        let line = line.trim_end();
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
        out.push('\n');
    }
}

fn normalize_newlines(body: &str) -> String {
    body.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;

    fn doc(text: &str) -> DocComment {
        DocComment {
            lines: text.split('\n').map(str::to_owned).collect(),
            span: Span::new(0, 0),
        }
    }

    #[test]
    fn markdown_retains_lines_and_degrades_only_live_kiodoc_forms() {
        let source = "# Title\nfirst [`name`];\nsecond [`@source helper`].\n\n- **bold**\n  continued\n> quote\n```kio\n[`literal`]\n```\n[label](docs/[`path`])\n[override]: docs/[`path`]";
        assert_eq!(
            render(&doc(source), Dialect::Markdown),
            source
                .replace("[`name`]", "`name`")
                .replace("[`@source helper`]", "`helper`")
        );
    }

    #[test]
    fn plaintext_uses_shared_forms_without_rewrapping() {
        let source = "# **Title**\nfirst **bold**\nsecond *italic* and [label](url)\n\n- one\n  continued\n- two\n\n> quoted **text**\n> next\n\n```text\nλ <x>\n```\n\n<div>raw *text*</div>\n\n`code` and [`name`]";
        let expected = "Title\nfirst bold\nsecond italic and label (url)\n\n- one\n  continued\n- two\n\n> quoted text\n> next\n\n    λ <x>\n\n<div>raw *text*</div>\n\n`code` and `name`";
        assert_eq!(render(&doc(source), Dialect::PlainText), expected);
    }

    #[test]
    fn malformed_and_unknown_markup_is_total_and_opaque() {
        let source = "[`@unknown name`]; [`@source`]; [`unfinished\n```text\nunterminated";
        assert_eq!(render(&doc(source), Dialect::Markdown), source);
        assert_eq!(
            render(&doc(source), Dialect::PlainText),
            "[`@unknown name`]; [`@source`]; [`unfinished\n    unterminated"
        );
        assert_eq!(render(&doc(" \n\t"), Dialect::Markdown), "\n");
    }

    #[test]
    fn jsdoc_neutralizes_terminators_and_native_tags_without_trailing_space() {
        assert_eq!(
            jsdoc("@type {never}\n{@link private}\n*/\n \nλ  "),
            "/**\n * \\@type {never}\n * {\\@link private}\n * *\\/\n *\n * λ\n */\n"
        );
        assert_eq!(jsdoc(" \n\t"), "");
    }

    #[test]
    fn plaintext_quoted_code_remains_literal() {
        let source = "> ~~~text\n> **literal** [`name`]\n> ~~~\n> **prose** [`name`];";
        assert_eq!(
            render(&doc(source), Dialect::PlainText),
            ">     **literal** [`name`]\n> prose `name`;"
        );
    }

    #[test]
    fn python_literal_preserves_quotes_backslashes_and_controls() {
        let body = "λ \"\"\" \\n\n\0";
        let literal = python_docstring(body);
        assert_eq!(serde_json::from_str::<String>(&literal).unwrap(), body);
        assert!(!literal.contains('\n'));
        assert!(!literal.contains('\0'));
    }

    #[test]
    fn line_comments_keep_blank_lines_without_trailing_whitespace() {
        let mut out = String::new();
        push_line_comments(&mut out, "first\n \nlast \n", "  ", "///");
        assert_eq!(out, "  /// first\n  ///\n  /// last\n");
        push_line_comments(&mut out, " \n\t", "", "///");
        assert_eq!(out, "  /// first\n  ///\n  /// last\n");
    }

    #[test]
    fn every_native_line_is_framed_after_cr_normalization() {
        let mut out = String::new();
        push_line_comments(&mut out, "first\rsecond\r\nthird\n", "", "///");
        assert_eq!(out, "/// first\n/// second\n/// third\n");
        assert_eq!(
            render(&doc("first\rsecond\r\nthird"), Dialect::Markdown),
            "first\nsecond\nthird"
        );
    }

    #[test]
    fn godoc_links_do_not_share_ambiguous_labels_or_infer_native_names() {
        let source = "## Heading\n[same](https://one.example) [same](https://two.example); [fmt].\n\n```go\n[fmt]\n```\n\n- item\n  continued";
        assert_eq!(
            render(&doc(source), Dialect::Godoc),
            "# Heading\nsame (https://one.example) same (https://two.example); [fmt\\].\n\n\t[fmt]\n\n- item\n  continued"
        );
    }

    #[test]
    fn godoc_source_controls_are_visible_in_prose_and_code() {
        assert_eq!(
            render(
                &doc("NUL\0BOM\u{feff}\n```go\nNUL\0BOM\u{feff}\n```"),
                Dialect::Godoc
            ),
            "NUL\\0BOM\\u{feff}\n\tNUL\\0BOM\\u{feff}\n"
        );
    }

    #[test]
    fn javadoc_html_escapes_raw_text_and_prelexical_host_syntax() {
        let source = "## Heading\n<div>raw</div>\n\n`a<b` and [link](https://example.com?a&b)\n```kio\nλ <x>\n```";
        assert_eq!(
            render(&doc(source), Dialect::JavadocHtml),
            "<p><strong>## Heading</strong></p>\n&lt;div&gt;raw&lt;/div&gt;\n\n<p><code>a&lt;b</code> and <a href=\"https://example.com?a&amp;b\">link</a></p>\n<pre><code class=\"language-kio\">λ &lt;x&gt;\n</code></pre>\n"
        );
        assert_eq!(
            javadoc("\\u002a\\u002f @hidden */"),
            "/**\n * &#92;u002a&#92;u002f &#64;hidden *&#47;\n */\n"
        );
    }

    #[test]
    fn haddock_source_markup_is_literal_outside_bird_track_code() {
        let source = "# Heading\n[fmt] @tag 'Bool' __bold__\n\n```hs\n@raw 'Bool' [fmt]\n```";
        assert_eq!(
            render(&doc(source), Dialect::Haddock),
            "= Heading\n&#91;fmt&#93; &#64;tag &#39;Bool&#39; bold\n\n> @raw 'Bool' [fmt]\n"
        );
        let mut out = String::new();
        haddock(&mut out, "first\rsecond\r\n\nlast", "  ");
        assert_eq!(out, "  -- | first\n  -- second\n  --\n  -- last\n");
    }

    #[test]
    fn deeply_nested_native_docs_still_pass_through_safe_framing() {
        let source = format!("{}@private */", "> ".repeat(256));
        assert!(jsdoc(&render(&doc(&source), Dialect::Markdown)).contains("\\@private *\\/"));
        assert!(
            javadoc(&render(&doc(&source), Dialect::JavadocHtml)).contains("&#64;private *&#47;")
        );
        assert!(render(&doc(&source), Dialect::Haddock).contains("&#64;private"));
    }

    #[test]
    fn rustdoc_examples_are_inert_in_every_native_code_context() {
        for tag in ["", "rust", "compile_fail", "rust,no_run"] {
            let source = format!("```{tag}\nnot Rust [`literal`];\n```\n");
            assert_eq!(
                render(&doc(&source), Dialect::Rustdoc),
                "```text\nnot Rust [`literal`];\n```\n"
            );
        }
        assert_eq!(
            render(
                &doc("    [`literal`];\n\n>     [`quoted`];\n\n-     [`nested`];"),
                Dialect::Rustdoc
            ),
            "```text\n[`literal`];\n```\n\n> ```text\n> [`quoted`];\n> ```\n\n- ```text\n  [`nested`];\n  ```"
        );
        assert_eq!(
            render(&doc("\tnot Rust"), Dialect::Rustdoc),
            "```text\nnot Rust\n```"
        );
    }

    #[test]
    fn swift_doc_directives_and_symbol_links_stay_literal() {
        let source = "@Metadata {\n@Snippet(path: \"missing\")\n@param x\n\\param x\n``missingSymbol`` and `@literal`\n\n```swift\n@literal \\literal\n```\nNUL\0DOC";
        assert_eq!(
            render(&doc(source), Dialect::SwiftDoc),
            "&#64;Metadata {\n&#64;Snippet(path: \"missing\")\n&#64;param x\n&#92;param x\n`missingSymbol` and `@literal`\n\n```swift\n@literal \\literal\n```\nNUL\\0DOC"
        );
        assert_eq!(
            render(
                &doc("Ordinary `inline` and **prose**.\n```swift\nlet x = 1\n```"),
                Dialect::SwiftDoc
            ),
            "Ordinary `inline` and **prose**.\n```swift\nlet x = 1\n```"
        );
        assert_eq!(
            native_markdown_inline("``a```b``", Dialect::SwiftDoc),
            "````a```b````"
        );
        assert_eq!(
            render(&doc("~~~a`b\n@Metadata {\n~~~"), Dialect::SwiftDoc),
            "```text\n@Metadata {\n```"
        );
    }

    #[test]
    fn rustdoc_bare_names_do_not_become_host_symbol_queries() {
        assert_eq!(
            render(
                &doc(
                    "[UnknownKioType] and [external](https://example.com).\n`[literal]` and [`kio`];\n```rust\n[code]\n```"
                ),
                Dialect::Rustdoc
            ),
            "&#91;UnknownKioType] and <a href=\"https://example.com\">external</a>.\n`[literal]` and `kio`;\n```text\n[code]\n```"
        );
        assert_eq!(
            render(
                &doc(
                    "[**relative** `resource`](UnknownKioType) [query](?q=1&x=2) [fragment](#part)"
                ),
                Dialect::Rustdoc
            ),
            "<a href=\"UnknownKioType\"><strong>relative</strong> <code>resource</code></a> <a href=\"?q=1&amp;x=2\">query</a> <a href=\"#part\">fragment</a>"
        );
    }

    #[test]
    fn native_literal_fallback_cannot_be_closed_by_authored_fences() {
        let source = format!(
            "{}@Metadata\n{} ```` ```",
            "> ".repeat(256),
            "> ".repeat(256)
        );
        for dialect in [Dialect::Rustdoc, Dialect::SwiftDoc] {
            let rendered = render(&doc(&source), dialect);
            assert!(rendered.contains("`````text\n"));
            assert!(rendered.contains("@Metadata"));
            assert!(rendered.contains(" ```` ```"));
        }
        assert_eq!(
            render(&doc("```rust\nbody\n````\n@Metadata"), Dialect::Rustdoc),
            "`````text\nbody\n````\n@Metadata\n`````"
        );
    }
}
