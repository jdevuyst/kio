//! Source-only documentation roles. Lexical reference roles do not assert resolution.

use super::fences::{Block, scan_blocks};
use super::lexical::{inline_literal_end, reference_definition_key};
use super::source::DocSource;
use crate::span::Span;
use crate::tokens::{ClassifiedToken, TokenKind};

// A single walk supplies both lexical roles and cursor candidates. Eligibility
// (scope, Markdown overrides and fence attributes) belongs to the caller.
fn prose_brackets(
    source: &str,
    mut visit: impl FnMut(usize, usize, usize, bool),
    mut opaque: impl FnMut(usize, usize, Option<TokenKind>),
) {
    let mut offset = 0;
    let mut links = super::generated::LinkCursor::new(source);
    while offset < source.len() {
        if let Some(end) = inline_literal_end(source, offset) {
            opaque(
                offset,
                end,
                Some(
                    if source.as_bytes()[offset] == super::generated::INLINE_CODE {
                        TokenKind::CommentDocCode
                    } else {
                        TokenKind::CommentDocMarker
                    },
                ),
            );
            offset = end;
            continue;
        }
        if offset == 0 || source.as_bytes()[offset - 1] == b'\n' {
            let line = source[offset..].split('\n').next().unwrap();
            if reference_definition_key(line).is_some() {
                opaque(offset, offset + line.len(), Some(TokenKind::CommentDocLink));
                offset += line.len();
                continue;
            }
        }
        if let Some((_, end)) = links.bounds(offset) {
            opaque(offset, end + 1, Some(TokenKind::CommentDocLink));
            offset = end + 1;
            continue;
        }
        if let Some((start, end, complete)) = super::generated::bracket_bounds(source, offset) {
            let after = end + if complete { 2 } else { 0 };
            opaque(offset, after, None);
            if !complete || !source[after..].trim_start().starts_with(['(', '[']) {
                visit(offset, start, end, complete);
            }
            offset = after;
            continue;
        }
        offset += source[offset..].chars().next().unwrap().len_utf8();
    }
}

#[cfg(feature = "lsp")]
pub(crate) enum DocCursor<'a> {
    Keyword {
        replacement: std::ops::Range<usize>,
    },
    Operand {
        replacement: std::ops::Range<usize>,
        directive: Option<super::generated::DirectiveKeyword>,
    },
    Fence {
        body: std::ops::Range<usize>,
        info: &'a str,
    },
}

#[cfg(feature = "lsp")]
pub(crate) fn completion_cursor<'a>(
    doc: &'a DocSource<'_>,
    offset: usize,
) -> Option<DocCursor<'a>> {
    use super::generated::DirectiveKeyword;
    if !doc.text.is_char_boundary(offset) {
        return None;
    }
    for block in scan_blocks(&doc.text) {
        match block {
            Block::Fence(fence) => {
                let body = fence.body.span.start as usize..fence.body.span.end as usize;
                if body.start <= offset && offset <= body.end {
                    return Some(DocCursor::Fence {
                        body,
                        info: fence.info.text,
                    });
                }
            }
            Block::Prose(prose) => {
                let base = prose.span.start as usize;
                if !(base..=prose.span.end as usize).contains(&offset) {
                    continue;
                }
                let mut candidate = None;
                prose_brackets(
                    prose.text,
                    |_, start, end, _| {
                        let local = offset - base;
                        if !(start..=end).contains(&local) {
                            return;
                        }
                        let mut operand = start;
                        let mut directive = None;
                        if prose.text.as_bytes().get(start) == Some(&b'@') {
                            let keyword = start + 1;
                            let keyword_end = keyword
                                + prose.text[keyword..end]
                                    .bytes()
                                    .take_while(u8::is_ascii_alphabetic)
                                    .count();
                            if keyword_end < end
                                && !prose.text[keyword_end..end].starts_with(char::is_whitespace)
                            {
                                return;
                            }
                            if (keyword..=keyword_end).contains(&local) {
                                candidate = Some(DocCursor::Keyword {
                                    replacement: base + keyword..base + keyword_end,
                                });
                                return;
                            }
                            let Some(known) =
                                DirectiveKeyword::parse(&prose.text[keyword..keyword_end])
                            else {
                                return;
                            };
                            directive = Some(known);
                            operand = keyword_end;
                        }
                        let text = prose.text[operand..end].trim();
                        let start = operand + prose.text[operand..end].len()
                            - prose.text[operand..end].trim_start().len();
                        let replacement = if text.is_empty() {
                            base + local..base + local
                        } else {
                            if !(start..=start + text.len()).contains(&local) {
                                return;
                            }
                            base + start..base + start + text.len()
                        };
                        candidate = Some(DocCursor::Operand {
                            replacement,
                            directive,
                        });
                    },
                    |_, _, _| {},
                );
                if candidate.is_some() {
                    return candidate;
                }
            }
        }
    }
    None
}

