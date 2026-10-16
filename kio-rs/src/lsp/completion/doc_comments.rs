use super::*;
use crate::ast::{ImportKind, KioFileKind};
use crate::doc_entry::DocEntry;
use crate::doc_markup::editor::{DocCursor, completion_cursor};
use crate::doc_markup::generated::DirectiveKeyword;
use crate::doc_markup::source::DocSource;
use crate::kiodoc::refs;
use crate::pass::parser::ToolingProbe;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

pub(super) fn complete(
    uri: &Uri,
    source: &str,
    offset: u32,
    kind: KioFileKind,
    probe: &ToolingProbe<'_>,
    sources: &CompletionSources<'_>,
) -> CompletionResult {
    let mut result = CompletionList {
        is_incomplete: false,
        items: Vec::new(),
    };
    let lexed = crate::pass::lexer::lex_prefix(source);
    let docs = DocSource::from_lexed(source, &lexed);
    let Some((doc, cursor)) = docs.iter().find_map(|doc| {
        let cursor = doc.source_to_logical(offset as usize)?;
        Some((doc, cursor))
    }) else {
        return CompletionResult::syntax(result);
    };
    let Some(context) = completion_cursor(doc, cursor) else {
        return CompletionResult::syntax(result);
    };
    let module = if kind == KioFileKind::Module {
        probe
            .parse_error
            .as_ref()
            .and_then(|_| crate::pass::parser::parse_module_file_lazy(source).ok())
            .map(|parsed| std::borrow::Cow::Owned(parsed.module))
            .or_else(|| crate::scope_walk::tooling_module(probe))
    } else {
        None
    };
    if let Some(module) = &module
        && !module_doc_context(module, probe, offset)
    {
        return CompletionResult::syntax(result);
    }
    let replacement = match context {
        DocCursor::Keyword { replacement } => {
            result
                .items
                .extend(DirectiveKeyword::ALL.iter().map(|keyword| CompletionItem {
                    label: keyword.as_str().to_owned(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    ..Default::default()
                }));
            replacement
        }
        DocCursor::Operand {
            replacement,
            directive,
        } => {
            if kind != KioFileKind::Module
                || refs::collect_ref_overrides(&doc.text).contains(&doc.text[replacement.clone()])
            {
                return CompletionResult::syntax(result);
            }
            let Some(module) = &module else {
                result.is_incomplete = true;
                return CompletionResult::syntax(result);
            };
            if let crate::lsp::util::FileUriPath::Path(path) = crate::lsp::util::file_uri_path(uri)
                && crate::package_collection::ModuleFileParserContext::from_module(&path, module)
                    .is_err()
            {
                return CompletionResult::syntax(result);
            }
            result = names(
                module,
                &doc.text[replacement.start..cursor.clamp(replacement.start, replacement.end)],
                directive,
                sources,
            );
            result.is_incomplete |= probe.parse_error.is_some() || probe.lexical_error.is_some();
            replacement
        }
        DocCursor::Fence { body, info } => {
            if kind != KioFileKind::Module || !self_harness(info) {
                return CompletionResult::syntax(result);
            }
            return embedded(uri, source, doc, cursor, body, sources);
        }
    };
    let prefix = &doc.text[replacement.start..cursor.clamp(replacement.start, replacement.end)];
    result.items.retain(|item| item.label.starts_with(prefix));
    result.items.retain(|item| {
        let written = format!("[`{}`]", item.label);
        crate::doc_markup::lexical::bracket_at(&written, 0)
            .is_some_and(|bracket| bracket.payload.text == item.label)
    });
    let index = LineIndex::new(source);
    result.items.retain_mut(|item| {
        let Some(edit) = doc.plain_edit(replacement.clone(), &item.label) else {
            return false;
        };
        item.text_edit = Some(CompletionTextEdit::Edit(text_edit(
            &index,
            edit.span,
            edit.replacement,
        )));
        true
    });
    result.items.sort_by(|a, b| a.label.cmp(&b.label));
    result.items.dedup_by(|a, b| a.label == b.label);
    CompletionResult::syntax(result)
}

struct Candidate<'a> {
    item: CompletionItem,
    entry: DocEntry<'a>,
    visibility: crate::ast::Visibility,
    value: bool,
}

