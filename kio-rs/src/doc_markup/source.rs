//! Lexer-authenticated documentation coordinates. Logical separators are LF;
//! original payload bytes and physical framing remain source-owned.

use std::ops::Range;

#[cfg(any(test, feature = "lsp"))]
use crate::pass::lexer::LexPrefix;
#[cfg(test)]
use crate::pass::lexer::lex_prefix;
use crate::pass::lexer::{Token, Trivia};
use crate::span::Span;

#[derive(Debug)]
struct DocLine {
    raw: Span,
    payload: Range<usize>,
    logical: Range<usize>,
    #[cfg(any(test, feature = "lsp"))]
    prefix: Option<Range<usize>>,
    #[cfg(any(test, feature = "lsp"))]
    newline: Option<&'static str>,
}

#[derive(Debug)]
pub(crate) struct DocSource<'a> {
    source: &'a str,
    pub(crate) text: String,
    lines: Vec<DocLine>,
}

#[derive(Debug, PartialEq, Eq)]
#[cfg(any(test, feature = "lsp"))]
pub(crate) struct SourceEdit {
    pub(crate) span: Span,
    pub(crate) replacement: String,
}

impl<'a> DocSource<'a> {
    #[cfg(test)]
    fn all(source: &'a str) -> Vec<Self> {
        let prefix = lex_prefix(source);
        Self::from_lexed(source, &prefix)
    }

    // The source and lexical result must describe the same document version.
    // Keeping this boundary separate permits reuse of the caller's lexical view.
    #[cfg(any(test, feature = "lsp"))]
    pub(crate) fn from_lexed(source: &'a str, prefix: &LexPrefix) -> Vec<Self> {
        Self::from_trivia(
            source,
            prefix
                .tokens
                .iter()
                .map(|token| token.leading_trivia.as_slice())
                .chain(std::iter::once(prefix.trailing.as_slice())),
            prefix.frontier,
        )
    }

    pub(crate) fn from_tokens(source: &'a str, tokens: &[Token], trailing: &[Trivia]) -> Vec<Self> {
        Self::from_trivia(
            source,
            tokens
                .iter()
                .map(|token| token.leading_trivia.as_slice())
                .chain(std::iter::once(trailing)),
            source.len() as u32,
        )
    }

