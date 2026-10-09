//! A small GitHub-flavored-Markdown → HTML renderer for Kiodoc.
//!
//! Kiodoc's rendered HTML site (`kio doc build --html`) needs to turn
//! the markdown body of a `///` doc-comment or a `.md` tutorial page
//! into HTML. Rather than pull in a heavyweight markdown crate, this
//! module implements the GFM subset Kiodoc prose actually uses:
//!
//! - ATX headings (`#`..`######`)
//! - paragraphs
//! - fenced code blocks (``` ``` ``` with an optional info string)
//! - blockquotes (`> `)
//! - unordered (`-`, `*`, `+`) and ordered (`1.`) lists, one level
//! - thematic breaks (`---`)
//! - inline: `code`, **bold**, *italic*, `[text](url)` links
//!
//! It is deliberately line-based and forgiving — the input has
//! already been validated by `kio doc check`, so the renderer's job
//! is faithful rendering, not error detection.
//!
//! The Markdown-output emitter (`kio doc build --md`) does *not* go
//! through this module: a `.md` page renders to `.md` essentially
//! verbatim (only the Kiodoc directives and intra-doc links are
//! rewritten). HTML is the format that needs a real renderer.

/// Escape a string for safe inclusion in HTML text / attribute
/// context. Public so the page builder can escape signatures and
/// other interpolated text consistently.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Highlight Kio `src` into HTML for `kio doc`'s fenced `language-kio`
/// code blocks and `.sig` signature blocks: each classified token is
/// wrapped in `<span class="kio-…">` (the class is the token's
/// [`crate::tokens::TokenKind::css_class`]), and the byte-gaps the token
/// stream leaves between tokens — inter-token whitespace and newlines —
/// are emitted verbatim (HTML-escaped) so the reconstructed text is
/// byte-faithful to `src` and `<pre>` preserves the layout.
///
/// On a lex failure the whole snippet degrades to plain HTML-escaped
/// text (no spans), so a malformed fence never panics.
pub fn highlight_kio(src: &str) -> String {
    let Ok(tokens) = crate::tokens::dump(src) else {
        return escape_html(src);
    };
    highlight_classified(src, &tokens)
}

/// Highlight a pretty-printed declaration signature with parser-confirmed
/// structural context.
pub(super) fn highlight_kio_item_signature(signature: &str) -> String {
    let Ok(tokens) = crate::tokens::dump_item_signature(signature) else {
        return highlight_kio(signature);
    };
    highlight_classified(signature, &tokens)
}

fn highlight_classified(src: &str, tokens: &[crate::tokens::ClassifiedToken]) -> String {
    let mut out = String::with_capacity(src.len());
    let mut cursor = 0usize;
    for token in tokens {
        let start = token.span.start as usize;
        let end = token.span.end as usize;
        if start > cursor {
            out.push_str(&escape_html(&src[cursor..start]));
        }
        out.push_str("<span class=\"");
        out.push_str(token.kind.css_class());
        out.push_str("\">");
        out.push_str(&escape_html(&src[start..end]));
        out.push_str("</span>");
        cursor = end;
    }
    if cursor < src.len() {
        out.push_str(&escape_html(&src[cursor..]));
    }
    out
}

