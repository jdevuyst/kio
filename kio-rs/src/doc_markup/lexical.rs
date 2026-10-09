//! Source-neutral Kiodoc lexical forms and rendered-block boundaries.

use crate::span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSlice<'a> {
    pub text: &'a str,
    pub span: Span,
}

impl<'a> SourceSlice<'a> {
    pub(crate) fn new(source: &'a str, start: usize, end: usize) -> Self {
        Self {
            text: &source[start..end],
            span: Span::new(start as u32, end as u32),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Bracket<'a> {
    pub source: SourceSlice<'a>,
    pub payload: SourceSlice<'a>,
}

/// Recognize only the complete delimiters; resolution and directive policy are separate.
pub fn bracket_at(source: &str, start: usize) -> Option<Bracket<'_>> {
    let (payload, end, complete) = super::generated::bracket_bounds(source, start)?;
    if !complete {
        return None;
    }
    Some(Bracket {
        source: SourceSlice::new(source, start, end + 2),
        payload: SourceSlice::new(source, payload, end),
    })
}

pub(crate) struct InlineLink<'a> {
    pub source: SourceSlice<'a>,
    pub label: SourceSlice<'a>,
    pub destination: SourceSlice<'a>,
}

pub(crate) fn inline_link_at(source: &str, start: usize) -> Option<InlineLink<'_>> {
    let (close_label, close_url) = super::generated::link_bounds(source, start)?;
    Some(InlineLink {
        source: SourceSlice::new(source, start, close_url + 1),
        label: SourceSlice::new(source, start + 1, close_label),
        destination: SourceSlice::new(source, close_label + 2, close_url),
    })
}

pub(crate) fn reference_definition_key(line: &str) -> Option<SourceSlice<'_>> {
    let (start, end) = super::generated::definition_bounds(line)?;
    Some(SourceSlice::new(line, start, end))
}

/// End of an inline code span, unmatched backtick run, or Markdown escape.
pub(crate) fn inline_literal_end(text: &str, start: usize) -> Option<usize> {
    use super::generated::{INLINE_CODE, INLINE_ESCAPE};
    let bytes = text.as_bytes();
    if bytes[start] == INLINE_ESCAPE && bytes.get(start + 1).is_some_and(u8::is_ascii_punctuation) {
        return Some(start + 2);
    }
    if bytes[start] != INLINE_CODE {
        return None;
    }
    let run_end = |from: usize| {
        from + bytes[from..]
            .iter()
            .take_while(|&&byte| byte == INLINE_CODE)
            .count()
    };
    let open_end = run_end(start);
    let width = open_end - start;
    let line_start = text[..start].rfind('\n').map_or(0, |offset| offset + 1);
    let opening_line = text[line_start..].split('\n').next().unwrap();
    let in_heading = super::generated::inline_heading(opening_line.trim_start());
    let mut cursor = open_end;
    while cursor < bytes.len() {
        if bytes[cursor] == INLINE_CODE {
            let end = run_end(cursor);
            if end - cursor == width {
                return Some(end);
            }
            cursor = end;
        } else if bytes[cursor] == b'\n'
            && (in_heading
                || text[cursor + 1..]
                    .split('\n')
                    .next()
                    .is_some_and(ends_paragraph))
        {
            // A matching delimiter must belong to the same inline block.
            break;
        } else {
            cursor += 1;
        }
    }
    // An unmatched run is literal; its suffix is not a shorter opener.
    Some(open_end)
}

pub(crate) fn ends_paragraph(line: &str) -> bool {
    super::generated::inline_break(line)
}

/// True iff `line` begins a raw HTML block — a `<` followed by an
/// ASCII letter (open tag) or `/` (close tag). The block passes
/// through to the output verbatim.
pub(crate) fn is_html_block_start(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.first() != Some(&b'<') || bytes.len() < 2 {
        return false;
    }
    bytes[1].is_ascii_alphabetic() || bytes[1] == b'/'
}

/// If `line` opens a fenced code block, return the fence marker
/// (a run of 3+ backticks or tildes).
pub(crate) fn code_fence_marker(line: &str) -> Option<String> {
    for marker in ['`', '~'] {
        let run: String = line.chars().take_while(|&c| c == marker).collect();
        if run.len() >= 3 {
            return Some(run);
        }
    }
    None
}