fn module_doc_context(module: &Module<Surface>, probe: &ToolingProbe<'_>, offset: u32) -> bool {
    let contains = |span: crate::span::Span| span.start <= offset && offset <= span.end;
    if module.doc.as_ref().is_some_and(|doc| contains(doc.span))
        || crate::doc_entry::documented_items_module(module)
            .iter()
            .any(|entry| entry.doc().is_some_and(|doc| contains(doc.span)))
    {
        return true;
    }
    !module.items.iter().any(|item| contains(item.span()))
        && !probe
            .facts
            .regions
            .iter()
            .any(|region| contains(region.interior))
}

fn declarations(module: &Module<Surface>) -> Vec<Candidate<'_>> {
    declarations_matching(module, |_| true)
}

fn declarations_matching(
    module: &Module<Surface>,
    keep: impl Fn(&Candidate<'_>) -> bool,
) -> Vec<Candidate<'_>> {
    let scope = refs::module_scope_from_surface(module);
    let names: HashSet<_> = scope.top_level.into_iter().collect();
    let mut out = BTreeMap::new();
    for entry in crate::doc_entry::documented_items_module(module) {
        let kind = match &entry {
            DocEntry::Fn(_)
            | DocEntry::RecFn(_)
            | DocEntry::HostFn(_)
            | DocEntry::Elaborator(_) => CompletionItemKind::FUNCTION,
            DocEntry::LiteralAlias(_) => CompletionItemKind::CONSTANT,
            DocEntry::Op(..) | DocEntry::VariadicOperator(..) => CompletionItemKind::OPERATOR,
            DocEntry::LabelForward(..) => CompletionItemKind::FIELD,
            DocEntry::TypeAlias(..)
            | DocEntry::Newtype(..)
            | DocEntry::HostType(_)
            | DocEntry::Labels(..)
            | DocEntry::LabelNominal { .. } => CompletionItemKind::STRUCT,
        };
        let documentation = entry.doc().map(|doc| {
            Documentation::MarkupContent(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: crate::doc_markup::rewrite::rewrite_inert(&doc.lines.join("\n")),
            })
        });
        let mut add = |name: String, kind, value, visibility: &crate::ast::Visibility, detail| {
            if !names.contains(&name) {
                return;
            }
            let candidate = Candidate {
                item: CompletionItem {
                    label: name.clone(),
                    kind: Some(kind),
                    detail,
                    documentation: documentation.clone(),
                    ..Default::default()
                },
                entry: entry.clone(),
                visibility: visibility.clone(),
                value,
            };
            if !keep(&candidate) {
                return;
            }
            out.entry(name)
                .and_modify(|existing| *existing = None)
                .or_insert(Some(candidate));
        };
        add(
            entry.name().to_owned(),
            kind,
            entry.ty().is_some(),
            entry.visibility(),
            Some(entry.signature()),
        );
        match &entry {
            DocEntry::Op(op, _) => {
                if let Some(name) = refs::operator_token_name(op) {
                    add(
                        name,
                        kind,
                        false,
                        entry.visibility(),
                        Some(entry.signature()),
                    );
                }
            }
            DocEntry::Newtype(newtype, _) => {
                for member in [&newtype.constructor, &newtype.projector] {
                    add(
                        member.name.clone(),
                        CompletionItemKind::FUNCTION,
                        true,
                        &member.vis,
                        None,
                    );
                }
            }
            DocEntry::LabelNominal {
                labels,
                entry: label,
                ..
            } => {
                add(
                    label.name.clone(),
                    CompletionItemKind::FIELD,
                    false,
                    &labels.vis,
                    None,
                );
            }
            DocEntry::Labels(labels, _, _) => {
                for label in labels.entries.iter().filter(|label| {
                    !label.is_reuse_marker()
                        && labels.type_alias_name.as_ref()
                            == Some(&crate::ast::mint_label_newtype_name(&label.name))
                }) {
                    add(
                        label.name.clone(),
                        CompletionItemKind::FIELD,
                        false,
                        &labels.vis,
                        None,
                    );
                }
            }
            _ => {}
        }
    }
    out.into_values().flatten().collect()
}

