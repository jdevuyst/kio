use std::collections::HashSet;

use lsp_types::{Documentation, Hover, HoverContents, MarkupContent, MarkupKind, Position, Range};

use crate::ast::{DocComment, Item, Module, Surface};
use crate::doc_entry::DocEntry;
use crate::kiodoc::render::rewrite::rewrite;
use crate::kiodoc::render::scope::ModuleScope;
use crate::kiodoc::render::site::SymbolIndex;
use crate::lsp::positions::LineIndex;
use crate::span::Span;

pub fn doc_hover_at(source: &str, byte_offset: u32, line_index: &LineIndex) -> Option<Hover> {
    let module = parse_module(source)?;
    doc_hover_at_with_module(source, &module, byte_offset, line_index)
}

pub fn doc_hover_at_with_module(
    source: &str,
    module: &Module<Surface>,
    byte_offset: u32,
    line_index: &LineIndex,
) -> Option<Hover> {
    let (span, markdown) = declaration_doc_at(module, source, byte_offset)
        .or_else(|| same_module_elaborator_call_doc_at(module, source, byte_offset))
        .or_else(|| module_doc_at(module, source, byte_offset))?;
    Some(markdown_hover(markdown, span, line_index))
}

pub fn builtin_hover_at(source: &str, byte_offset: u32, line_index: &LineIndex) -> Option<Hover> {
    let (token, span) = token_at(source, byte_offset)?;
    let markdown = builtin_markdown(&token)?;
    Some(markdown_hover(markdown, span, line_index))
}

pub fn builtin_completion(label: &str) -> Option<(Option<String>, Documentation)> {
    let detail = builtin_detail(label);
    let documentation = builtin_markdown(label).map(markdown_documentation)?;
    Some((detail, documentation))
}

pub fn doc_comment_documentation(source: &str, doc: Option<&DocComment>) -> Option<Documentation> {
    let doc = doc?;
    let markdown = render_doc_comment(None, source, doc);
    if markdown.trim().is_empty() {
        None
    } else {
        Some(markdown_documentation(markdown))
    }
}

fn declaration_doc_at(
    module: &Module<Surface>,
    source: &str,
    byte_offset: u32,
) -> Option<(Span, String)> {
    for item in &module.items {
        match item {
            Item::FnDef(d) => {
                let span = find_named_span(source, d.meta.span, &d.name)?;
                if span_contains(span, byte_offset) {
                    return Some((span, entry_markdown(module, source, DocEntry::Fn(d))));
                }
            }
            Item::RecGroup(group, _) => {
                for member in &group.members {
                    let span = find_named_span(source, member.meta.span, &member.name)?;
                    if span_contains(span, byte_offset) {
                        return Some((span, entry_markdown(module, source, DocEntry::Fn(member))));
                    }
                }
            }
            Item::TypeRecGroup(group) => {
                if let Some(span) = group.rec_span
                    && span_contains(span, byte_offset)
                {
                    let mut markdown =
                        format!("```kio\n{}\n```", crate::pretty::pretty_item_source(item));
                    if let Some(doc) = &group.doc {
                        markdown.push_str("\n\n");
                        markdown.push_str(&render_doc_comment(Some(module), source, doc));
                    }
                    return Some((span, markdown));
                }
                for entry in crate::doc_entry::documented_entries_for_item(item) {
                    if let Some(span) = entry.type_name_span()
                        && span_contains(span, byte_offset)
                    {
                        return Some((span, entry_markdown(module, source, entry)));
                    }
                }
            }
            Item::TypeAlias(a) => {
                let span = find_named_span(source, a.meta.span, &a.name)?;
                if span_contains(span, byte_offset) {
                    return Some((
                        span,
                        entry_markdown(module, source, DocEntry::TypeAlias(a, None)),
                    ));
                }
            }
            Item::LiteralAlias(l, _) => {
                let span = find_named_span(source, l.meta.span, &l.name)?;
                if span_contains(span, byte_offset) {
                    return Some((
                        span,
                        entry_markdown(module, source, DocEntry::LiteralAlias(l)),
                    ));
                }
            }
            Item::Newtype(n) => {
                let span = find_named_span(source, n.meta.span, &n.name)?;
                if span_contains(span, byte_offset) {
                    return Some((
                        span,
                        entry_markdown(module, source, DocEntry::Newtype(n, None)),
                    ));
                }
            }
            Item::Labels(_, _) => {
                for entry in crate::doc_entry::documented_entries_for_item(item) {
                    if let Some(span) = entry.type_name_span()
                        && span_contains(span, byte_offset)
                    {
                        return Some((span, entry_markdown(module, source, entry)));
                    }
                }
            }
            Item::LabelForward(forward, _) => {
                if span_contains(forward.name_span, byte_offset) {
                    return Some((
                        forward.name_span,
                        entry_markdown(
                            module,
                            source,
                            DocEntry::LabelForward(forward, format!("{{{}}}", forward.name)),
                        ),
                    ));
                }
            }
            Item::Elaborator(e, _) => {
                if span_contains(e.name_span, byte_offset) {
                    return Some((
                        e.name_span,
                        entry_markdown(module, source, DocEntry::Elaborator(e)),
                    ));
                }
            }
            Item::Op(_, _) | Item::VariadicOperator(_, _) => {
                let span = operator_head_span(source, item.span())?;
                if span_contains(span, byte_offset) {
                    let entry = crate::doc_entry::documented_entries_for_item(item)
                        .into_iter()
                        .next()?;
                    return Some((span, entry_markdown(module, source, entry)));
                }
            }
            Item::HostType(h) => {
                let span = find_named_span(source, h.meta.span, &h.name)?;
                if span_contains(span, byte_offset) {
                    return Some((span, entry_markdown(module, source, DocEntry::HostType(h))));
                }
            }
            Item::HostFn(h) => {
                let span = find_named_span(source, h.meta.span, &h.name)?;
                if span_contains(span, byte_offset) {
                    return Some((span, entry_markdown(module, source, DocEntry::HostFn(h))));
                }
            }
            Item::Equiv(_, _) => {}
        }
    }
    None
}