/// Parse an ATX heading line, returning `(level, text)`.
pub(crate) fn atx_heading(line: &str) -> Option<(usize, &str)> {
    if !super::generated::inline_heading(line) {
        return None;
    }
    let hashes: usize = line.chars().take_while(|&c| c == '#').count();
    let rest = &line[hashes..];
    if rest.is_empty() {
        return Some((hashes, rest));
    }
    Some((hashes, rest.trim().trim_end_matches('#').trim_end()))
}

/// True for a thematic break (`---`, `***`, `___`, 3+ of one char).
pub(crate) fn is_thematic_break(line: &str) -> bool {
    super::generated::thematic_break(line)
}

/// If `line` is a list item, return the byte index where its content
/// begins (after the marker and following whitespace).
pub(crate) fn list_item_marker(line: &str) -> Option<usize> {
    super::generated::list_marker(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotone_link_search_preserves_first_delimiter_grammar() {
        let alphabet = ['[', ']', '(', ')', 'é'];
        for width in 0..=6 {
            for mut encoded in 0..5_usize.pow(width) {
                let mut source = String::new();
                for _ in 0..width {
                    source.push(alphabet[encoded % 5]);
                    encoded /= 5;
                }
                let mut cursor = super::super::generated::LinkCursor::new(&source);
                for (start, _) in source.char_indices() {
                    let expected = (|| {
                        let suffix = source.get(start..)?;
                        if !suffix.starts_with('[') {
                            return None;
                        }
                        let label = start + suffix.find(']')?;
                        let after = &source[label + 1..];
                        if !after.starts_with('(') {
                            return None;
                        }
                        Some((label, label + 1 + after.find(')')?))
                    })();
                    assert_eq!(cursor.bounds(start), expected, "{source:?} at {start}");
                }
            }
        }
    }

    #[test]
    fn generated_inline_boundaries_match_existing_block_recognizers() {
        for prefix in ["", " ", "\t", "\u{2003}"] {
            for body in [
                "",
                "prose",
                "#",
                "# title",
                "##x",
                "####### title",
                "~~~",
                "```",
                "``",
                "<tag",
                "</",
                "<1",
                "> q",
                "- x",
                "-x",
                "123) item",
                "4. ",
                "- - -",
                "___",
                "___x",
            ] {
                let line = format!("{prefix}{body}");
                let trimmed = line.trim_start();
                let expected = trimmed.is_empty()
                    || code_fence_marker(trimmed).is_some()
                    || atx_heading(trimmed).is_some()
                    || is_thematic_break(trimmed)
                    || is_html_block_start(trimmed)
                    || trimmed.starts_with('>')
                    || list_item_marker(trimmed).is_some();
                assert_eq!(ends_paragraph(&line), expected, "{line:?}");
            }
        }
        for value in 0..=0x10ffff {
            if let Some(c) = char::from_u32(value) {
                assert_eq!(
                    super::super::generated::whitespace(c),
                    c.is_whitespace(),
                    "U+{value:04X}"
                );
            }
        }
    }

    #[test]
    fn bracket_ranges_are_original_utf8_slices() {
        let source = "é [`@signature α`] end";
        let start = source.find('[').unwrap();
        let bracket = bracket_at(source, start).unwrap();
        assert_eq!(bracket.source.text, "[`@signature α`]");
        assert_eq!(bracket.payload.text, "@signature α");
        assert_eq!(bracket.payload.span.start as usize, start + 2);
        assert_eq!(
            &source[bracket.payload.span.start as usize..bracket.payload.span.end as usize],
            bracket.payload.text
        );
        assert!(bracket_at(source, 1).is_none());
        assert!(bracket_at(source, source.len() + 1).is_none());
        for incomplete in ["[", "[`", "[`name", "[`name`", "[`na]me`]", "[`na\nme`]"] {
            assert!(bracket_at(incomplete, 0).is_none(), "{incomplete}");
        }
        assert_eq!(bracket_at("[``]", 0).unwrap().payload.text, "");
    }
}