fn names(
    module: &Module<Surface>,
    written: &str,
    directive: Option<DirectiveKeyword>,
    sources: &CompletionSources<'_>,
) -> CompletionList {
    let mut result = CompletionList {
        is_incomplete: false,
        items: Vec::new(),
    };
    let value_only = directive == Some(DirectiveKeyword::Type);
    let mut providers = BTreeMap::new();
    let current = module.path.segments.join("/");
    let mut selected = BTreeMap::<String, CompletionItem>::new();
    let mut local_labels = HashSet::new();
    let mut imported_identities = BTreeMap::new();
    let mut ambiguous_labels = HashSet::new();
    let mut add = |candidate: Candidate<'_>, prefix: &str, owner: Option<&str>| {
        if !value_only || candidate.value {
            let declaration = candidate.entry.name().to_owned();
            let mut item = candidate.item;
            item.label = format!("{prefix}{}", item.label);
            if let Some(owner) = owner {
                if local_labels.contains(&item.label) {
                    return;
                }
                let identity = (owner.to_owned(), declaration);
                if let Some(previous) = imported_identities.get(&item.label)
                    && previous != &identity
                {
                    selected.remove(&item.label);
                    ambiguous_labels.insert(item.label);
                    return;
                }
                imported_identities.insert(item.label.clone(), identity);
                if ambiguous_labels.contains(&item.label) {
                    return;
                }
            } else {
                local_labels.insert(item.label.clone());
            }
            selected.entry(item.label.clone()).or_insert(item);
        }
    };
    let qualified = written.rsplit_once('.');
    if qualified.is_none() {
        for candidate in declarations(module) {
            add(candidate, "", None);
        }
    }
    for import in &module.imports {
        let (owner, alias) = match &import.kind {
            ImportKind::Selective { from, .. } => (from.segments.join("/"), None),
            ImportKind::Qualified { path, alias } => {
                (path.segments.join("/"), Some(alias.as_str()))
            }
            ImportKind::Intrinsics | ImportKind::Comptime => continue,
        };
        let prefix = qualified.and_then(|(prefix, _)| {
            (prefix == owner || Some(prefix) == alias).then(|| format!("{prefix}."))
        });
        if let Some(alias) = alias
            && qualified.is_some_and(|(head, _)| head == alias)
            && module
                .imports
                .iter()
                .filter(|import| {
                    matches!(&import.kind,
                ImportKind::Qualified { alias: other, .. } if other == alias)
                })
                .count()
                != 1
        {
            result.is_incomplete = true;
            continue;
        }
        if qualified.is_some() && prefix.is_none() {
            continue;
        }
        if alias.is_some() && prefix.is_none() {
            continue;
        }
        let provider = providers
            .entry(owner.clone())
            .or_insert_with(|| sources.provider_syntax(&owner));
        let Some(provider) = provider else {
            result.is_incomplete = true;
            continue;
        };
        let candidates = declarations_matching(provider, |candidate| {
            crate::pass::resolve::is_visible(candidate.entry.visibility(), &module.path)
                && crate::pass::resolve::is_visible(&candidate.visibility, &module.path)
                && (prefix.is_some()
                    || matches!(&import.kind, ImportKind::Selective { items, .. }
                    if items.iter().any(|item| imported_candidate(item, candidate))))
        });
        for candidate in candidates {
            if let Some(prefix) = &prefix {
                add(candidate, prefix, Some(&owner));
            } else if let ImportKind::Selective { items, .. } = &import.kind {
                let imported = items
                    .iter()
                    .any(|item| imported_candidate(item, &candidate));
                if imported {
                    if candidate.item.kind == Some(CompletionItemKind::FIELD)
                        && items.iter().any(|item| matches!(item,
                            crate::ast::ImportItem::Label { name, .. } if name == &candidate.item.label))
                    {
                        let mut braced = Candidate {
                            item: candidate.item.clone(), entry: candidate.entry.clone(),
                            visibility: candidate.visibility.clone(), value: false,
                        };
                        braced.item.label = format!("{{{}}}", braced.item.label);
                        add(braced, "", Some(&owner));
                    }
                    add(candidate, "", Some(&owner));
                }
            }
        }
    }
    if let Some((head, _)) = qualified {
        if head == current {
            for candidate in declarations(module) {
                add(candidate, &format!("{head}."), None);
            }
        } else {
            let mut head_parts = head
                .split('.')
                .map(|part| crate::ast::PathSegment {
                    name: part.to_owned(),
                    span: crate::span::Span::new(0, 0),
                })
                .collect::<Vec<_>>();
            if let [provider, ..] = head_parts.as_mut_slice()
                && let Some(alias) = module.imports.iter().find_map(|import| match &import.kind {
                    ImportKind::Qualified { path, alias }
                        if path.segments.join("/") == provider.name =>
                    {
                        Some(alias)
                    }
                    _ => None,
                })
            {
                provider.name.clone_from(alias);
            }
            let (loaded, incomplete) = crate::scope_walk::qualified_providers(
                module,
                &head_parts,
                crate::pass::parser::CursorSlot::Value,
                |name| sources.provider_syntax(name).map(|module| ((), module)),
            );
            result.is_incomplete |= incomplete;
            let modules = loaded
                .values()
                .map(|(_, module)| module)
                .collect::<Vec<_>>();
            for candidate in crate::scope_walk::qualified_candidates(
                module,
                &head_parts,
                crate::pass::parser::CursorSlot::Value,
                &modules,
            ) {
                // Module-level candidates use the Kiodoc declaration inventory above.
                if head_parts.len() == 1
                    && module.imports.iter().any(|import| {
                        matches!(
                            &import.kind, ImportKind::Qualified { alias, .. } if alias == head
                        )
                    })
                {
                    continue;
                }
                let label = format!("{head}.{}", candidate.label);
                selected.entry(label.clone()).or_insert(CompletionItem {
                    label,
                    kind: Some(to_lsp_kind(candidate.kind)),
                    detail: candidate.detail,
                    ..Default::default()
                });
            }
        }
    } else {
        if directive.is_none() {
            for item in &module.items {
                if let Item::Equiv(equiv, _) = item {
                    selected
                        .entry(equiv.name.clone())
                        .or_insert(CompletionItem {
                            label: equiv.name.clone(),
                            kind: Some(CompletionItemKind::REFERENCE),
                            ..Default::default()
                        });
                }
            }
        }
        for entry in crate::scope_walk::in_scope_candidates(module, u32::MAX)
            .into_iter()
            .chain(crate::scope_walk::type_candidates(module, u32::MAX))
        {
            if let CandidateOrigin::Builtin(_) = entry.origin {
                if value_only && entry.kind == EntryKind::Type {
                    continue;
                }
                let item = entry_to_lsp(entry, None, None, None);
                selected.entry(item.label.clone()).or_insert(item);
            } else if directive.is_none()
                && matches!(entry.origin, CandidateOrigin::QualifiedImport(_))
            {
                selected
                    .entry(entry.label.clone())
                    .or_insert(CompletionItem {
                        label: entry.label,
                        kind: Some(CompletionItemKind::MODULE),
                        ..Default::default()
                    });
            }
        }
    }
    result.is_incomplete |= !ambiguous_labels.is_empty();
    result.items = selected
        .into_values()
        .filter(|item| !ambiguous_labels.contains(&item.label))
        .collect();
    result
}

