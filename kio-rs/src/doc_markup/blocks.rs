//! Borrowed block and inline forms recognized by Kiodoc's HTML renderer.
//!
//! Source ranges retain authored whitespace. Joining paragraph or list lines is
//! a rendering policy, not part of recognition. Fence validation and snippet
//! extraction continue to use the separate lexical rules in `kiodoc::parse`.

use super::lexical::{
    SourceSlice, atx_heading, code_fence_marker, ends_paragraph, inline_link_at,
    is_html_block_start, is_thematic_break, list_item_marker,
};

#[derive(Debug)]
pub(crate) struct Block<'a> {
    pub source: SourceSlice<'a>,
    pub kind: Kind<'a>,
}

#[derive(Debug)]
pub(crate) enum Kind<'a> {
    Blank,
    Code {
        language: &'a str,
        lines: Vec<SourceSlice<'a>>,
    },
    Html,
    Heading {
        level: usize,
        text: SourceSlice<'a>,
    },
    Break,
    Quote(Vec<SourceSlice<'a>>),
    List {
        ordered: bool,
        items: Vec<Vec<SourceSlice<'a>>>,
    },
    Paragraph(Vec<SourceSlice<'a>>),
}

/// The subset's block policy, with unmodified original slices for every block.
pub(crate) fn parse(source: &str) -> Vec<Block<'_>> {
    let lines: Vec<&str> = source.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let start = offset(source, lines[i]);
        let trimmed = lines[i].trim_start();
        let kind = if trimmed.is_empty() {
            i += 1;
            Kind::Blank
        } else if let Some(fence) = code_fence_marker(trimmed) {
            let language = trimmed[fence.len()..]
                .split_whitespace()
                .next()
                .unwrap_or("");
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() {
                let line = lines[i];
                i += 1;
                if line.trim_start().starts_with(&fence)
                    && line.trim_start()[fence.len()..].trim().is_empty()
                {
                    break;
                }
                body.push(slice(source, line));
            }
            Kind::Code {
                language,
                lines: body,
            }
        } else if trimmed.starts_with("<!--") {
            while i < lines.len() {
                let closed = lines[i].contains("-->");
                i += 1;
                if closed {
                    break;
                }
            }
            Kind::Html
        } else if is_html_block_start(trimmed) {
            while i < lines.len() && !lines[i].trim().is_empty() {
                i += 1;
            }
            Kind::Html
        } else if let Some((level, text)) = atx_heading(trimmed) {
            i += 1;
            Kind::Heading {
                level,
                text: slice(source, text),
            }
        } else if is_thematic_break(trimmed) {
            i += 1;
            Kind::Break
        } else if trimmed.starts_with('>') {
            let mut quoted = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                let line = lines[i].trim_start();
                let marker = super::generated::quote_marker(line).unwrap();
                quoted.push(slice(source, &line[marker..]));
                i += 1;
            }
            Kind::Quote(quoted)
        } else if list_item_marker(trimmed).is_some() {
            let ordered = is_ordered_marker(trimmed);
            let mut items = Vec::new();
            while i < lines.len() {
                let line = lines[i].trim_start();
                let Some(content_start) = list_item_marker(line) else {
                    break;
                };
                if is_ordered_marker(line) != ordered {
                    break;
                }
                let mut item = vec![slice(source, &line[content_start..])];
                i += 1;
                while i < lines.len() {
                    let continuation = lines[i].trim_start();
                    if continuation.is_empty() || list_item_marker(continuation).is_some() {
                        break;
                    }
                    item.push(slice(source, continuation));
                    i += 1;
                }
                items.push(item);
            }
            Kind::List { ordered, items }
        } else {
            let mut paragraph = Vec::new();
            while i < lines.len() && !ends_paragraph(lines[i].trim_start()) {
                paragraph.push(slice(source, lines[i].trim_start()));
                i += 1;
            }
            Kind::Paragraph(paragraph)
        };
        let end = lines
            .get(i)
            .map_or(source.len(), |line| offset(source, line));
        blocks.push(Block {
            source: SourceSlice::new(source, start, end),
            kind,
        });
    }
    blocks
}

fn offset(source: &str, part: &str) -> usize {
    part.as_ptr() as usize - source.as_ptr() as usize
}

fn slice<'a>(source: &'a str, part: &'a str) -> SourceSlice<'a> {
    let start = offset(source, part);
    SourceSlice::new(source, start, start + part.len())
}

fn is_ordered_marker(line: &str) -> bool {
    line.starts_with(|ch: char| ch.is_ascii_digit())
}

/// Native Markdown needs the extra indentation that the HTML renderer joins
/// away. Return source-owned payloads after only the list's marker margin.
pub(crate) fn native_list_payloads<'a>(
    source: &'a str,
    lines: &[SourceSlice<'a>],
) -> Vec<SourceSlice<'a>> {
    let Some(first) = lines.first() else {
        return Vec::new();
    };
    let start = first.span.start as usize;
    let margin = start - source[..start].rfind('\n').map_or(0, |at| at + 1);
    let mut payloads = vec![*first];
    for line in &lines[1..] {
        let start = line.span.start as usize;
        let physical = source[..start].rfind('\n').map_or(0, |at| at + 1);
        let raw = &source[physical..line.span.end as usize];
        let remove = raw
            .bytes()
            .take(margin)
            .take_while(|&byte| byte == b' ')
            .count();
        payloads.push(SourceSlice::new(
            source,
            physical + remove,
            line.span.end as usize,
        ));
    }
    payloads
}

pub(crate) struct Inline<'a> {
    pub source: SourceSlice<'a>,
    pub kind: InlineKind<'a>,
}

