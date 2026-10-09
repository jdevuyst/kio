//! Shared literal/bracket rewriting with an explicit inert presentation policy.

use super::generated::DirectiveKeyword;
use super::lexical::bracket_at;
use std::collections::HashSet;

/// Render references and directive operands as inert code, without resolving or expanding them.
/// Markdown overrides, literals, and fence bodies retain their authored text.
pub(crate) fn rewrite_inert(prose: &str) -> String {
    use super::blocks;
    let blocks = blocks::parse(prose);
    let mut overrides = HashSet::new();
    collect_inert_overrides(prose, &blocks, &mut overrides, 0);
    rewrite_inert_blocks(prose, blocks, &overrides, 0)
}

fn collect_inert_overrides(
    prose: &str,
    blocks: &[super::blocks::Block<'_>],
    overrides: &mut HashSet<String>,
    depth: usize,
) {
    use super::blocks::{self, Kind};
    if depth >= super::MAX_RENDER_DEPTH {
        return;
    }
    for block in blocks {
        if is_indented_code(block.source.text) {
            continue;
        }
        let groups = match &block.kind {
            Kind::Code { .. } | Kind::Html => continue,
            Kind::Quote(lines) => vec![lines.clone()],
            Kind::List { items, .. } => items
                .iter()
                .map(|lines| blocks::native_list_payloads(prose, lines))
                .collect(),
            _ => {
                overrides.extend(
                    block
                        .source
                        .text
                        .lines()
                        .filter_map(super::lexical::reference_definition_key)
                        .map(|key| key.text.to_owned()),
                );
                continue;
            }
        };
        for lines in groups {
            let payload = lines
                .iter()
                .map(|line| line.text)
                .collect::<Vec<_>>()
                .join("\n");
            collect_inert_overrides(&payload, &blocks::parse(&payload), overrides, depth + 1);
        }
    }
}

fn rewrite_inert_blocks(
    prose: &str,
    blocks: Vec<super::blocks::Block<'_>>,
    overrides: &HashSet<String>,
    depth: usize,
) -> String {
    use super::blocks::{self, Kind};
    if depth >= super::MAX_RENDER_DEPTH {
        return blocks.iter().map(|block| block.source.text).collect();
    }
    let mut render = |parsed: &KiodocBracket<'_>| {
        let term = match parsed.kind {
            BracketKind::Directive { keyword, term } => {
                if DirectiveKeyword::parse(keyword).is_none() {
                    return parsed.source.to_owned();
                }
                term
            }
            BracketKind::Ref { name } => name,
        };
        if overrides.contains(term) {
            parsed.source.to_owned()
        } else {
            format!("`{term}`")
        }
    };
    let mut out = String::new();
    for block in blocks {
        if is_indented_code(block.source.text) {
            out.push_str(block.source.text);
            continue;
        }
        match block.kind {
            Kind::Code { .. } | Kind::Html => out.push_str(block.source.text),
            Kind::Quote(_) | Kind::List { .. } => {
                let groups = match block.kind {
                    Kind::Quote(lines) => vec![lines],
                    Kind::List { items, .. } => items
                        .iter()
                        .map(|lines| blocks::native_list_payloads(prose, lines))
                        .collect(),
                    _ => unreachable!("quote or list payload selected above"),
                };
                let mut cursor = 0;
                // Inert substitutions neither add nor remove physical lines, so
                // each transformed payload keeps its authored quote/list prefix.
                for lines in groups {
                    let payload = lines
                        .iter()
                        .map(|line| line.text)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let rewritten = rewrite_inert_blocks(
                        &payload,
                        blocks::parse(&payload),
                        overrides,
                        depth + 1,
                    );
                    let mut rewritten_lines = rewritten.split('\n');
                    for line in lines {
                        let start = (line.span.start - block.source.span.start) as usize;
                        let end = (line.span.end - block.source.span.start) as usize;
                        out.push_str(&block.source.text[cursor..start]);
                        out.push_str(
                            rewritten_lines
                                .next()
                                .expect("inert rewriting preserves physical lines"),
                        );
                        cursor = end;
                    }
                    debug_assert!(rewritten_lines.next().is_none());
                }
                out.push_str(&block.source.text[cursor..]);
            }
            _ => out.push_str(&rewrite_prose_segment(
                block.source.text,
                inert_literal_end,
                &mut render,
            )),
        }
    }
    out
}

pub(crate) fn is_indented_code(text: &str) -> bool {
    text.starts_with("    ") || text.starts_with('\t')
}

fn inert_literal_end(prose: &str, start: usize) -> Option<usize> {
    use crate::doc_markup::lexical::{
        inline_link_at, inline_literal_end, reference_definition_key,
    };
    if let Some(end) = inline_literal_end(prose, start) {
        return Some(end);
    }
    if start == 0 || prose.as_bytes()[start - 1] == b'\n' {
        let line = prose[start..].split('\n').next().unwrap();
        if reference_definition_key(line).is_some() {
            return Some(start + line.len());
        }
    }
    inline_link_at(prose, start).map(|link| link.source.span.end as usize)
}

pub(crate) fn rewrite_prose_segment(
    prose: &str,
    literal_end: fn(&str, usize) -> Option<usize>,
    render: &mut impl FnMut(&KiodocBracket<'_>) -> String,
) -> String {
    let bytes = prose.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        if let Some(end) = literal_end(prose, i) {
            out.push_str(&prose[i..end]);
            i = end;
            continue;
        }
        if bytes[i] != b'[' {
            push_char(&mut out, prose, &mut i);
            continue;
        }
        // Try to parse a `` [`...`] `` Kiodoc bracket form starting
        // here. If it doesn't match, emit the `[` verbatim.
        match parse_kiodoc_bracket(&prose[i..]) {
            Some(parsed) => {
                let consumed = parsed.source.len();
                // A trailing `(` or `[` makes this a plain Markdown
                // link, not a Kiodoc form — leave it verbatim.
                let after = prose[i + consumed..].trim_start();
                if after.starts_with('(') || after.starts_with('[') {
                    push_char(&mut out, prose, &mut i);
                    continue;
                }
                let rendered = render(&parsed);
                out.push_str(&rendered);
                i += consumed;
            }
            None => push_char(&mut out, prose, &mut i),
        }
    }
    out
}