fn imported_candidate(item: &crate::ast::ImportItem, candidate: &Candidate<'_>) -> bool {
    match item {
        crate::ast::ImportItem::Name { name, .. } => name == &candidate.item.label,
        crate::ast::ImportItem::Label { name, .. } => {
            candidate.item.kind == Some(CompletionItemKind::FIELD)
                && (name == &candidate.item.label || format!("{{{name}}}") == candidate.item.label)
        }
        crate::ast::ImportItem::OperatorPattern { grammar, .. } => {
            grammar.render() == candidate.entry.name()
        }
    }
}

fn self_harness(info: &str) -> bool {
    let Some(attrs) = info
        .trim()
        .strip_prefix("kio")
        .filter(|suffix| suffix.starts_with(char::is_whitespace))
        .and_then(|suffix| crate::kiodoc::attrs::parse(suffix.trim()).ok())
    else {
        return false;
    };
    attrs.self_ref
        && attrs.harness_ref.is_none()
        && attrs.bare.is_empty()
        && attrs
            .kvs
            .iter()
            .all(|kv| kv.key == "check_exit_code" && kv.value.parse::<i32>().is_ok())
}

fn embedded(
    uri: &Uri,
    source: &str,
    doc: &DocSource<'_>,
    cursor: usize,
    body: Range<usize>,
    sources: &CompletionSources<'_>,
) -> CompletionResult {
    let mut result = CompletionList {
        is_incomplete: false,
        items: Vec::new(),
    };
    let Some(module) = crate::pass::parser::parse_module_file_lazy(source)
        .ok()
        .map(|parsed| parsed.module)
    else {
        result.is_incomplete = true;
        return CompletionResult::syntax(result);
    };
    let names: HashSet<_> = refs::module_scope_from_surface(&module)
        .top_level
        .into_iter()
        .collect();
    let mut wrapper = "kiodoc_completion".to_owned();
    while names.contains(&wrapper) {
        wrapper.push('_');
    }
    let prefix = format!("{source}\nfn {wrapper}() -> . {{\n");
    let synthetic = format!("{prefix}{}\n}}\n", &doc.text[body.clone()]);
    let index = LineIndex::new(&synthetic);
    let position = index.to_position((prefix.len() + cursor - body.start) as u32);
    if let Some(completed) = complete_source(
        uri,
        &Position::new(position.line, position.character),
        &synthetic,
        CompletionSources {
            local: None,
            catalog: sources.catalog,
            declarations: sources.declarations,
            state: sources.state,
        },
    ) {
        result.is_incomplete = completed.list.is_incomplete;
        result.items = completed
            .list
            .items
            .into_iter()
            .filter(|item| item.label != wrapper)
            .filter_map(|item| {
                map_embedded_item(item, &synthetic, source, doc, prefix.len(), body.clone())
            })
            .collect();
    }
    CompletionResult::syntax(result)
}

