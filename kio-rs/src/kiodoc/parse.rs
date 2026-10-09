//! Markdown lexical boundaries and fence scanning over a `.md` file's source.
//!
//! Two fence shapes are recognized:
//!
//! - **Visible fenced code block.** Opening with three or more
//!   backticks at the start of a line (optionally indented up to
//!   three spaces — GitHub-flavored Markdown's rule), the info
//!   string (language tag + optional `{attrs}`) on the same line,
//!   the body on the lines that follow, and a closing fence
//!   matching the opener.
//! - **Hidden fence (HTML comment).** A line `<!--LANG {attrs}` at
//!   the start of a line, the body on the lines that follow, and
//!   a line `-->` (possibly trailing whitespace) terminating it.
//!   The `LANG {attrs}` part is parsed identically to a visible
//!   fence's info string.
//!
//! Other Markdown stays verbatim in [`Block::Prose`]. [`scan`] projects
//! only fences into normalized snippet records; [`scan_blocks`] retains
//! original slices and line endings.
//!
//! The scanner does **not** parse attribute lists or the document
//! structure (harness/snippet/output pairings); those live in
//! [`super::attrs`] and [`super::document`]. Inline lexical helpers keep
//! source ranges separate from reference resolution and rendering policy.

use crate::doc_markup::fences::{Block, scan_blocks};
use crate::span::Span;

/// One scanned fence: visible or hidden form, agnostic of meaning.
#[derive(Debug, Clone)]
pub struct Fence {
    /// Language tag at the front of the info string (`kio`, `text`,
    /// etc.). Empty when the info string had no leading
    /// non-whitespace word (rare, but legal in CommonMark — produces
    /// a fence with no language).
    pub lang: String,
    /// The text *after* the language tag on the opening line —
    /// typically a `{...}` attribute list. Leading whitespace is
    /// stripped. `None` if the info string was just the language
    /// tag with no trailing content.
    pub attrs_text: Option<String>,
    /// The fence body text, joined with `\n`. Trailing newlines on
    /// the final body line are stripped; an empty body is represented
    /// as an empty string.
    pub body: String,
    /// Source byte range covering the *whole* fence (open marker
    /// through close marker, inclusive).
    pub span: Span,
    /// Whether the fence used the HTML-comment hidden form. Visible
    /// fences set this to `false`; hidden fences to `true`.
    pub hidden: bool,
    /// Byte offset of the first body line — used by diagnostics that
    /// need to map a snippet line back to the markdown source.
    pub body_offset: u32,
    /// 1-based line number of the opening marker in the markdown
    /// source — convenient for diagnostics.
    pub open_line: usize,
}

/// Scan `source` and return every fence (visible + hidden) in
/// source order.
///
/// The scanner is byte-oriented but UTF-8-clean: every position it
/// emits is a UTF-8 character boundary because the marker bytes it
/// looks for (backtick, ASCII `<>-{!` etc.) are all 1-byte ASCII
/// and the scanner only advances along line boundaries from there.
pub fn scan(source: &str) -> Vec<Fence> {
    scan_blocks(source)
        .into_iter()
        .filter_map(|block| {
            let Block::Fence(fence) = block else {
                return None;
            };
            let (lang, attrs_text) = split_info(fence.info.text);
            Some(Fence {
                lang,
                attrs_text,
                body: fence
                    .body
                    .text
                    .strip_suffix('\n')
                    .unwrap_or(fence.body.text)
                    .to_owned(),
                span: fence.source.span,
                hidden: fence.hidden,
                body_offset: fence.body.span.start,
                open_line: fence.open_line,
            })
        })
        .collect()
}

