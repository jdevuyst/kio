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

use crate::kiodoc::parse::{
    atx_heading, code_fence_marker, ends_paragraph, is_html_block_start, is_thematic_break,
    list_item_marker,
};

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
    let mut out = String::new();
    let lines: Vec<&str> = markdown.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // Blank line — paragraph separator, no output.
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // Fenced code block.
        if let Some(fence) = code_fence_marker(trimmed) {
            let info = trimmed[fence.len()..].trim();
            let lang = info.split_whitespace().next().unwrap_or("");
            let mut body = String::new();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                if l.trim_start().starts_with(&fence)
                    && l.trim_start()[fence.len()..].trim().is_empty()
                {
                    i += 1;
                    break;
                }
                body.push_str(l);
                body.push('\n');
                i += 1;
            }
            let class = if lang.is_empty() {
                String::new()
            } else {
                format!(" class=\"language-{}\"", escape_html(lang))
            };
            // A `kio` fence is highlighted into per-token spans; every
            // other language (and the no-language case) stays a single
            // escaped blob.
            let rendered = if lang == "kio" {
                highlight_kio(&body)
            } else {
                escape_html(&body)
            };
            out.push_str(&format!("<pre><code{}>{}</code></pre>\n", class, rendered));
            continue;
        }

        // HTML comments pass through verbatim, including hidden Kiodoc
        // fences, so the browser keeps them hidden.
        if trimmed.starts_with("<!--") {
            while i < lines.len() {
                out.push_str(lines[i]);
                out.push('\n');
                let closed = lines[i].contains("-->");
                i += 1;
                if closed {
                    break;
                }
            }
            continue;
        }

        // Raw HTML block — a line that begins with an HTML tag passes
        // through verbatim (GFM HTML-block behavior). The block runs
        // to the next blank line. The page builder relies on this to
        // emit per-item `<h3 id=…>` / `<pre class="sig">` chrome
        // through the same Markdown pipeline as prose.
        if is_html_block_start(trimmed) {
            while i < lines.len() && !lines[i].trim().is_empty() {
                out.push_str(lines[i]);
                out.push('\n');
                i += 1;
            }
            continue;
        }

        // ATX heading.
        if let Some((level, text)) = atx_heading(trimmed) {
            out.push_str(&format!("<h{level}>{}</h{level}>\n", render_inline(text)));
            i += 1;
            continue;
        }

        // Thematic break.
        if is_thematic_break(trimmed) {
            out.push_str("<hr />\n");
            i += 1;
            continue;
        }

        // Blockquote — a run of `>`-prefixed lines.
        if trimmed.starts_with('>') {
            let mut quoted = String::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                let l = lines[i].trim_start();
                let rest = l.strip_prefix('>').unwrap_or(l);
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                quoted.push_str(rest);
                quoted.push('\n');
                i += 1;
            }
            out.push_str("<blockquote>\n");
            out.push_str(&render_html(&quoted));
            out.push_str("</blockquote>\n");
            continue;
        }

        // List — a run of list-item lines (all same ordered/unordered
        // kind). One level only; nested lists render flat.
        if list_item_marker(trimmed).is_some() {
            let ordered = is_ordered_marker(trimmed);
            let tag = if ordered { "ol" } else { "ul" };
            out.push_str(&format!("<{tag}>\n"));
            while i < lines.len() {
                let l = lines[i].trim_start();
                let Some(content_start) = list_item_marker(l) else {
                    break;
                };
                if is_ordered_marker(l) != ordered {
                    break;
                }
                // Collect the item's text plus any continuation lines
                // (indented, non-blank, not a new item).
                let mut item_text = l[content_start..].to_string();
                i += 1;
                while i < lines.len() {
                    let cont = lines[i];
                    let ct = cont.trim_start();
                    if ct.is_empty() || list_item_marker(ct).is_some() {
                        break;
                    }
                    item_text.push(' ');
                    item_text.push_str(ct);
                    i += 1;
                }
                out.push_str(&format!("<li>{}</li>\n", render_inline(&item_text)));
            }
            out.push_str(&format!("</{tag}>\n"));
            continue;
        }

        // Paragraph — collect consecutive non-blank, non-block lines.
        let mut para = String::new();
        while i < lines.len() {
            let l = lines[i];
            let lt = l.trim_start();
            if ends_paragraph(lt) {
                break;
            }
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(lt);
            i += 1;
        }
        if !para.is_empty() {
            out.push_str(&format!("<p>{}</p>\n", render_inline(&para)));
        }
    }
    out
}

/// True iff `line`'s list marker is the ordered (`1.`) kind.
fn is_ordered_marker(line: &str) -> bool {
    let digits: usize = line.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0
}

/// Render inline markdown (`code`, **bold**, *italic*, links) inside
/// already-block-split text. Emits HTML.
pub fn render_inline(text: &str) -> String {
    let mut out = String::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        // Inline code span — `code`. Backticks bind tightest, so this
        // is checked before any emphasis / link handling.
        if c == b'`' {
            // Find the closing backtick.
            if let Some(close) = text[i + 1..].find('`') {
                let code = &text[i + 1..i + 1 + close];
                out.push_str(&format!("<code>{}</code>", escape_html(code)));
                i += 1 + close + 1;
                continue;
            }
        }
        // Link — [text](url). The text part may contain inline code.
        if c == b'['
            && let Some((label, url, consumed)) = parse_link(&text[i..])
        {
            out.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                escape_html(&url),
                render_inline(&label)
            ));
            i += consumed;
            continue;
        }
        // Bold — **text** or __text__.
        if (c == b'*' || c == b'_') && i + 1 < bytes.len() && bytes[i + 1] == c {
            let marker = &text[i..i + 2];
            if let Some(end) = text[i + 2..].find(marker) {
                let inner = &text[i + 2..i + 2 + end];
                out.push_str(&format!("<strong>{}</strong>", render_inline(inner)));
                i += 2 + end + 2;
                continue;
            }
        }
        // Italic — *text* or _text_.
        if c == b'*' || c == b'_' {
            let marker = c as char;
            if let Some(end) = text[i + 1..].find(marker) {
                let inner = &text[i + 1..i + 1 + end];
                if !inner.is_empty() && !inner.starts_with(' ') {
                    out.push_str(&format!("<em>{}</em>", render_inline(inner)));
                    i += 1 + end + 1;
                    continue;
                }
            }
        }
        // Default — escape the single character.
        match c {
            b'&' => out.push_str("&amp;"),
            b'<' => out.push_str("&lt;"),
            b'>' => out.push_str("&gt;"),
            b'"' => out.push_str("&quot;"),
            _ => {
                // Copy the full UTF-8 char.
                let ch_len = utf8_char_len(c);
                out.push_str(&text[i..i + ch_len]);
                i += ch_len;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Parse a `[label](url)` link at the start of `s`. Returns
/// `(label, url, bytes_consumed)`.
fn parse_link(s: &str) -> Option<(String, String, usize)> {
    if !s.starts_with('[') {
        return None;
    }
    let close_label = s.find(']')?;
    let after = &s[close_label + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let close_url = after.find(')')?;
    let label = s[1..close_label].to_string();
    let url = after[1..close_url].to_string();
    let consumed = close_label + 1 + close_url + 1;
    Some((label, url, consumed))
}

/// Byte length of a UTF-8 character given its leading byte.
fn utf8_char_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead < 0xE0 {
        2
    } else if lead < 0xF0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