/// Render a GFM-subset markdown fragment to an HTML fragment (no
/// surrounding `<html>` / `<body>` — that is the page builder's
/// job). The input is assumed already validated.
pub fn render_html(markdown: &str) -> String {
    use crate::doc_markup::blocks::{self, Kind};
    let mut out = String::new();
    for block in blocks::parse(markdown) {
        match block.kind {
            Kind::Blank => {}
            Kind::Code { language, lines } => {
                let body = lines
                    .iter()
                    .map(|line| format!("{}\n", line.text))
                    .collect::<String>();
                let class = if language.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"language-{}\"", escape_html(language))
                };
                let rendered = if language == "kio" {
                    highlight_kio(&body)
                } else {
                    escape_html(&body)
                };
                out.push_str(&format!("<pre><code{}>{}</code></pre>\n", class, rendered));
            }
            Kind::Html => {
                for line in block.source.text.lines() {
                    out.push_str(line);
                    out.push('\n');
                }
            }
            Kind::Heading { level, text } => {
                out.push_str(&format!(
                    "<h{level}>{}</h{level}>\n",
                    render_inline(text.text)
                ));
            }
            Kind::Break => out.push_str("<hr />\n"),
            Kind::Quote(lines) => {
                let quoted = lines
                    .iter()
                    .map(|line| format!("{}\n", line.text))
                    .collect::<String>();
                out.push_str("<blockquote>\n");
                out.push_str(&render_html(&quoted));
                out.push_str("</blockquote>\n");
            }
            Kind::List { ordered, items } => {
                let tag = if ordered { "ol" } else { "ul" };
                out.push_str(&format!("<{tag}>\n"));
                for lines in items {
                    let item = lines
                        .iter()
                        .map(|line| line.text)
                        .collect::<Vec<_>>()
                        .join(" ");
                    out.push_str(&format!("<li>{}</li>\n", render_inline(&item)));
                }
                out.push_str(&format!("</{tag}>\n"));
            }
            Kind::Paragraph(lines) => {
                let paragraph = lines
                    .iter()
                    .map(|line| line.text)
                    .collect::<Vec<_>>()
                    .join(" ");
                if !paragraph.is_empty() {
                    out.push_str(&format!("<p>{}</p>\n", render_inline(&paragraph)));
                }
            }
        }
    }
    out
}