pub(crate) enum InlineKind<'a> {
    Code(SourceSlice<'a>),
    Link {
        label: SourceSlice<'a>,
        destination: SourceSlice<'a>,
    },
    Strong(SourceSlice<'a>),
    Emphasis(SourceSlice<'a>),
}

/// Recognize a rendered inline form at a character boundary. Unrecognized text
/// stays literal; this deliberately preserves the renderer's permissive grammar.
pub(crate) fn inline_at(text: &str, start: usize) -> Option<Inline<'_>> {
    let rest = text.get(start..)?;
    let first = *rest.as_bytes().first()?;
    let (end, kind) = if first == b'`'
        && let Some(close) = rest[1..].find('`')
    {
        let end = start + 1 + close;
        (
            end + 1,
            InlineKind::Code(SourceSlice::new(text, start + 1, end)),
        )
    } else if first == b'['
        && let Some(link) = inline_link_at(text, start)
    {
        (
            link.source.span.end as usize,
            InlineKind::Link {
                label: link.label,
                destination: link.destination,
            },
        )
    } else if let Some(bounds) = super::generated::EmphasisCursor::new(text).bounds(start) {
        let inner = SourceSlice::new(text, start + bounds.width, bounds.end - bounds.width);
        (
            bounds.end,
            if bounds.width == 2 {
                InlineKind::Strong(inner)
            } else {
                InlineKind::Emphasis(inner)
            },
        )
    } else {
        return None;
    };
    Some(Inline {
        source: SourceSlice::new(text, start, end),
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_emphasis_preserves_first_closer_grammar_and_four_form_depth() {
        use crate::doc_markup::generated::{EmphasisBounds, EmphasisCursor};
        fn original(source: &str, start: usize) -> Option<EmphasisBounds> {
            let rest = &source[start..];
            for delimiter in ["**", "__"] {
                if let Some(body) = rest.strip_prefix(delimiter)
                    && let Some(close) = body.find(delimiter)
                {
                    return Some(EmphasisBounds {
                        width: 2,
                        end: start + 4 + close,
                    });
                }
            }
            for delimiter in ['*', '_'] {
                if let Some(body) = rest.strip_prefix(delimiter)
                    && let Some(close) = body.find(delimiter)
                    && close > 0
                    && !body.starts_with(' ')
                {
                    return Some(EmphasisBounds {
                        width: 1,
                        end: start + 2 + close,
                    });
                }
            }
            None
        }
        fn depth(source: &str) -> usize {
            let mut result = 0;
            let mut offset = 0;
            while offset < source.len() {
                if let Some(form) = inline_at(source, offset) {
                    let inner = match form.kind {
                        InlineKind::Strong(inner) | InlineKind::Emphasis(inner) => inner,
                        _ => unreachable!(),
                    };
                    result = result.max(1 + depth(inner.text));
                    offset = form.source.span.end as usize;
                } else {
                    offset += source[offset..].chars().next().unwrap().len_utf8();
                }
            }
            result
        }
        let mut cases = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..6 {
            frontier = frontier
                .iter()
                .flat_map(|prefix| ['*', '_', 'a', ' ', 'λ'].map(|ch| format!("{prefix}{ch}")))
                .collect();
            cases.extend(frontier.iter().cloned());
        }
        for source in &cases {
            let mut cursor = EmphasisCursor::new(source);
            for (start, _) in source.char_indices() {
                assert_eq!(
                    cursor.bounds(start),
                    original(source, start),
                    "{source:?} at {start}"
                );
            }
            assert!(depth(source) <= 4, "{source:?}");
        }
        assert_eq!(cases.len(), 19_531);
        assert_eq!(depth("**__*_x_*__**"), 4);
    }

    #[test]
    fn blocks_keep_original_ranges_and_line_breaks() {
        let source = "# Héading\r\n\r\n  first\r\n\tsecond\r\n\n- one\n  continued\n- two\n> quote\n> next\n\n```text\nλ\n```";
        let blocks = parse(source);
        assert_eq!(
            blocks.iter().map(|b| b.source.text).collect::<String>(),
            source
        );
        for block in &blocks {
            assert_eq!(
                &source[block.source.span.start as usize..block.source.span.end as usize],
                block.source.text
            );
        }
        let Kind::Paragraph(lines) = &blocks[2].kind else {
            panic!("paragraph");
        };
        assert_eq!(
            lines.iter().map(|l| l.text).collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(blocks[2].source.text, "  first\r\n\tsecond\r\n");
    }

    #[test]
    fn empty_heading_payloads_keep_source_owned_ranges() {
        let source = "λ\n#\n##  ##\n###";
        let blocks = parse(source);
        for (block, expected_offset) in blocks[1..].iter().zip([4, 9, 15]) {
            let Kind::Heading { text, .. } = &block.kind else {
                panic!("heading");
            };
            assert_eq!(text.text, "");
            assert_eq!(text.span.start, expected_offset);
            assert_eq!(text.span.end, expected_offset);
            assert_eq!(
                text.text.as_ptr(),
                source[expected_offset as usize..].as_ptr()
            );
        }
    }

    #[test]
    fn inline_forms_borrow_exact_utf8_ranges() {
        let source = "λ **bold** [`code`](url) *word*";
        let bold = inline_at(source, 3).unwrap();
        assert_eq!(bold.source.text, "**bold**");
        let InlineKind::Strong(inner) = bold.kind else {
            panic!("bold");
        };
        assert_eq!(inner.text, "bold");
        assert_eq!(
            &source[inner.span.start as usize..inner.span.end as usize],
            "bold"
        );
        assert!(inline_at(source, 1).is_none());
    }
}