fn module_doc_at(
    module: &Module<Surface>,
    source: &str,
    byte_offset: u32,
) -> Option<(Span, String)> {
    let doc = module.doc.as_ref()?;
    let first_segment = module.path.segments.first()?;
    if !span_contains(first_segment.span, byte_offset) {
        return None;
    }
    let mut markdown = format!(
        "```kio\nmodule {};\n```",
        module
            .path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("/")
    );
    let body = render_doc_comment(Some(module), source, doc);
    if !body.trim().is_empty() {
        markdown.push_str("\n\n");
        markdown.push_str(body.trim());
    }
    Some((first_segment.span, markdown))
}

fn same_module_elaborator_call_doc_at(
    module: &Module<Surface>,
    source: &str,
    byte_offset: u32,
) -> Option<(Span, String)> {
    let (token, span) = token_at(source, byte_offset)?;
    let name = token.strip_suffix('!')?;
    let elaborator = module.items.iter().find_map(|item| match item {
        Item::Elaborator(e, _) if e.name == name => Some(e),
        _ => None,
    })?;
    Some((
        span,
        entry_markdown(module, source, DocEntry::Elaborator(elaborator)),
    ))
}

fn entry_markdown(module: &Module<Surface>, source: &str, entry: DocEntry<'_>) -> String {
    let mut markdown = format!("```kio\n{}\n```", entry.signature());
    if let Some(doc) = entry.doc() {
        let body = render_doc_comment(Some(module), source, doc);
        if !body.trim().is_empty() {
            markdown.push_str("\n\n");
            markdown.push_str(body.trim());
        }
    }
    markdown
}

fn render_doc_comment(module: Option<&Module<Surface>>, source: &str, doc: &DocComment) -> String {
    let prose = doc.lines.join("\n");
    if let Some(module) = module {
        let scope = ModuleScope::from_module(module.clone());
        rewrite(
            &prose,
            "",
            ".md",
            &SymbolIndex::default(),
            &scope,
            &HashSet::new(),
        )
    } else if let Some(scope) = ModuleScope::from_source(source) {
        rewrite(
            &prose,
            "",
            ".md",
            &SymbolIndex::default(),
            &scope,
            &HashSet::new(),
        )
    } else {
        prose
    }
}

fn parse_module(source: &str) -> Option<Module<Surface>> {
    crate::pass::parser::parse_module_file(source)
        .map(|module_file| module_file.module)
        .ok()
}

fn operator_head_span(source: &str, within: Span) -> Option<Span> {
    let text = source.get(within.start as usize..within.end as usize)?;
    let body = crate::pass::lexer::lex(text)
        .ok()?
        .into_iter()
        .find(|token| matches!(token.kind, crate::pass::lexer::TokenKind::LBrace))?;
    Some(Span::new(within.start, within.start + body.span.start))
}

fn find_named_span(source: &str, within: Span, name: &str) -> Option<Span> {
    let start = within.start.min(source.len() as u32) as usize;
    let end = within.end.min(source.len() as u32) as usize;
    let haystack = source.get(start..end)?;
    let mut search_from = 0usize;
    while let Some(relative) = haystack[search_from..].find(name) {
        let span_start = start + search_from + relative;
        let span_end = span_start + name.len();
        if identifier_boundary(source, span_start, span_end) {
            return Some(Span::new(span_start as u32, span_end as u32));
        }
        search_from += relative + 1;
    }
    None
}

fn identifier_boundary(source: &str, start: usize, end: usize) -> bool {
    let before = source[..start].chars().next_back();
    let after = source[end..].chars().next();
    before.is_none_or(|c| !is_identifier_char(c)) && after.is_none_or(|c| !is_identifier_char(c))
}