/// Render inline markdown (`code`, **bold**, *italic*, links) inside
/// already-block-split text. Emits HTML.
pub fn render_inline(text: &str) -> String {
    use crate::doc_markup::blocks::{InlineKind, inline_at};
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        if let Some(inline) = inline_at(text, i) {
            match inline.kind {
                InlineKind::Code(code) => {
                    out.push_str(&format!("<code>{}</code>", escape_html(code.text)))
                }
                InlineKind::Link { label, destination } => out.push_str(&format!(
                    "<a href=\"{}\">{}</a>",
                    escape_html(destination.text),
                    render_inline(label.text)
                )),
                InlineKind::Strong(inner) => {
                    out.push_str(&format!("<strong>{}</strong>", render_inline(inner.text)))
                }
                InlineKind::Emphasis(inner) => {
                    out.push_str(&format!("<em>{}</em>", render_inline(inner.text)))
                }
            }
            i = inline.source.span.end as usize;
        } else {
            let ch = text[i..].chars().next().unwrap();
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                _ => out.push(ch),
            }
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_headings_retain_html_rendering() {
        assert_eq!(
            render_html("λ\n#\n##  ##\n###"),
            "<p>λ</p>\n<h1></h1>\n<h2></h2>\n<h3></h3>\n"
        );
    }

    fn parse_link(s: &str) -> Option<(String, String, usize)> {
        let link = crate::doc_markup::lexical::inline_link_at(s, 0)?;
        Some((
            link.label.text.to_owned(),
            link.destination.text.to_owned(),
            link.source.text.len(),
        ))
    }

    #[test]
    fn compatibility_link_parser_keeps_original_unicode_and_destination() {
        let source = "[λ](docs/[`add`]) tail";
        assert_eq!(
            parse_link(source),
            Some((
                "λ".to_owned(),
                "docs/[`add`]".to_owned(),
                source.find(" tail").unwrap()
            ))
        );
        assert_eq!(
            render_html(&format!("é {source}")),
            "<p>é <a href=\"docs/[`add`]\">λ</a> tail</p>\n"
        );
        assert_eq!(
            parse_link("[x](a(b)c)"),
            Some(("x".to_owned(), "a(b".to_owned(), 8))
        );
    }

    #[test]
    fn heading_renders() {
        assert_eq!(render_html("# Title"), "<h1>Title</h1>\n");
        assert_eq!(render_html("### Sub"), "<h3>Sub</h3>\n");
    }

    #[test]
    fn paragraph_joins_lines() {
        assert_eq!(render_html("one\ntwo"), "<p>one two</p>\n");
    }

    #[test]
    fn fenced_code_block() {
        let source = "module kio_doc; fn value() { let x = 1; x }";
        crate::pass::parser::parse(source).expect("valid declaration and local binding");
        let md = format!("```kio\n{source}\n```");
        let html = render_html(&md);
        assert!(html.contains("<pre><code class=\"language-kio\">"));
        assert!(html.contains("<span class=\"kio-keyword-declaration\">let</span>"));
        assert!(html.contains("<span class=\"kio-identifier\">x</span>"));
        assert!(html.contains("<span class=\"kio-literal-number\">1</span>"));
        // Inter-token whitespace is emitted verbatim between the spans.
        assert!(html.contains("</span> <span"));
    }

    #[test]
    fn item_signature_uses_declaration_context_for_type_binder_brackets() {
        let signature = "pub fn identity[A](x: A) -> A";
        crate::pass::parser::parse(&format!("module kio_doc;\n{signature} {{ x }}"))
            .expect("valid complete declaration supplies the body-less signature");
        let html = highlight_kio_item_signature(signature);
        assert!(html.contains("<span class=\"kio-punctuation-bracket\">[</span>"));
        assert!(html.contains("<span class=\"kio-punctuation-bracket\">]</span>"));
        assert!(!html.contains("<span class=\"kio-operator-user\">[</span>"));
        assert_eq!(
            html.matches("<span class=\"kio-variable-parameter\">A</span>")
                .count(),
            1
        );
        assert!(html.contains("<span class=\"kio-variable-parameter\">x</span>"));
        assert_eq!(
            html.matches("<span class=\"kio-entity-name-type\">A</span>")
                .count(),
            2
        );
        assert!(!html.contains("{ x }"));
    }

    #[test]
    fn item_signature_keeps_square_bracket_operator_runs_ordinary() {
        let signature = "pub varop [* *] { foldl join empty; };";
        assert!(
            crate::pass::parser::parse(&format!("module kio_doc;\n{signature}")).is_ok(),
            "negative control must exercise the parser-backed path"
        );
        let html = highlight_kio_item_signature(signature);
        assert!(html.contains("<span class=\"kio-operator-user\">[*</span>"));
        assert!(html.contains("<span class=\"kio-operator-user\">*]</span>"));
        assert!(!html.contains("<span class=\"kio-punctuation-bracket\">[</span>"));
        assert!(!html.contains("<span class=\"kio-punctuation-bracket\">]</span>"));
    }

    #[test]
    fn non_kio_fence_is_not_highlighted() {
        // Only `kio` fences are tokenized; other languages keep the plain
        // escaped-blob path.
        let md = "```rust\nlet x = 1;\n```";
        let html = render_html(md);
        assert!(html.contains("<pre><code class=\"language-rust\">"));
        assert!(html.contains("let x = 1;"));
        assert!(!html.contains("kio-"));
    }

    #[test]
    fn html_comment_and_hidden_fence_stay_hidden() {
        let md = "<!-- note -->\n\n<!--kio {ignore}\n[`literal`]\n-->\n";
        let html = render_html(md);
        assert!(html.contains("<!-- note -->"));
        assert!(html.contains("<!--kio {ignore}\n[`literal`]\n-->"));
        assert!(!html.contains("&lt;!--"));
    }

    #[test]
    fn inline_code_span() {
        assert_eq!(
            render_html("use `print` here"),
            "<p>use <code>print</code> here</p>\n"
        );
    }

    #[test]
    fn bold_and_italic() {
        assert_eq!(
            render_inline("**bold** and *italic*"),
            "<strong>bold</strong> and <em>italic</em>"
        );
    }

    #[test]
    fn link_renders() {
        assert_eq!(
            render_inline("see [docs](http://x.com)"),
            "see <a href=\"http://x.com\">docs</a>"
        );
    }

    #[test]
    fn unordered_list() {
        let html = render_html("- one\n- two");
        assert_eq!(html, "<ul>\n<li>one</li>\n<li>two</li>\n</ul>\n");
    }

    #[test]
    fn ordered_list() {
        let html = render_html("1. one\n2. two");
        assert_eq!(html, "<ol>\n<li>one</li>\n<li>two</li>\n</ol>\n");
    }

    #[test]
    fn blockquote() {
        let html = render_html("> quoted");
        assert!(html.contains("<blockquote>"));
        assert!(html.contains("<p>quoted</p>"));
    }

    #[test]
    fn thematic_break() {
        assert_eq!(render_html("---"), "<hr />\n");
    }

    #[test]
    fn html_escaping() {
        assert_eq!(escape_html("a < b & c"), "a &lt; b &amp; c");
        assert_eq!(render_html("a < b"), "<p>a &lt; b</p>\n");
    }

    #[test]
    fn code_span_not_escaped_twice() {
        // The `<` inside a code span is escaped exactly once.
        assert_eq!(render_inline("`a < b`"), "<code>a &lt; b</code>");
    }
}
