//! Lossless Kiodoc batch fence boundaries, independent of snippet validation.

use super::lexical::SourceSlice;

#[derive(Debug, Clone, Copy)]
pub struct FenceSlice<'a> {
    pub source: SourceSlice<'a>,
    pub info: SourceSlice<'a>,
    pub body: SourceSlice<'a>,
    pub close: Option<SourceSlice<'a>>,
    #[cfg(all(feature = "surface", feature = "cli"))]
    pub hidden: bool,
    #[cfg(all(feature = "surface", feature = "cli"))]
    pub open_line: usize,
}

/// A lossless lexical partition. Prose retains Markdown block structure verbatim.
#[derive(Debug, Clone, Copy)]
pub enum Block<'a> {
    Prose(SourceSlice<'a>),
    Fence(FenceSlice<'a>),
}

impl<'a> Block<'a> {
    pub fn source(&self) -> SourceSlice<'a> {
        match self {
            Self::Prose(source) => *source,
            Self::Fence(fence) => fence.source,
        }
    }
}

/// Preserve physical line endings and byte ranges before any rendering or snippet normalization.
pub fn scan_blocks(source: &str) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut lines = LineIter::new(source);
    let mut prose_start = 0;

    while let Some(line) = lines.peek() {
        let (info, visible) = if let Some((indent, width, info)) = parse_visible_open(line.text) {
            (info, Some((indent, width)))
        } else if let Some(info) = parse_hidden_open(line.text) {
            (info, None)
        } else {
            lines.advance();
            continue;
        };
        let open_start = line.start as usize;
        let info_start = open_start + line.text.len() - info.len();
        let info_end = open_start + line.text.len();
        #[cfg(all(feature = "surface", feature = "cli"))]
        let open_line = lines.line_number();
        lines.advance();
        let body_start = lines.cursor() as usize;
        let mut body_end = source.len();
        let mut close = None;
        while let Some(inner) = lines.peek() {
            let closes = match visible {
                Some((indent, width)) => is_visible_close(inner.text, indent, width),
                None => is_hidden_close(inner.text),
            };
            if closes {
                body_end = inner.start as usize;
                close = Some(SourceSlice::new(
                    source,
                    body_end,
                    (inner.start + inner.bytes_with_newline) as usize,
                ));
                lines.advance();
                break;
            }
            lines.advance();
        }
        // A hidden opener without its closer remains opaque prose.
        if visible.is_none() && close.is_none() {
            continue;
        }
        if prose_start < open_start {
            out.push(Block::Prose(SourceSlice::new(
                source,
                prose_start,
                open_start,
            )));
        }
        let end = lines.cursor() as usize;
        out.push(Block::Fence(FenceSlice {
            source: SourceSlice::new(source, open_start, end),
            info: SourceSlice::new(source, info_start, info_end),
            body: SourceSlice::new(source, body_start, body_end),
            close,
            #[cfg(all(feature = "surface", feature = "cli"))]
            hidden: visible.is_none(),
            #[cfg(all(feature = "surface", feature = "cli"))]
            open_line,
        }));
        prose_start = end;
    }
    if prose_start < source.len() {
        out.push(Block::Prose(SourceSlice::new(
            source,
            prose_start,
            source.len(),
        )));
    }
    out
}

fn parse_visible_open(line: &str) -> Option<(usize, usize, &str)> {
    let facts = super::generated::fence_facts(line);
    facts
        .open_visible
        .then(|| (facts.leading, facts.width, &line[facts.marker_end..]))
}

fn is_visible_close(line: &str, _indent: usize, fence_len: usize) -> bool {
    let facts = super::generated::fence_facts(line);
    facts.close_visible && facts.width >= fence_len
}

fn parse_hidden_open(line: &str) -> Option<&str> {
    super::generated::fence_facts(line)
        .open_hidden
        .then(|| &line[4..])
}

fn is_hidden_close(line: &str) -> bool {
    super::generated::fence_facts(line).close_hidden
}

/// One line of the source, viewed without its terminator.
#[derive(Debug)]
struct Line<'a> {
    text: &'a str,
    start: u32,
    bytes_with_newline: u32,
}

/// Lightweight line iterator: tracks byte offset of each line and
/// supports a single-step lookahead via `peek`.
struct LineIter<'a> {
    source: &'a str,
    pos: usize,
    #[cfg(all(feature = "surface", feature = "cli"))]
    line_no: usize,
}

impl<'a> LineIter<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            #[cfg(all(feature = "surface", feature = "cli"))]
            line_no: 1,
        }
    }
    fn cursor(&self) -> u32 {
        self.pos as u32
    }
    #[cfg(all(feature = "surface", feature = "cli"))]
    fn line_number(&self) -> usize {
        self.line_no
    }
    fn peek(&self) -> Option<Line<'a>> {
        if self.pos >= self.source.len() {
            return None;
        }
        let rest = &self.source[self.pos..];
        let (line_len, newline_len) = match rest.find('\n') {
            Some(i) => (i, 1),
            None => (rest.len(), 0),
        };
        let text = &rest[..line_len];
        Some(Line {
            text,
            start: self.pos as u32,
            bytes_with_newline: (line_len + newline_len) as u32,
        })
    }
    fn advance(&mut self) {
        if let Some(line) = self.peek() {
            self.pos += line.bytes_with_newline as usize;
            #[cfg(all(feature = "surface", feature = "cli"))]
            {
                self.line_no += 1;
            }
        }
    }
}