fn token_at(source: &str, byte_offset: u32) -> Option<(String, Span)> {
    let mut offset = byte_offset.min(source.len() as u32) as usize;
    while offset > 0 && !source.is_char_boundary(offset) {
        offset -= 1;
    }

    let at_bang = source.get(offset..).is_some_and(|s| s.starts_with('!'));
    let at_non_identifier = !source
        .get(offset..)
        .and_then(|s| s.chars().next())
        .is_some_and(is_identifier_char);
    if offset > 0 && (at_bang || at_non_identifier) {
        offset -= 1;
    }

    if !source
        .get(offset..)
        .and_then(|s| s.chars().next())
        .is_some_and(is_identifier_char)
    {
        return None;
    }

    let mut start = offset;
    while start > 0 {
        let prev = source[..start].chars().next_back()?;
        if !is_identifier_char(prev) {
            break;
        }
        start -= prev.len_utf8();
    }

    let mut end = offset;
    while end < source.len() {
        let ch = source[end..].chars().next()?;
        if !is_identifier_char(ch) {
            break;
        }
        end += ch.len_utf8();
    }
    if source.get(end..).is_some_and(|s| s.starts_with('!')) {
        end += 1;
    }

    Some((
        source[start..end].to_owned(),
        Span::new(start as u32, end as u32),
    ))
}

fn is_identifier_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn builtin_markdown(label: &str) -> Option<String> {
    crate::builtin_docs::builtin_markdown(label)
}

fn builtin_detail(label: &str) -> Option<String> {
    crate::builtin_docs::builtin_detail(label)
}

fn markdown_hover(markdown: String, span: Span, line_index: &LineIndex) -> Hover {
    Hover {
        contents: HoverContents::Markup(markup(markdown)),
        range: Some(span_to_lsp_range(span, line_index)),
    }
}

fn markdown_documentation(markdown: String) -> Documentation {
    Documentation::MarkupContent(markup(markdown))
}

fn markup(value: String) -> MarkupContent {
    MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    }
}

fn span_to_lsp_range(span: Span, line_index: &LineIndex) -> Range {
    let lsp_range = line_index.to_range(span);
    Range {
        start: Position {
            line: lsp_range.start.line,
            character: lsp_range.start.character,
        },
        end: Position {
            line: lsp_range.end.line,
            character: lsp_range.end.character,
        },
    }
}

fn span_contains(span: Span, byte_offset: u32) -> bool {
    span.start <= byte_offset && byte_offset <= span.end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_label_nominal_hover_uses_its_labels_owner() {
        let source = "module pkg;\n/// The field label.\nlabels { field: . };\n";
        let offset = source.find("field:").unwrap() as u32;
        let hover = doc_hover_at(source, offset, &LineIndex::new(source)).unwrap();
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(
            markup.value.contains("labels { field: . };"),
            "{}",
            markup.value
        );
        assert!(markup.value.contains("The field label."));
        assert!(!markup.value.contains("newtype Field"));
    }

    #[test]
    fn forwarded_label_hover_uses_its_own_declaration_and_doc() {
        let source = concat!(
            "module pkg;\nfn field() -> . { () }\n",
            "/// The forwarded field.\n",
            "pub type {field} = {original};\n",
        );
        let offset = source.find("{field}").unwrap() as u32 + 1;
        let index = LineIndex::new(source);
        let hover = doc_hover_at(source, offset, &index).unwrap();
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(markup.value.contains("pub type {field} = {original};"));
        assert!(markup.value.contains("The forwarded field."));
        assert!(!markup.value.contains("newtype Field"));
        assert_eq!(
            hover.range,
            Some(span_to_lsp_range(Span::new(offset, offset + 5), &index))
        );
    }

    #[test]
    fn doc_hover_renders_elaborator_doc_comment() {
        let source = "module pkg/elabs;\n/// See [`@signature id`].\npub elab id : [A] A -> A { impl id_impl }\nfn id_impl() -> . { () }\n";
        let offset = source.find("id :").unwrap() as u32;
        let line_index = LineIndex::new(source);
        let hover = doc_hover_at(source, offset, &line_index).unwrap();
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(markup.value.contains("elab id :"));
        assert!(!markup.value.contains("@signature"));
    }

    #[test]
    fn doc_hover_renders_literal_doc_comment() {
        let source = "module pkg/main;\n/// Default greeting.\nliteral greeting = \"hi\";\n";
        let offset = source.find("greeting =").unwrap() as u32;
        let line_index = LineIndex::new(source);
        let hover = doc_hover_at(source, offset, &line_index).unwrap();
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(markup.value.contains("literal greeting"));
        assert!(markup.value.contains("Default greeting."));
    }

    #[test]
    fn doc_hover_uses_supplied_module_with_imported_operator() {
        let source = "module pkg/main;\nimport pkg/syntax(op ? _ : _);\n/// Chooses a value with [`@source exercise`].\npub fn choose_value(value: .) -> . { value }\nfn exercise(condition: ., yes: .) -> . { ? condition : yes }\n";
        let module = crate::pass::parser::parse_module_file(source)
            .unwrap()
            .module;
        let offset = source.find("choose_value(").unwrap() as u32;
        let line_index = LineIndex::new(source);
        let hover = doc_hover_at_with_module(source, &module, offset, &line_index).unwrap();
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(markup.value.contains("fn choose_value"));
        assert!(markup.value.contains("Chooses a value with"));
        assert!(markup.value.contains("? condition : yes"));
    }
}