fn map_embedded_item(
    mut item: CompletionItem,
    synthetic: &str,
    source: &str,
    doc: &DocSource<'_>,
    start: usize,
    body: Range<usize>,
) -> Option<CompletionItem> {
    if item
        .additional_text_edits
        .as_ref()
        .is_some_and(|edits| !edits.is_empty())
        || item.command.is_some()
    {
        return None;
    }
    let Some(CompletionTextEdit::Edit(edit)) = item.text_edit.take() else {
        return None;
    };
    let index = LineIndex::new(synthetic);
    let offset = |position: Position| {
        let position = LspPosition {
            line: position.line,
            character: position.character,
        };
        let offset = index.position_to_offset(position);
        (index.to_position(offset) == position).then_some(offset as usize)
    };
    let lo = offset(edit.range.start)?;
    let hi = offset(edit.range.end)?;
    if lo < start || hi < lo || hi > start + body.len() {
        return None;
    }
    // Placeholder expansion can insert arbitrary newlines after the edit is
    // applied. Plain insertion keeps every byte under authenticated framing.
    let replacement = if item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET) {
        item.label.clone()
    } else {
        edit.new_text
    };
    let edit = doc.plain_edit(
        body.start + lo - start..body.start + hi - start,
        &replacement,
    )?;
    item.insert_text = None;
    item.insert_text_format = None;
    item.insert_text_mode = Some(lsp_types::InsertTextMode::AS_IS);
    item.text_edit = Some(CompletionTextEdit::Edit(text_edit(
        &LineIndex::new(source),
        edit.span,
        edit.replacement,
    )));
    Some(item)
}

fn text_edit(index: &LineIndex, span: crate::span::Span, new_text: String) -> TextEdit {
    let range = index.to_range(span);
    TextEdit {
        range: lsp_types::Range::new(
            Position::new(range.start.line, range.start.character),
            Position::new(range.end.line, range.end.character),
        ),
        new_text,
    }
}

#[cfg(test)]
mod tests;