    fn from_trivia<'t>(
        source: &'a str,
        trivia: impl Iterator<Item = &'t [Trivia]>,
        frontier: u32,
    ) -> Vec<Self> {
        let mut runs = Vec::new();
        for trivia in trivia {
            let mut spans = Vec::new();
            let mut newline = false;
            for item in trivia {
                match item {
                    Trivia::DocCommentLine { span, .. } if span.end <= frontier => {
                        spans.push(*span);
                        newline = false;
                    }
                    Trivia::Newline if !newline => newline = true,
                    _ => {
                        if !spans.is_empty() {
                            runs.push(Self::from_spans(source, &spans));
                            spans.clear();
                        }
                        newline = false;
                    }
                }
            }
            if !spans.is_empty() {
                runs.push(Self::from_spans(source, &spans));
            }
        }
        runs
    }

    // Called only with spans authenticated by the lexer, never a raw marker scan.
    fn from_spans(source: &'a str, spans: &[Span]) -> Self {
        let mut text = String::new();
        let mut lines = Vec::new();
        for &raw in spans {
            let start = raw.start as usize;
            let end = raw.end as usize;
            let mut payload_start = start + 3;
            if source.as_bytes().get(payload_start) == Some(&b' ') {
                payload_start += 1;
            }
            let (payload_end, _newline) = if source.as_bytes().get(end) == Some(&b'\n') {
                if source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
                    (end - 1, Some("\r\n"))
                } else {
                    (end, Some("\n"))
                }
            } else {
                (end, None)
            };
            #[cfg(any(test, feature = "lsp"))]
            let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
            #[cfg(any(test, feature = "lsp"))]
            let prefix = source[line_start..start]
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'\t'))
                .then_some(line_start..payload_start);
            if !lines.is_empty() {
                text.push('\n');
            }
            let logical_start = text.len();
            text.push_str(&source[payload_start..payload_end]);
            lines.push(DocLine {
                raw,
                payload: payload_start..payload_end,
                logical: logical_start..text.len(),
                #[cfg(any(test, feature = "lsp"))]
                prefix,
                #[cfg(any(test, feature = "lsp"))]
                newline: _newline,
            });
        }
        Self {
            source,
            text,
            lines,
        }
    }

    #[cfg(any(test, feature = "lsp"))]
    pub(crate) fn source_to_logical(&self, offset: usize) -> Option<usize> {
        if !self.source.is_char_boundary(offset) {
            return None;
        }
        self.lines.iter().find_map(|line| {
            (line.payload.start <= offset && offset <= line.payload.end)
                .then(|| line.logical.start + offset - line.payload.start)
        })
    }

    #[cfg(any(test, feature = "lsp"))]
    pub(crate) fn logical_to_source(&self, offset: usize) -> Option<usize> {
        if !self.text.is_char_boundary(offset) {
            return None;
        }
        self.lines.iter().find_map(|line| {
            (line.logical.start <= offset && offset <= line.logical.end)
                .then(|| line.payload.start + offset - line.logical.start)
        })
    }

    /// Map plain text only. Cross-line changes require uniform, code-free
    /// framing; snippets and edits outside this one authenticated run are absent.
    #[cfg(any(test, feature = "lsp"))]
    pub(crate) fn plain_edit(&self, range: Range<usize>, replacement: &str) -> Option<SourceEdit> {
        if range.start > range.end {
            return None;
        }
        let start = self.logical_to_source(range.start)?;
        let end = self.logical_to_source(range.end)?;
        let first = self.lines.iter().position(|line| {
            line.logical.start <= range.start && range.start <= line.logical.end
        })?;
        let last = self
            .lines
            .iter()
            .position(|line| line.logical.start <= range.end && range.end <= line.logical.end)?;
        let replacement = replacement.replace("\r\n", "\n");
        if replacement.contains('\r') {
            return None;
        }
        let line = &self.lines[first];
        let multiline = first != last || replacement.contains('\n');
        let newline = line
            .newline
            .or_else(|| self.lines.iter().rev().find_map(|line| line.newline))
            .unwrap_or("\n");
        let prefix = if multiline {
            let prefix = &self.source[line.prefix.clone()?];
            if self.lines[first..=last].iter().any(|line| {
                line.prefix
                    .as_ref()
                    .map(|range| &self.source[range.clone()])
                    != Some(prefix)
                    || line.newline.is_some_and(|style| style != newline)
            }) {
                return None;
            }
            prefix
        } else {
            ""
        };
        // A no-space marker is valid only before whitespace or end-of-line.
        // Replacing its leading tab must not turn `///\tname` into `///name`,
        // or promote a newly inserted space into stripped marker framing.
        let marker_has_space = self.source.as_bytes()[line.payload.start - 1] == b' ';
        let affected = format!(
            "{}{}{}",
            &self.text[line.logical.start..range.start],
            replacement,
            &self.text[range.end..self.lines[last].logical.end]
        );
        if !marker_has_space
            && affected.split('\n').any(|body| {
                body.as_bytes()
                    .first()
                    .is_some_and(|first| *first == b' ' || !first.is_ascii_whitespace())
            })
        {
            return None;
        }
        // Before bare LF a retained payload CR would become CRLF framing.
        // Before CRLF or EOF it remains an authored payload byte.
        let mut bodies = affected.split('\n').peekable();
        while let Some(body) = bodies.next() {
            let terminator = if bodies.peek().is_some() {
                Some(newline)
            } else {
                self.lines[last].newline
            };
            if body.ends_with('\r') && terminator == Some("\n") {
                return None;
            }
        }
        Some(SourceEdit {
            span: Span::new(start as u32, end as u32),
            replacement: replacement.replace('\n', &format!("{newline}{prefix}")),
        })
    }

    pub(crate) fn raw_lines(&self) -> impl Iterator<Item = Span> + '_ {
        self.lines.iter().map(|line| line.raw)
    }

    pub(crate) fn project(&self, span: Span) -> Vec<Span> {
        let start = span.start as usize;
        let end = span.end as usize;
        if start >= end || !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
            return Vec::new();
        }
        let first = self.lines.partition_point(|line| line.logical.end <= start);
        self.lines[first..]
            .iter()
            .take_while(|line| line.logical.start < end)
            .filter_map(|line| {
                let lo = start.max(line.logical.start);
                let hi = end.min(line.logical.end);
                if lo >= hi {
                    return None;
                }
                let lo = line.payload.start + lo - line.logical.start;
                let hi = line.payload.start + hi - line.logical.start;
                debug_assert!(self.source.is_char_boundary(lo) && self.source.is_char_boundary(hi));
                Some(Span::new(lo as u32, hi as u32))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(source: &str, edit: &SourceEdit) -> String {
        format!(
            "{}{}{}",
            &source[..edit.span.start as usize],
            edit.replacement,
            &source[edit.span.end as usize..]
        )
    }

    fn program_tokens(source: &str) -> Vec<(crate::pass::lexer::TokenKind, &str)> {
        let lexed = lex_prefix(source);
        assert!(lexed.error.is_none());
        lexed
            .tokens
            .into_iter()
            .map(|token| {
                (
                    token.kind,
                    &source[token.span.start as usize..token.span.end as usize],
                )
            })
            .collect()
    }

    #[test]
    fn original_payload_bytes_survive_lexer_canonicalization() {
        let source = "  /// é😀 \t\r\n\t///\tsecond  \r\n/// eof\r";
        let runs = DocSource::all(source);
        assert_eq!(runs.len(), 1);
        let doc = &runs[0];
        assert_eq!(doc.text, "é😀 \t\n\tsecond  \neof\r");
        assert_eq!(doc.source, source);
        assert_eq!(
            doc.lines[0]
                .prefix
                .as_ref()
                .map(|range| &source[range.clone()]),
            Some("  /// ")
        );
        assert_eq!(
            doc.lines[1]
                .prefix
                .as_ref()
                .map(|range| &source[range.clone()]),
            Some("\t///")
        );
        assert_eq!(doc.lines[0].newline, Some("\r\n"));
        let lexed = lex_prefix(source);
        let Trivia::DocCommentLine { text, .. } = &lexed.trailing[0] else {
            panic!("expected authenticated documentation");
        };
        assert_eq!(text, "é😀");
        assert_ne!(text, &doc.text[..doc.lines[0].logical.end]);
    }

    #[test]
    fn offsets_are_exact_utf8_payload_boundaries() {
        let source = "/// é😀\r\n  /// tail\n";
        let runs = DocSource::all(source);
        let doc = &runs[0];
        for line in &doc.lines {
            for source_offset in line
                .payload
                .clone()
                .chain(std::iter::once(line.payload.end))
            {
                let logical = doc.source_to_logical(source_offset);
                if source.is_char_boundary(source_offset) {
                    assert_eq!(doc.logical_to_source(logical.unwrap()), Some(source_offset));
                } else {
                    assert_eq!(logical, None);
                }
            }
        }
        for offset in 0..=source.len() + 1 {
            if !doc
                .lines
                .iter()
                .any(|line| line.payload.start <= offset && offset <= line.payload.end)
            {
                assert_eq!(doc.source_to_logical(offset), None, "offset {offset}");
            }
        }
        assert_eq!(doc.logical_to_source(1), None);
        assert_eq!(doc.logical_to_source(doc.text.len() + 1), None);
        assert_eq!(doc.logical_to_source(doc.lines[0].logical.end), Some(10));
        assert_eq!(doc.logical_to_source(doc.lines[1].logical.start), Some(18));
        assert!(doc.plain_edit(Range { start: 3, end: 1 }, "x").is_none());
        assert!(doc.plain_edit(0..doc.text.len() + 1, "x").is_none());
    }

    #[test]
    fn grouping_obeys_trivia_not_physical_proximity() {
        let source = "/// one\n/// two\n\n/// three\n// ordinary\n/// four\nmodule app;\n/// five\nfn f() -> . { () }\n/// eof";
        let runs = DocSource::all(source);
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<Vec<_>>(),
            ["one\ntwo", "three", "four", "five", "eof"]
        );
        let parsed = crate::pass::parser::parse(source).unwrap();
        assert_eq!(parsed.doc.unwrap().span, runs[2].lines[0].raw);
    }

    #[test]
    fn failed_lexical_frontier_never_authenticates_later_markers() {
        for suffix in [
            "\"bad\\q\n/// fake",
            "//// invalid\n/// fake",
            "///x\n/// fake",
        ] {
            let source = format!("/// real\n{suffix}");
            let lexed = lex_prefix(&source);
            assert!(lexed.error.is_some(), "{suffix}");
            let runs = DocSource::all(&source);
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].text, "real");
            assert!(runs[0].lines[0].raw.end <= lexed.frontier);
            assert!(
                runs[0]
                    .source_to_logical(source.rfind("fake").unwrap())
                    .is_none()
            );
        }
        assert!(DocSource::all("\"/// string\";").is_empty());
    }

    #[test]
    fn multiline_plain_edit_preserves_framing_and_program_tokens() {
        for newline in ["\n", "\r\n"] {
            let source = format!(
                "module app;{newline}\t/// before 😀 tail{newline}\t/// second{newline}fn f() -> () {{ () }}{newline}"
            );
            let runs = DocSource::all(&source);
            let doc = &runs[0];
            let start = doc.text.find('😀').unwrap();
            let end = doc.text.find("second").unwrap() + 3;
            let replacement = "x\nfn escaped() -> () { () }\n/// literal\n// literal";
            let edit = doc.plain_edit(start..end, replacement).unwrap();
            let changed = apply(&source, &edit);
            assert_eq!(program_tokens(&changed), program_tokens(&source));
            assert!(program_tokens(&changed).len() > 8);
            assert_eq!(
                &changed[..edit.span.start as usize],
                &source[..edit.span.start as usize]
            );
            let changed_runs = DocSource::all(&changed);
            assert_eq!(changed_runs.len(), 1);
            assert_eq!(changed_runs[0].lines.len(), 4);
            assert_eq!(changed_runs[0].text, format!("before {replacement}ond"));
            assert!(changed_runs[0].lines.iter().all(|line| {
                line.prefix.as_ref().map(|range| &changed[range.clone()]) == Some("\t/// ")
            }));
            assert!(
                changed_runs[0]
                    .lines
                    .iter()
                    .all(|line| line.newline == Some(newline))
            );
        }
    }

    #[test]
    fn same_line_code_prefix_is_never_replicated() {
        let source = "module app; /// trailing\nfn f() -> () { () }";
        let runs = DocSource::all(source);
        let doc = &runs[0];
        assert_eq!(doc.lines[0].prefix, None);
        assert!(doc.plain_edit(0..0, "new\nfn escaped() {}").is_none());
        let changed = apply(source, &doc.plain_edit(0..8, "changed").unwrap());
        assert_eq!(program_tokens(&changed), program_tokens(source));
        assert_eq!(DocSource::all(&changed)[0].text, "changed");
    }

    #[test]
    fn unsafe_marker_and_mixed_framing_edits_are_withheld() {
        let source = "///\tvalue\n///";
        let runs = DocSource::all(source);
        assert!(runs[0].plain_edit(0..1, "x").is_none());
        assert!(
            runs[0]
                .plain_edit(runs[0].text.len()..runs[0].text.len(), "x")
                .is_none()
        );
        let changed = apply(source, &runs[0].plain_edit(1..6, "other\n\tnext").unwrap());
        assert_eq!(DocSource::all(&changed)[0].text, "\tother\n\tnext\n");
        assert!(lex_prefix(&changed).error.is_none());
        for source in ["/// one\n  /// two", "/// one\r\n/// two\n/// three"] {
            let runs = DocSource::all(source);
            assert!(
                runs[0]
                    .plain_edit(0..runs[0].text.len(), "joined")
                    .is_none()
            );
        }
        assert!(runs[0].plain_edit(1..1, "bare\rreturn").is_none());
    }

    #[test]
    fn eof_insertion_and_crlf_input_remain_documentation() {
        let source = "module app;\n  /// end";
        let runs = DocSource::all(source);
        let changed = apply(source, &runs[0].plain_edit(3..3, "\r\nmore").unwrap());
        assert_eq!(changed, "module app;\n  /// end\n  /// more");
        assert_eq!(program_tokens(&changed), program_tokens(source));
    }

    #[test]
    fn edits_cannot_promote_authored_payload_cr_into_framing() {
        let mut withheld = Vec::new();
        for (source, range, replacement) in
            [("/// end\r", 4..4, "\nmore"), ("/// a\rX\n", 2..3, "")]
        {
            let runs = DocSource::all(source);
            let doc = &runs[0];
            assert!(doc.text.contains('\r'));
            withheld.push(doc.plain_edit(range, replacement).is_none());
        }
        assert_eq!(
            withheld,
            [true, true],
            "EOF insertion and interior deletion"
        );
        for (source, range, replacement, expected) in [
            ("/// a\rX", 2..3, "", "a\r"),
            ("/// a\rX\r\n", 2..3, "", "a\r"),
            ("/// end\r\r\n", 4..4, "\nmore", "end\r\nmore"),
        ] {
            let runs = DocSource::all(source);
            let edit = runs[0].plain_edit(range, replacement).unwrap();
            let changed = apply(source, &edit);
            assert!(lex_prefix(&changed).error.is_none());
            assert_eq!(DocSource::all(&changed)[0].text, expected);
        }
    }

    #[test]
    fn every_accepted_plain_edit_roundtrips_its_logical_text() {
        let mut accepted = 0;
        let mut withheld = 0;
        for (body, at_eof) in [
            ("/// α😀 tail\n/// second", false),
            ("///\tα😀\n///", false),
            ("  /// \r\n  /// tail", false),
            ("/// a\rX\n/// tail", false),
            ("/// a\r\r\n/// tail", false),
            ("/// end\r", true),
        ] {
            let suffix = if at_eof { "" } else { "\nfn f() -> . { () }" };
            let source = format!("module app;\n{body}{suffix}");
            let runs = DocSource::all(&source);
            let doc = &runs[0];
            let boundaries = doc
                .text
                .char_indices()
                .map(|(offset, _)| offset)
                .chain(std::iter::once(doc.text.len()))
                .collect::<Vec<_>>();
            for &start in &boundaries {
                for &end in boundaries.iter().filter(|&&end| end >= start) {
                    for replacement in [
                        "",
                        "x",
                        " space",
                        "\tvalue",
                        "\nnext",
                        "\n\tnext",
                        "fn escape() {}\n// payload",
                        "😀",
                    ] {
                        let Some(edit) = doc.plain_edit(start..end, replacement) else {
                            withheld += 1;
                            continue;
                        };
                        accepted += 1;
                        let changed = apply(&source, &edit);
                        assert_eq!(program_tokens(&changed), program_tokens(&source));
                        let changed_runs = DocSource::all(&changed);
                        assert_eq!(changed_runs.len(), 1);
                        assert_eq!(
                            changed_runs[0].text,
                            format!("{}{}{}", &doc.text[..start], replacement, &doc.text[end..]),
                            "source={source:?}, range={start}..{end}, replacement={replacement:?}"
                        );
                    }
                }
            }
        }
        assert!(accepted > 500);
        assert!(withheld > 0);
        eprintln!("plain-edit matrix: {accepted} accepted, {withheld} withheld");
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn existing_line_index_supplies_utf16_coordinates() {
        use crate::lsp::positions::{LineIndex, LspPosition};
        let source = "module app;\r\n\t/// é😀 tail\r\n";
        let index = LineIndex::new(source);
        let runs = DocSource::all(source);
        let doc = &runs[0];
        let byte = source.find(" tail").unwrap();
        let position = index.to_position(byte as u32);
        assert_eq!(
            position,
            LspPosition {
                line: 1,
                character: 8
            }
        );
        assert_eq!(index.position_to_offset(position) as usize, byte);
        assert_eq!(doc.source_to_logical(byte), Some("é😀".len()));
        let mid_surrogate = LspPosition {
            line: 1,
            character: 7,
        };
        let clamped = index.position_to_offset(mid_surrogate);
        assert_ne!(index.to_position(clamped), mid_surrogate);
        let outside = LspPosition {
            line: 50,
            character: 100,
        };
        assert_ne!(
            index.to_position(index.position_to_offset(outside)),
            outside
        );
        let edit = doc.plain_edit(0.."é😀".len(), "other").unwrap();
        let range = index.to_range(edit.span);
        assert_eq!(
            range.start,
            LspPosition {
                line: 1,
                character: 5
            }
        );
        assert_eq!(range.end, position);
    }
}