/// Return `source` with every visible or hidden fence replaced by
/// whitespace while preserving newlines and byte offsets.
///
/// Reference and directive processing operates on Kiodoc prose, not on
/// fenced examples. Blanking the whole fence also keeps reference-link
/// definitions in examples from overriding prose references.
pub fn blank_fences(source: &str) -> String {
    let fences = scan(source);
    if fences.is_empty() {
        return source.to_owned();
    }

    let ranges: Vec<(usize, usize)> = fences
        .iter()
        .map(|fence| (fence.span.start as usize, fence.span.end as usize))
        .collect();
    let mut out = String::with_capacity(source.len());
    for (offset, ch) in source.char_indices() {
        if ch != '\n'
            && ranges
                .iter()
                .any(|(start, end)| offset >= *start && offset < *end)
        {
            for _ in 0..ch.len_utf8() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Split an info string `<lang> [<attrs>]` into its language tag
/// and the trailing attribute text. Leading whitespace on the
/// attrs portion is stripped. An info string with no leading
/// non-whitespace word produces an empty `lang`.
fn split_info(info: &str) -> (String, Option<String>) {
    let (lang, attrs) = crate::doc_markup::generated::fence_info_parts(info);
    let attrs_text = if attrs.is_empty() {
        None
    } else {
        Some(attrs.to_owned())
    };
    (lang.to_owned(), attrs_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_blocks_partition_original_bytes() {
        let source = "# Héading\r\nfirst\r\nsecond\r\n\r\n- list\r\n  continued\r\n```kio {@}\r\nα\r\n\r\n```\r\n> quote\n<!--text\nβ\n-->\ntail";
        let blocks = scan_blocks(source);
        assert_eq!(blocks.len(), 5);
        let mut end = 0;
        for block in &blocks {
            let slice = block.source();
            assert_eq!(slice.span.start, end);
            assert_eq!(
                slice.text,
                &source[slice.span.start as usize..slice.span.end as usize]
            );
            assert_eq!(
                slice.text.as_ptr(),
                source[slice.span.start as usize..].as_ptr()
            );
            end = slice.span.end;
        }
        assert_eq!(end as usize, source.len());
        assert_eq!(
            blocks
                .iter()
                .map(|block| block.source().text)
                .collect::<String>(),
            source
        );
        let Block::Fence(fence) = blocks[1] else {
            panic!("visible fence")
        };
        assert_eq!(fence.info.text, "kio {@}\r");
        assert_eq!(fence.body.text, "α\r\n\r\n");
        assert_eq!(fence.close.unwrap().text, "```\r\n");
        assert!(!fence.hidden);
        for slice in [fence.info, fence.body, fence.close.unwrap()] {
            assert_eq!(
                slice.text,
                &source[slice.span.start as usize..slice.span.end as usize]
            );
        }
    }

    #[test]
    fn incomplete_fences_retain_original_suffix() {
        assert!(scan_blocks("").is_empty());
        let source = "before\n```kio\r\nα\r\n";
        let blocks = scan_blocks(source);
        let Block::Fence(fence) = blocks[1] else {
            panic!("visible fence")
        };
        assert_eq!(fence.body.text, "α\r\n");
        assert!(fence.close.is_none());
        let source = "before\n<!--kio\n```text\nbody\n```\n";
        let blocks = scan_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], Block::Prose(_)));
        assert_eq!(blocks[0].source().text, source);
    }

    #[test]
    fn compatibility_fence_body_offsets_and_line_endings() {
        let source = "é prose\r\n```kio {@}\r\nα\r\n\r\n```\r\n<!--text\nβ\n-->\n";
        let fences = scan(source);
        assert_eq!(fences.len(), 2);
        assert_eq!(fences[0].lang, "kio");
        assert_eq!(fences[0].attrs_text.as_deref(), Some("{@}"));
        assert_eq!(fences[0].body, "α\r\n\r");
        assert_eq!(fences[0].open_line, 2);
        assert_eq!(fences[0].body_offset as usize, source.find('α').unwrap());
        assert_eq!(
            fences[0].span.start as usize,
            source.find("```kio").unwrap()
        );
        assert_eq!(fences[0].span.end as usize, source.find("<!--").unwrap());
        assert_eq!(fences[1].body, "β");
        assert!(fences[1].hidden);
        assert_eq!(fences[1].span.end as usize, source.len());
        assert_eq!(scan("```kio\nα\n\n")[0].body, "α\n");
        assert!(scan("<!--kio\n```text\nbody\n```\n").is_empty());
    }

    #[test]
    fn no_fences_in_plain_prose() {
        assert!(scan("just some text\nover two lines\n").is_empty());
    }

    #[test]
    fn one_visible_kio_fence() {
        let src = "```kio {ignore}\nlet x = 1\n```\n";
        let fences = scan(src);
        assert_eq!(fences.len(), 1);
        let f = &fences[0];
        assert_eq!(f.lang, "kio");
        assert_eq!(f.attrs_text.as_deref(), Some("{ignore}"));
        assert_eq!(f.body, "let x = 1");
        assert!(!f.hidden);
    }

    #[test]
    fn visible_fence_without_attrs() {
        let src = "```kio\nlet x = 1\n```\n";
        let fences = scan(src);
        assert_eq!(fences.len(), 1);
        assert_eq!(fences[0].lang, "kio");
        assert!(fences[0].attrs_text.is_none());
    }

    #[test]
    fn visible_fence_no_lang() {
        let src = "```\nplain text\n```\n";
        let fences = scan(src);
        assert_eq!(fences.len(), 1);
        assert!(fences[0].lang.is_empty());
    }

    #[test]
    fn two_fences_in_sequence() {
        let src = "```kio {@main}\nlet x = 1\n```\n\nprose\n\n```text {stdout}\n2\n```\n";
        let fences = scan(src);
        assert_eq!(fences.len(), 2);
        assert_eq!(fences[0].lang, "kio");
        assert_eq!(fences[1].lang, "text");
    }

    #[test]
    fn hidden_kio_fence() {
        let src = "<!--kio {harness=main}\npackage tutorial;\n__INSERT_CODE_HERE__\n-->\n";
        let fences = scan(src);
        assert_eq!(fences.len(), 1);
        let f = &fences[0];
        assert!(f.hidden);
        assert_eq!(f.lang, "kio");
        assert_eq!(f.attrs_text.as_deref(), Some("{harness=main}"));
        assert!(f.body.contains("__INSERT_CODE_HERE__"));
    }

    #[test]
    fn near_miss_html_comment_is_opaque_prose() {
        // Has a space after `<!--`, so it doesn't qualify as a
        // hidden fence — the contract treats it as ordinary prose.
        let src = "<!-- TODO: rewrite this paragraph -->\n";
        assert!(scan(src).is_empty());
    }

    #[test]
    fn html_comment_terminates_inline_is_opaque() {
        let src = "<!--kio {x} -->\n";
        assert!(scan(src).is_empty());
    }

    #[test]
    fn nested_fence_with_more_backticks() {
        // Four-backtick opener allows three-backtick markers inside
        // the body (CommonMark fence rule).
        let src = "````markdown\n```kio {ignore}\nlet x = 1\n```\n````\n";
        let fences = scan(src);
        assert_eq!(fences.len(), 1);
        assert_eq!(fences[0].lang, "markdown");
        assert!(fences[0].body.contains("```kio"));
    }

    #[test]
    fn fence_indented_two_spaces_is_recognized() {
        let src = "  ```kio {ignore}\n  let x = 1\n  ```\n";
        let fences = scan(src);
        assert_eq!(fences.len(), 1);
    }

    #[test]
    fn fence_indented_four_spaces_is_code_block_not_fence() {
        let src = "    ```kio {ignore}\n    let x = 1\n    ```\n";
        assert!(scan(src).is_empty());
    }

    #[test]
    fn unterminated_hidden_fence_is_skipped() {
        let src = "<!--kio {harness=x}\npackage tutorial;\nnever closes\n";
        assert!(scan(src).is_empty());
    }

    #[test]
    fn blank_fences_covers_every_language_and_hidden_form() {
        let src = "before [`kept`]\n```text\n[`visible`]\n```\n<!--kio {ignore}\n[`hidden`]\n-->\nafter [`also_kept`]\n";
        let blanked = blank_fences(src);
        assert_eq!(blanked.len(), src.len());
        assert!(blanked.contains("before [`kept`]"));
        assert!(blanked.contains("after [`also_kept`]"));
        assert!(!blanked.contains("visible"));
        assert!(!blanked.contains("hidden"));
        assert_eq!(blanked.matches('\n').count(), src.matches('\n').count());
    }

    #[test]
    fn blank_fences_preserves_utf8_byte_offsets() {
        let src = "```text\nλ [`inside`]\n```\n[`outside`]\n";
        let blanked = blank_fences(src);
        assert_eq!(blanked.len(), src.len());
        assert_eq!(blanked.find("[`outside`]"), src.find("[`outside`]"));
    }
}