/// Copy the next UTF-8 character of `s` (starting at `*i`) to `out`,
/// advancing `*i`.
fn push_char(out: &mut String, s: &str, i: &mut usize) {
    let ch = s[*i..].chars().next().unwrap();
    out.push(ch);
    *i += ch.len_utf8();
}

/// A parsed `` [`...`] `` Kiodoc bracket form.
pub(crate) struct KiodocBracket<'a> {
    pub(crate) kind: BracketKind<'a>,
    pub(crate) source: &'a str,
}

pub(crate) enum BracketKind<'a> {
    /// `` [`@signature term`] ``, `` [`@source term`] ``, or
    /// `` [`@type term`] ``.
    Directive { keyword: &'a str, term: &'a str },
    /// `` [`name`] ``.
    Ref { name: &'a str },
}

/// Parse a `` [`...`] `` form at the start of `s`. The payload is
/// either a directive (`@KEYWORD term`) or a plain ref name.
fn parse_kiodoc_bracket(s: &str) -> Option<KiodocBracket<'_>> {
    let bracket = bracket_at(s, 0)?;
    let payload = bracket.payload.text;
    if payload.is_empty() {
        return None;
    }
    // Directive form: `@KEYWORD term` — keyword and term separated by
    // one or more whitespace characters.
    if let Some(rest) = payload.strip_prefix('@') {
        let space = rest.find(char::is_whitespace)?;
        let keyword = &rest[..space];
        let term = rest[space..].trim();
        // The keyword must be a non-empty run of letters, and the
        // term must be non-empty after trimming.
        if keyword.is_empty()
            || keyword.chars().any(|c| !c.is_ascii_alphabetic())
            || term.is_empty()
        {
            return None;
        }
        return Some(KiodocBracket {
            kind: BracketKind::Directive { keyword, term },
            source: bracket.source.text,
        });
    }
    Some(KiodocBracket {
        kind: BracketKind::Ref { name: payload },
        source: bracket.source.text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_indented_examples_keep_literal_payloads_and_ranges() {
        let source = "    [`literal`];\n\n>     [`quoted`];\n\n-     [`nested`];\n\n[`live`];";
        assert_eq!(
            rewrite_inert(source),
            source.replace("[`live`];", "`live`;")
        );
    }

    #[test]
    fn native_override_keys_use_the_same_nested_literal_boundaries() {
        let source = "- item\n  ~~~text\n  [live]: ignored\n  ~~~\n\n[`live`];\n\n> [kept]: url\n\n[`kept`];\n\n    [also_live]: ignored\n\n[`also_live`];";
        assert_eq!(
            rewrite_inert(source),
            source
                .replace("[`live`];", "`live`;")
                .replace("[`also_live`];", "`also_live`;")
        );
    }

    #[test]
    fn inert_list_code_and_deep_nesting_stay_literal() {
        let source = "- item\n  ~~~text\n  [`literal`];\n  ~~~\n  [`live`];";
        assert_eq!(
            rewrite_inert(source),
            source.replace("[`live`];", "`live`;")
        );
        let nested = format!("{}[`deep`];", "> ".repeat(256));
        assert_eq!(rewrite_inert(&nested), nested);
    }

    #[test]
    fn inert_rendered_code_boundaries_include_tildes_quotes_and_exact_closers() {
        let source = "~~~text\n[`literal`]\n[live]: ignored-in-code\n~~~\n[`live`];\n\n> ~~~text\n> [`quoted`]\n> ~~~\n> [`live`];\n\n```text\nbody\n````\n[`still_code`]";
        assert_eq!(
            rewrite_inert(source),
            source.replace("[`live`];", "`live`;")
        );
    }

    #[test]
    fn inert_quotes_keep_nested_empty_payloads_and_crlf_prefixes() {
        let source = " \t> > ~~~text\r\n \t> > [`literal`]\r\n \t> > ~~~\r\n \t> >\r\n \t> > [`live`];\r\n> \r\n> tail";
        assert_eq!(
            rewrite_inert(source),
            source.replace("[`live`];", "`live`;")
        );
    }

    #[test]
    fn inert_policy_preserves_destinations_while_rewriting_real_references() {
        let source = "before [label](docs/[`add`]); [`live`];\n[other]: docs/[`add`]\n";
        assert_eq!(
            rewrite_inert(source),
            "before [label](docs/[`add`]); `live`;\n[other]: docs/[`add`]\n"
        );
    }

    #[test]
    fn inert_policy_preserves_literals_links_overrides_and_fences() {
        let source = "é first\r\nsecond\r\n\\[`add`]; `` [`add`] ``; [`add`](url); [`add`]; [`@signature\tadd`]\r\n\r\n[add]: url\r\n```kio {@}\r\n[`missing`]\r\n```\r\n<!--text\n[`missing`]\n-->\n";
        assert_eq!(rewrite_inert(source), source);
        assert_eq!(
            rewrite_inert("```text\n[add]: url\n```\n[`add`]"),
            "```text\n[add]: url\n```\n`add`"
        );
        assert_eq!(rewrite_inert("[`add`]\r\nnext"), "`add`\r\nnext");
    }
}