pub(crate) fn classify(doc: &DocSource<'_>) -> Vec<ClassifiedToken> {
    let mut logical = Vec::new();
    let mut markup = Vec::new();
    for block in scan_blocks(&doc.text) {
        let block_start = block.source().span.start;
        match block {
            Block::Fence(fence) => {
                let (_, attributes) = super::generated::fence_info_parts(fence.info.text);
                let attribute_start = if attributes.is_empty() {
                    fence.info.span.end
                } else {
                    fence.info.span.start
                        + (attributes.as_ptr() as usize - fence.info.text.as_ptr() as usize) as u32
                };
                logical.push((
                    Span::new(block_start, attribute_start),
                    TokenKind::CommentDocFence,
                ));
                if !attributes.is_empty() {
                    logical.push((
                        Span::new(attribute_start, attribute_start + attributes.len() as u32),
                        TokenKind::CommentDocAttribute,
                    ));
                }
                if super::generated::is_kio_info(fence.info.text) {
                    let mut cursor = fence.body.span.start;
                    for token in crate::tokens::dump_doc_code(
                        fence.body.text,
                        super::generated::self_harness_info(fence.info.text),
                    ) {
                        let span = Span::new(
                            fence.body.span.start + token.span.start,
                            fence.body.span.start + token.span.end,
                        );
                        if cursor < span.start {
                            logical
                                .push((Span::new(cursor, span.start), TokenKind::CommentDocCode));
                        }
                        logical.push((span, token.kind));
                        cursor = span.end;
                    }
                    if cursor < fence.body.span.end {
                        logical.push((
                            Span::new(cursor, fence.body.span.end),
                            TokenKind::CommentDocCode,
                        ));
                    }
                } else {
                    logical.push((fence.body.span, TokenKind::CommentDocCode));
                }
                if let Some(close) = fence.close {
                    logical.push((close.span, TokenKind::CommentDocFence));
                }
            }
            Block::Prose(prose) => {
                let source = prose.text;
                let mut opaque = Vec::new();
                prose_brackets(
                    source,
                    |_, start, end, _| {
                        let mut operand = start;
                        if source.as_bytes().get(start) == Some(&b'@') {
                            let keyword_end = start
                                + 1
                                + source[start + 1..end]
                                    .bytes()
                                    .take_while(u8::is_ascii_alphabetic)
                                    .count();
                            logical.push((
                                Span::new(
                                    prose.span.start + start as u32,
                                    prose.span.start + keyword_end as u32,
                                ),
                                TokenKind::CommentDocDirective,
                            ));
                            operand = keyword_end;
                        }
                        let text = source[operand..end].trim();
                        let leading =
                            source[operand..end].len() - source[operand..end].trim_start().len();
                        if !text.is_empty() {
                            let start = prose.span.start + (operand + leading) as u32;
                            logical.push((
                                Span::new(start, start + text.len() as u32),
                                TokenKind::CommentDocReference,
                            ));
                        }
                    },
                    |start, end, kind| {
                        opaque.push((
                            Span::new(
                                prose.span.start + start as u32,
                                prose.span.start + end as u32,
                            ),
                            kind,
                        ));
                    },
                );
                markdown_roles(source, prose.span.start, &opaque, &mut markup);
            }
        }
    }
    let structured = merge_roles(logical, markup)
        .into_iter()
        .flat_map(|(span, kind)| {
            doc.project(span)
                .into_iter()
                .map(move |span| ClassifiedToken {
                    span,
                    kind,
                    in_documentation: true,
                })
        })
        .collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    for line in doc.raw_lines() {
        let mut cursor = line.start;
        while let Some(token) = structured.get(index) {
            if token.span.start >= line.end {
                break;
            }
            if cursor < token.span.start {
                tokens.push(ClassifiedToken {
                    span: Span::new(cursor, token.span.start),
                    kind: TokenKind::CommentDoc,
                    in_documentation: true,
                });
            }
            tokens.push(token.clone());
            cursor = token.span.end;
            index += 1;
        }
        if cursor < line.end {
            tokens.push(ClassifiedToken {
                span: Span::new(cursor, line.end),
                kind: TokenKind::CommentDoc,
                in_documentation: true,
            });
        }
    }
    tokens
}

