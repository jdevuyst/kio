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
//! Anything else — prose, ATX headings, HTML comments that don't
//! match the hidden-fence shape — is skipped over.
//!
//! The scanner does **not** parse attribute lists or the document
//! structure (harness/snippet/output pairings); those live in
//! [`super::attrs`] and [`super::document`]. This module's job is
//! to slice the markdown source into fence records carrying the
//! raw info string and body.

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
    let mut out = Vec::new();
    let mut lines = LineIter::new(source);

    while let Some(line) = lines.peek() {
        // Visible fenced code block: line starts with 0–3 spaces
        // then ≥3 backticks.
        if let Some((indent, fence_len, info)) = parse_visible_open(line.text) {
            let open_start = line.start as usize;
            let open_line_no = lines.line_number();
            lines.advance();
            let body_offset = lines.cursor();
            let mut body = String::new();
            let mut first_body_line = true;
            let mut close_end: Option<usize> = None;
            while let Some(inner) = lines.peek() {
                if is_visible_close(inner.text, indent, fence_len) {
                    close_end = Some((inner.start + inner.bytes_with_newline) as usize);
                    lines.advance();
                    break;
                }
                if !first_body_line {
                    body.push('\n');
                }
                body.push_str(inner.text);
                first_body_line = false;
                lines.advance();
            }
            let end = close_end.unwrap_or(source.len());
            let (lang, attrs_text) = split_info(info);
            out.push(Fence {
                lang,
                attrs_text,
                body,
                span: Span::new(open_start as u32, end as u32),
                hidden: false,
                body_offset,
                open_line: open_line_no,
            });
            continue;
        }

        // Hidden fence: `<!--LANG {attrs}` on a line, body, `-->`.
        if let Some(info) = parse_hidden_open(line.text) {
            let open_start = line.start as usize;
            let open_line_no = lines.line_number();
            lines.advance();
            let body_offset = lines.cursor();
            let mut body = String::new();
            let mut first_body_line = true;
            let mut close_end: Option<usize> = None;
            while let Some(inner) = lines.peek() {
                if is_hidden_close(inner.text) {
                    close_end = Some((inner.start + inner.bytes_with_newline) as usize);
                    lines.advance();
                    break;
                }
                if !first_body_line {
                    body.push('\n');
                }
                body.push_str(inner.text);
                first_body_line = false;
                lines.advance();
            }
            // If we never found the closer, treat this as a malformed
            // hidden fence and skip emitting it — the contract says
            // near-misses should be opaque prose, not parser errors.
            let Some(end) = close_end else {
                continue;
            };
            let (lang, attrs_text) = split_info(info);
            out.push(Fence {
                lang,
                attrs_text,
                body,
                span: Span::new(open_start as u32, end as u32),
                hidden: true,
                body_offset,
                open_line: open_line_no,
            });
            continue;
        }

        lines.advance();
    }

    out
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