type MarkupRole = (Span, TokenKind, u8);

fn markdown_roles(
    source: &str,
    base: u32,
    opaque: &[(Span, Option<TokenKind>)],
    roles: &mut Vec<MarkupRole>,
) {
    roles.extend(
        opaque
            .iter()
            .filter_map(|&(span, kind)| kind.map(|kind| (span, kind, 100))),
    );
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let raw = line.strip_suffix('\n').unwrap_or(line);
        let mut text = raw.trim_start();
        let mut start = offset + raw.len() - text.len();
        while let Some(width) = super::generated::quote_marker(text) {
            roles.push((
                Span::new(base + start as u32, base + (start + width) as u32),
                TokenKind::CommentDocMarker,
                1,
            ));
            let rest = &text[width..];
            text = rest.trim_start();
            start += width + rest.len() - text.len();
        }
        if let Some(width) = super::generated::list_marker(text) {
            roles.push((
                Span::new(base + start as u32, base + (start + width) as u32),
                TokenKind::CommentDocMarker,
                1,
            ));
        } else if super::generated::inline_heading(text) {
            roles.push((
                Span::new(base + start as u32, base + (offset + raw.len()) as u32),
                TokenKind::CommentDocHeading,
                0,
            ));
        } else if super::lexical::is_thematic_break(text) {
            roles.push((
                Span::new(base + start as u32, base + (offset + raw.len()) as u32),
                TokenKind::CommentDocMarker,
                1,
            ));
        }
        offset += line.len();
    }
    for block in super::blocks::parse(source) {
        let mut inline = |start: usize, end: usize| {
            markdown_inline_roles(&source[start..end], base + start as u32, 0, opaque, roles);
        };
        let mut lines = |lines: &[super::lexical::SourceSlice<'_>]| {
            if let (Some(first), Some(last)) = (lines.first(), lines.last()) {
                inline(first.span.start as usize, last.span.end as usize);
            }
        };
        match block.kind {
            super::blocks::Kind::Heading { text, .. } => {
                inline(text.span.start as usize, text.span.end as usize);
            }
            super::blocks::Kind::Paragraph(payload) => lines(&payload),
            super::blocks::Kind::List { items, .. } => {
                for item in items {
                    lines(&item);
                }
            }
            super::blocks::Kind::Quote(payload) => {
                for line in payload {
                    inline(line.span.start as usize, line.span.end as usize);
                }
            }
            _ => {}
        }
    }
}

fn markdown_inline_roles(
    source: &str,
    base: u32,
    depth: u8,
    opaque: &[(Span, Option<TokenKind>)],
    roles: &mut Vec<MarkupRole>,
) {
    // A first-closing-delimiter payload excludes its own delimiter. Each of
    // the four emphasis forms can occur at most once along a nested path.
    if depth == 4 {
        return;
    }
    let mut offset = 0;
    let mut emphasis = super::generated::EmphasisCursor::new(source);
    let mut literal = opaque.partition_point(|(span, _)| span.end <= base);
    while offset < source.len() {
        while literal < opaque.len() && opaque[literal].0.end <= base + offset as u32 {
            literal += 1;
        }
        if let Some(&(span, _)) = opaque.get(literal)
            && span.start <= base + offset as u32
        {
            offset = (span.end - base) as usize;
            continue;
        }
        let mut push = |end: usize, kind, priority| {
            roles.push((
                Span::new(base + offset as u32, base + end as u32),
                kind,
                priority,
            ));
        };
        if let Some(bounds) = emphasis.bounds(offset) {
            push(
                bounds.end,
                if bounds.width == 2 {
                    TokenKind::CommentDocStrong
                } else {
                    TokenKind::CommentDocEmphasis
                },
                10 + depth,
            );
            markdown_inline_roles(
                &source[offset + bounds.width..bounds.end - bounds.width],
                base + (offset + bounds.width) as u32,
                depth + 1,
                opaque,
                roles,
            );
            offset = bounds.end;
            continue;
        }
        offset += source[offset..].chars().next().unwrap().len_utf8();
    }
}