/// End of an inline code span, unmatched backtick run, or Markdown escape.
pub(super) fn inline_literal_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes[start] == b'\\' && bytes.get(start + 1).is_some_and(u8::is_ascii_punctuation) {
        return Some(start + 2);
    }
    if bytes[start] != b'`' {
        return None;
    }
    let run_end = |from: usize| {
        from + bytes[from..]
            .iter()
            .take_while(|&&byte| byte == b'`')
            .count()
    };
    let open_end = run_end(start);
    let width = open_end - start;
    let line_start = text[..start].rfind('\n').map_or(0, |offset| offset + 1);
    let opening_line = text[line_start..].split('\n').next().unwrap();
    let in_heading = atx_heading(opening_line.trim_start()).is_some();
    let mut cursor = open_end;
    while cursor < bytes.len() {
        if bytes[cursor] == b'`' {
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

pub(super) fn ends_paragraph(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.is_empty()
        || code_fence_marker(trimmed).is_some()
        || atx_heading(trimmed).is_some()
        || is_thematic_break(trimmed)
        || is_html_block_start(trimmed)
        || trimmed.starts_with('>')
        || list_item_marker(trimmed).is_some()
}

/// True iff `line` begins a raw HTML block — a `<` followed by an
/// ASCII letter (open tag) or `/` (close tag). The block passes
/// through to the output verbatim.
pub(super) fn is_html_block_start(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.first() != Some(&b'<') || bytes.len() < 2 {
        return false;
    }
    bytes[1].is_ascii_alphabetic() || bytes[1] == b'/'
}

/// If `line` opens a fenced code block, return the fence marker
/// (a run of 3+ backticks or tildes).
pub(super) fn code_fence_marker(line: &str) -> Option<String> {
    for marker in ['`', '~'] {
        let run: String = line.chars().take_while(|&c| c == marker).collect();
        if run.len() >= 3 {
            return Some(run);
        }
    }
    None
}

/// Parse an ATX heading line, returning `(level, text)`.
pub(super) fn atx_heading(line: &str) -> Option<(usize, &str)> {
    let hashes: usize = line.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if rest.is_empty() {
        return Some((hashes, ""));
    }
    if !rest.starts_with(' ') {
        return None;
    }
    Some((hashes, rest.trim().trim_end_matches('#').trim_end()))
}

/// True for a thematic break (`---`, `***`, `___`, 3+ of one char).
pub(super) fn is_thematic_break(line: &str) -> bool {
    for marker in ['-', '*', '_'] {
        let stripped: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        if stripped.len() >= 3 && stripped.chars().all(|c| c == marker) {
            return true;
        }
    }
    false
}

/// If `line` is a list item, return the byte index where its content
/// begins (after the marker and following whitespace).
pub(super) fn list_item_marker(line: &str) -> Option<usize> {
    // Unordered: `-`, `*`, `+` followed by a space.
    if let Some(rest) = line.strip_prefix(['-', '*', '+'])
        && rest.starts_with(' ')
    {
        return Some(line.len() - rest.len() + 1);
    }
    // Ordered: digits then `.` or `)` then a space.
    let digits: usize = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits < line.len() {
        let after = &line[digits..];
        if (after.starts_with('.') || after.starts_with(')')) && after[1..].starts_with(' ') {
            return Some(digits + 2);
        }
    }
    None
}

/// Try to interpret `line` as a visible fence opener. Returns
/// `(indent, fence_len, info_string_after_marker)` on success.
///
/// Accepts 0–3 spaces of leading indentation followed by `\`{3,}`
/// (CommonMark fence opener). Tilde fences are deliberately not
/// supported — the Kiodoc contract is backtick-only.
fn parse_visible_open(line: &str) -> Option<(usize, usize, &str)> {
    let mut chars = line.char_indices().peekable();
    let mut indent = 0usize;
    while let Some(&(_, c)) = chars.peek() {
        if c == ' ' && indent < 3 {
            indent += 1;
            chars.next();
        } else {
            break;
        }
    }
    let (mark_start, mark_char) = chars.next()?;
    if mark_char != '`' {
        return None;
    }
    let mut fence_len = 1usize;
    while let Some(&(_, c)) = chars.peek() {
        if c == '`' {
            fence_len += 1;
            chars.next();
        } else {
            break;
        }
    }
    if fence_len < 3 {
        return None;
    }
    let after_idx = mark_start + fence_len; // backtick is 1 byte
    let after = &line[after_idx..];
    // CommonMark forbids backticks in the info string of a backtick
    // fence — reject if any appear.
    if after.contains('`') {
        return None;
    }
    Some((indent, fence_len, after))
}

/// Closing test for a visible fence: a line that's 0–3 spaces, then
/// ≥`fence_len` backticks, then nothing but whitespace. The
/// indent on the close is allowed to differ from the opener; only
/// the fence-character count must match or exceed.
fn is_visible_close(line: &str, _indent: usize, fence_len: usize) -> bool {
    let mut chars = line.char_indices().peekable();
    let mut leading = 0usize;
    while let Some(&(_, c)) = chars.peek() {
        if c == ' ' && leading < 3 {
            leading += 1;
            chars.next();
        } else {
            break;
        }
    }
    let mut count = 0usize;
    while let Some(&(_, c)) = chars.peek() {
        if c == '`' {
            count += 1;
            chars.next();
        } else {
            break;
        }
    }
    if count < fence_len {
        return false;
    }
    // Any trailing whitespace is fine; non-whitespace is not.
    for (_, c) in chars {
        if !c.is_whitespace() {
            return false;
        }
    }
    true
}

/// Try to interpret `line` as a hidden-fence opener of the form
/// `<!--LANG {attrs}` (or `<!--LANG` for fences with no attribute
/// list — rare but legal). Returns the info string after `<!--`.
///
/// The opening sequence has no leading-indent allowance — the
/// hidden-fence shape is exact, and a typo'd shape is correctly
/// treated as opaque prose by failing this match.
fn parse_hidden_open(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("<!--")?;
    // Reject `<!-- TODO ...` style comments by requiring no leading
    // whitespace before the language tag. This is the simplest way
    // to gate the "exact shape" rule.
    let first = rest.chars().next()?;
    if !is_lang_first_char(first) {
        return None;
    }
    // Reject opening lines that already terminate the comment on
    // the same line (`<!--kio {harness=x} -->`). The contract
    // requires a multi-line shape.
    if rest.contains("-->") {
        return None;
    }
    Some(rest)
}

/// First character of a language tag: ASCII letter (case-insensitive).
fn is_lang_first_char(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// Closing test for a hidden fence: a line that contains the
/// HTML-comment terminator `-->` (with no other characters
/// other than optional surrounding whitespace).
fn is_hidden_close(line: &str) -> bool {
    let t = line.trim();
    t == "-->"
}

/// Split an info string `<lang> [<attrs>]` into its language tag
/// and the trailing attribute text. Leading whitespace on the
/// attrs portion is stripped. An info string with no leading
/// non-whitespace word produces an empty `lang`.
fn split_info(info: &str) -> (String, Option<String>) {
    let trimmed = info.trim_start();
    let mut split_at = trimmed.len();
    for (idx, ch) in trimmed.char_indices() {
        if ch.is_whitespace() {
            split_at = idx;
            break;
        }
    }
    let (lang, rest) = trimmed.split_at(split_at);
    let attrs = rest.trim().to_owned();
    let attrs_text = if attrs.is_empty() { None } else { Some(attrs) };
    (lang.to_owned(), attrs_text)
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
    line_no: usize,
}

impl<'a> LineIter<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            line_no: 1,
        }
    }
    fn cursor(&self) -> u32 {
        self.pos as u32
    }
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
            self.line_no += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