fn merge_roles(
    logical: Vec<(Span, TokenKind)>,
    mut markup: Vec<MarkupRole>,
) -> Vec<(Span, TokenKind)> {
    if markup.is_empty() {
        return logical;
    }
    markup.extend(logical.into_iter().map(|(span, kind)| (span, kind, 128)));
    let mut boundaries = Vec::with_capacity(markup.len() * 2);
    for (index, (span, _, _)) in markup.iter().enumerate() {
        if span.start < span.end {
            boundaries.push((span.start, Some(index)));
            boundaries.push((span.end, None));
        }
    }
    boundaries.sort_unstable();
    let mut active = std::collections::BinaryHeap::new();
    let mut output: Vec<(Span, TokenKind)> = Vec::new();
    let mut index = 0;
    while index < boundaries.len() {
        let start = boundaries[index].0;
        while index < boundaries.len() && boundaries[index].0 == start {
            if let Some(role) = boundaries[index].1 {
                active.push((markup[role].2, role));
            }
            index += 1;
        }
        while active
            .peek()
            .is_some_and(|&(_, role)| markup[role].0.end <= start)
        {
            active.pop();
        }
        let (Some(&(_, role)), Some(&(end, _))) = (active.peek(), boundaries.get(index)) else {
            continue;
        };
        let kind = markup[role].1;
        if let Some((last, last_kind)) = output.last_mut()
            && last.end == start
            && *last_kind == kind
        {
            last.end = end;
        } else {
            output.push((Span::new(start, end), kind));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::lexer::lex_with_trailing;

    #[test]
    fn live_roles_reuse_failed_link_searches_linearly() {
        for count in [256, 512, 1024] {
            for payload in ["[".repeat(count), "[x](".repeat(count)] {
                let source = format!("/// {payload}");
                super::super::generated::LINK_SEARCH_BYTES.with(|bytes| bytes.set(0));
                let _ = roles(&source);
                let searched = super::super::generated::LINK_SEARCH_BYTES.with(|bytes| bytes.get());
                assert!(
                    searched <= 2 * payload.len(),
                    "{} source bytes searched {searched} bytes",
                    payload.len()
                );
            }
        }
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn cursor_recovery_requires_real_delimiters_and_valid_operand_boundaries() {
        for (body, expected) in [
            ("[`@ty|", "keyword"),
            ("[`@type   |value`]", "operand"),
            ("[`@type |  value`]", "none"),
            ("[`@type   |`]", "operand"),
            ("[`@type.f|oo`]", "none"),
            ("[`@unknown x|`]", "none"),
            ("``[`x|`]``", "none"),
            ("[link](url/[`x|`])", "none"),
            ("[`pkg/module.(+)|`]", "operand"),
        ] {
            let source = format!("/// {body}");
            let offset = source.find('|').unwrap();
            let source = source.replace('|', "");
            let (tokens, trailing) = lex_with_trailing(&source).unwrap();
            let docs = DocSource::from_tokens(&source, &tokens, &trailing);
            let doc = &docs[0];
            let cursor = doc.source_to_logical(offset).unwrap();
            let actual = match completion_cursor(doc, cursor) {
                Some(DocCursor::Keyword { replacement }) => {
                    assert!(replacement.contains(&cursor) || replacement.end == cursor);
                    "keyword"
                }
                Some(DocCursor::Operand { replacement, .. }) => {
                    assert!(replacement.start <= cursor && cursor <= replacement.end);
                    "operand"
                }
                Some(DocCursor::Fence { .. }) => "fence",
                None => "none",
            };
            assert_eq!(actual, expected, "{body:?}");
        }
    }

    fn roles(source: &str) -> Vec<(String, TokenKind)> {
        let (tokens, trailing) = lex_with_trailing(source).unwrap();
        DocSource::from_tokens(source, &tokens, &trailing)
            .iter()
            .flat_map(classify)
            .filter(|token| token.kind != TokenKind::CommentDoc)
            .map(|token| {
                (
                    source[token.span.start as usize..token.span.end as usize].to_owned(),
                    token.kind,
                )
            })
            .collect()
    }

    #[test]
    fn embedded_kio_uses_original_ranges_without_recursive_documentation() {
        let source = "/// é\r\n\t/// ````kio {@}\r\n\t/// let value = 1(Int);\r\n\t/// /// [`nested_doc`]\r\n\t/// value\r\n\t/// ````\r\nfn actual() { () }";
        let tokens = crate::tokens::dump(source).unwrap();
        let keyword = source.find("let value").unwrap() as u32;
        assert!(tokens.iter().any(|token| {
            token.span == Span::new(keyword, keyword + 3)
                && matches!(
                    token.kind,
                    TokenKind::KeywordControl | TokenKind::KeywordDeclaration
                )
                && token.in_documentation
        }));
        assert!(
            !tokens
                .iter()
                .any(|token| token.kind == TokenKind::CommentDocReference)
        );
        let actual = source.rfind("fn actual").unwrap() as u32;
        assert!(
            tokens
                .iter()
                .any(|token| token.span.start == actual && !token.in_documentation)
        );
        for token in &tokens {
            assert!(source.is_char_boundary(token.span.start as usize));
            assert!(source.is_char_boundary(token.span.end as usize));
        }
        assert!(
            tokens
                .windows(2)
                .all(|pair| pair[0].span.end <= pair[1].span.start)
        );
        assert!(tokens.iter().any(|token| {
            token.kind == TokenKind::CommentDocAttribute
                && &source[token.span.start as usize..token.span.end as usize] == "{@}"
        }));
        let module = "/// ```kio\n/// module sample;\n/// fn entry() { () }\n/// ```";
        assert!(
            roles(module)
                .iter()
                .any(|(text, kind)| { text == "fn" && *kind == TokenKind::KeywordDeclaration })
        );
    }

    #[test]
    fn anonymous_harness_hint_preserves_attribute_boundaries_and_body_ranges() {
        for (info, expected) in [
            ("kio {@}", true),
            ("kio { \t@ check_exit_code=14}", true),
            ("kio {@", true),
            ("kio { @ ", true),
            ("kio {@name}", false),
            ("kio {@name", false),
            ("kio { @λ}", false),
            ("kiography {@}", false),
            ("text {@}", false),
            ("kio {\u{a0}@}", false),
        ] {
            assert_eq!(
                super::super::generated::self_harness_info(info),
                expected,
                "{info:?}"
            );
        }
        for source in [
            "",
            "let value = ();\nvalue",
            "let value =",
            "let(())",
            "}\nmodule escaped;",
            "\"unterminated",
        ] {
            let tokens = crate::tokens::dump_doc_code(source, true);
            assert!(
                tokens
                    .iter()
                    .all(|token| token.span.end as usize <= source.len()
                        && source.is_char_boundary(token.span.start as usize)
                        && source.is_char_boundary(token.span.end as usize))
            );
            if source.starts_with("let value") {
                assert!(
                    tokens.iter().any(|token| token.span == Span::new(0, 3)
                        && token.kind == TokenKind::KeywordDeclaration),
                    "{source:?}: {tokens:?}"
                );
            }
            if source == "let(())" {
                assert!(
                    tokens
                        .iter()
                        .all(|token| token.kind != TokenKind::KeywordDeclaration)
                );
            }
        }
    }

    #[test]
    fn markdown_roles_respect_blocks_and_keep_reference_opacity() {
        let source = "/// # Heading\n/// **strong** and *emphasis* [`name`].\n/// - item\n/// > quote\n/// [link](url)\n///\n/// *first\n/// second*\n///\n/// *unclosed\n///\n/// closed*\n/// - *one\n/// - two*\n/// > *quote\n/// > next*";
        let actual = roles(source);
        for (text, kind) in [
            ("# Heading", TokenKind::CommentDocHeading),
            ("**strong**", TokenKind::CommentDocStrong),
            ("*emphasis*", TokenKind::CommentDocEmphasis),
            ("name", TokenKind::CommentDocReference),
            ("- ", TokenKind::CommentDocMarker),
            ("> ", TokenKind::CommentDocMarker),
            ("[link](url)", TokenKind::CommentDocLink),
            ("*first", TokenKind::CommentDocEmphasis),
            ("second*", TokenKind::CommentDocEmphasis),
        ] {
            assert!(
                actual.contains(&(text.into(), kind)),
                "{text:?}: {actual:?}"
            );
        }
        for (text, kind) in actual {
            if kind == TokenKind::CommentDocEmphasis {
                assert!(
                    !text.contains("unclosed")
                        && !text.contains("closed*")
                        && !text.contains("one")
                        && !text.contains("two")
                        && !text.contains("quote")
                        && !text.contains("next")
                );
            }
        }
    }

    #[test]
    fn source_roles_keep_literals_destinations_and_batch_fences_distinct() {
        let source = "/// [`name`]; [`@signature pkg/name.item`]; [`@ty\n/// ``[`literal`]`` \\[`escaped`] [link](url/[`destination`])\n/// [key]: url/[`definition`]\n/// ```text\n/// [`code`]\n/// ````\n/// <!--text\n/// [`hidden`]\n/// -->\n/// <!--kio\n/// [`unclosed_is_prose`]";
        assert_eq!(
            roles(source),
            vec![
                ("name".into(), TokenKind::CommentDocReference),
                ("@signature".into(), TokenKind::CommentDocDirective),
                ("pkg/name.item".into(), TokenKind::CommentDocReference),
                ("@ty".into(), TokenKind::CommentDocDirective),
                ("``[`literal`]``".into(), TokenKind::CommentDocCode),
                ("\\[".into(), TokenKind::CommentDocMarker),
                ("`escaped`".into(), TokenKind::CommentDocCode),
                (
                    "[link](url/[`destination`])".into(),
                    TokenKind::CommentDocLink
                ),
                (
                    "[key]: url/[`definition`]".into(),
                    TokenKind::CommentDocLink
                ),
                ("```text".into(), TokenKind::CommentDocFence),
                ("[`code`]".into(), TokenKind::CommentDocCode),
                ("````".into(), TokenKind::CommentDocFence),
                ("<!--text".into(), TokenKind::CommentDocFence),
                ("[`hidden`]".into(), TokenKind::CommentDocCode),
                ("-->".into(), TokenKind::CommentDocFence),
                ("unclosed_is_prose".into(), TokenKind::CommentDocReference),
            ]
        );
        assert_eq!(roles("/// [`linked`] [destination]"), Vec::new());
    }

    #[test]
    fn physical_ranges_partition_doc_lines_without_prefix_or_unicode_drift() {
        let source =
            "/// é [`😀`].\r\n\t/// [`@source x`]  \r\nmodule app; // [`ordinary`]\n/// [`eof`]";
        let (tokens, trailing) = lex_with_trailing(source).unwrap();
        for doc in DocSource::from_tokens(source, &tokens, &trailing) {
            let classified = classify(&doc);
            for raw in doc.raw_lines() {
                let segments = classified
                    .iter()
                    .filter(|token| raw.start <= token.span.start && token.span.end <= raw.end)
                    .collect::<Vec<_>>();
                assert_eq!(segments.first().unwrap().span.start, raw.start);
                assert_eq!(segments.last().unwrap().span.end, raw.end);
                assert!(
                    segments
                        .windows(2)
                        .all(|pair| pair[0].span.end == pair[1].span.start)
                );
                assert!(
                    segments
                        .iter()
                        .all(|token| source.is_char_boundary(token.span.start as usize)
                            && source.is_char_boundary(token.span.end as usize))
                );
            }
        }
        assert_eq!(
            roles(source)
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>(),
            ["😀", "@source", "x", "eof"]
        );
    }
}
