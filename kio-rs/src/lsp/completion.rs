//! LSP `textDocument/completion` handler.
//!
//! Given a (URI, position) pair from the client, returns the set of
//! identifiers in lexical scope or full operator grammars from the explicitly
//! selected import provider in each declaration's selection namespace.  Each
//! `CompletionItem` carries:
//!
//! - `label` — the identifier or full tagged operator grammar.
//! - `kind` — `Function`, `Variable`, `Module`, or type-shaped item
//!   mapped from the binder class.
//! - `detail` — the binder's synthesized type or builtin signature,
//!   when available.
//! - `documentation` — rendered Kiodoc or builtin Markdown when
//!   available.
//!
//! ## Scope walk
//!
//! The position index records binders at *use* sites.  For completion
//! we need the inverse — "what names are in scope here?" — which the
//! position index alone does not answer. The ordinary parser supplies the
//! cursor and scope facts, including retained headers for incomplete input.
//! Shared [`crate::scope_walk`] projects them innermost-first, using the same
//! declaration identities and shadowing rules as REPL completion.
//!
//! ## Scope
//!
//! - Regular-module completion covers identifiers reachable from the
//!   cursor's lexical scope and imports.
//! - Package-file completion covers declarations valid in the current
//!   package-file context.
//! - Type-position completion combines syntactic context with the
//!   latest lexical scope.
//! - Import-list completion uses the selected provider's current declaration
//!   snapshot. Missing semantic data yields an incomplete list, while consumer
//!   syntax remains independent of provider state.
//! - Complete lists let the editor narrow the reachable set as the user types.

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionTextEdit, Documentation,
    Position, TextEdit, Uri,
};

use crate::ast::{Item, Module, Surface};
use crate::cmd::check::LspAnalysis;
use crate::lsp::docs::{builtin_completion, doc_comment_documentation};
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::lsp::util::filename_from_uri;
use crate::pass::typecheck_core::write_type;
use crate::scope_walk::{
    Candidate as CompletionEntry, CandidateKind as EntryKind, CandidateOrigin,
};

mod block_calls;
mod call_types;
pub(crate) use block_calls::handle_signature_request as handle_block_signature_request;
#[cfg(test)]
mod context_tests;

/// Handle completion against explicitly supplied current source and analysis.
#[cfg(test)]
pub fn handle_completion(
    uri: &Uri,
    position: &Position,
    overlay_text: Option<&str>,
    analysis: Option<&LspAnalysis>,
) -> Option<CompletionList> {
    let source = overlay_text.or_else(|| {
        let path = crate::lsp::util::uri_to_canonical(uri)?;
        analysis?.sources.get(&path).map(String::as_str)
    })?;
    complete_source(
        uri,
        position,
        source,
        CompletionSources {
            local: analysis,
            catalog: analysis,
            state: None,
        },
    )
    .map(|response| response.list)
}

pub(crate) struct CompletionResult {
    pub(crate) list: CompletionList,
    pub(crate) needs_local_metadata: bool,
}

impl CompletionResult {
    fn syntax(list: CompletionList) -> Self {
        Self {
            list,
            needs_local_metadata: false,
        }
    }
}

/// One request authenticates local typed facts separately from the current
/// declaration snapshots of explicitly selected providers.
pub(crate) fn handle_completion_request(
    uri: &Uri,
    position: &Position,
    workspace_root: Option<&std::path::Path>,
    state: &crate::lsp::state::ServerState,
    local: Option<&LspAnalysis>,
) -> Option<CompletionResult> {
    if matches!(
        crate::lsp::util::file_uri_path(uri),
        crate::lsp::util::FileUriPath::InvalidFile
    ) {
        return None;
    }
    let source = if let Some(document) = state.document(uri) {
        document.text().to_owned()
    } else {
        match crate::lsp::util::file_uri_path(uri) {
            crate::lsp::util::FileUriPath::Path(path) => std::fs::read_to_string(path).ok()?,
            crate::lsp::util::FileUriPath::InvalidFile => return None,
            crate::lsp::util::FileUriPath::NonFile => return None,
        }
    };
    let path = crate::lsp::util::uri_to_canonical(uri);
    let catalog = path
        .as_deref()
        .and_then(|path| state.analysis_for_file(path))
        .or_else(|| workspace_root.and_then(|root| state.analysis_for_package_root(root)));
    let mut result = complete_source(
        uri,
        position,
        &source,
        CompletionSources {
            local,
            catalog,
            state: Some(state),
        },
    )?;
    if !state.completion_snippets {
        for item in &mut result.list.items {
            if item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET) {
                item.insert_text = None;
                item.insert_text_format = None;
                if let Some(CompletionTextEdit::Edit(edit)) = &mut item.text_edit {
                    edit.new_text.clone_from(&item.label);
                }
            }
        }
    }
    Some(result)
}

struct CompletionSources<'a> {
    local: Option<&'a LspAnalysis>,
    catalog: Option<&'a LspAnalysis>,
    state: Option<&'a crate::lsp::state::ServerState>,
}

#[cfg(test)]
thread_local! {
    static COMPLETION_PROVIDER_READS: std::cell::RefCell<Vec<String>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

impl<'a> CompletionSources<'a> {
    fn provider(&self, name: &str) -> Option<(&'a str, Module<Surface>)> {
        #[cfg(test)]
        COMPLETION_PROVIDER_READS.with(|reads| reads.borrow_mut().push(name.to_owned()));
        let catalog = self.catalog?;
        let mut paths = catalog
            .file_to_module
            .iter()
            .filter(|(_, module)| module.as_str() == name);
        let (path, _) = paths.next()?;
        if paths.next().is_some() {
            return None;
        }
        let source = self.current_provider_source(path, name)?;
        let parsed = crate::pass::parser::parse_module_file_lazy(source).ok()?;
        if parsed.module.path.segments.join("/") != name {
            return None;
        }
        crate::package_collection::ModuleFileParserContext::from_module(path, &parsed.module)
            .ok()?;
        Some((source, parsed.module))
    }

    fn current_provider_source(&self, path: &std::path::Path, name: &str) -> Option<&'a str> {
        let catalog = self.catalog?;
        let analysis = if let Some(state) = self.state {
            let uri = crate::lsp::diagnostics::path_to_uri(path, path.parent()?)?;
            let analysis = state.best_analysis_for_file(path, &uri)?;
            let source = analysis.sources.get(path)?;
            if let Some(document) = state.document(&uri) {
                if document.text() != source {
                    return None;
                }
            } else if std::fs::read_to_string(path).ok().as_deref() != Some(source) {
                return None;
            }
            analysis
        } else {
            catalog
        };
        if analysis.file_to_module.get(path).map(String::as_str) != Some(name) {
            return None;
        }
        analysis.sources.get(path).map(String::as_str)
    }

    fn provider_names(&self) -> Option<(Vec<&'a str>, bool)> {
        let catalog = self.catalog?;
        let mut counts = std::collections::HashMap::new();
        for name in catalog.file_to_module.values() {
            *counts.entry(name.as_str()).or_insert(0usize) += 1;
        }
        let mut names = Vec::new();
        let mut incomplete = false;
        for (path, name) in &catalog.file_to_module {
            if counts[name.as_str()] == 1 && self.current_provider_source(path, name).is_some() {
                names.push(name.as_str());
            } else {
                incomplete = true;
            }
        }
        Some((names, incomplete))
    }
}

fn complete_source(
    uri: &Uri,
    position: &Position,
    source: &str,
    sources: CompletionSources<'_>,
) -> Option<CompletionResult> {
    use crate::ast::KioFileKind;
    use crate::pass::parser::probe_tooling;
    let byte_offset = LineIndex::new(source).position_to_offset(LspPosition {
        line: position.line,
        character: position.character,
    });
    let filename = filename_from_uri(uri);
    let kind = if crate::file_kind::is_package_file(&filename) {
        KioFileKind::Package
    } else if crate::file_kind::is_dep_file(&filename) {
        KioFileKind::Dependency
    } else if crate::file_kind::is_lock_file(&filename) {
        KioFileKind::Lock
    } else if crate::file_kind::is_sig_file(&filename) {
        KioFileKind::Signature
    } else {
        KioFileKind::Module
    };
    let mut probe = probe_tooling(source, Some(kind), Some(byte_offset), None);
    let argument_head = probe
        .facts
        .cursor
        .as_ref()
        .filter(|cursor| {
            probe.facts.suppression.is_none()
                && cursor.slot == crate::pass::parser::CursorSlot::Argument
        })
        .map(|cursor| {
            cursor
                .call
                .as_ref()
                .filter(|call| !call.has_prior_argument)
                .map(|call| call_types::current_call_head(uri, source, &call.callee, sources.local))
                .unwrap_or(crate::scope_walk::CallHead::Unknown)
        });
    if argument_head == Some(crate::scope_walk::CallHead::Value)
        && let Some(cursor) = &mut probe.facts.cursor
    {
        cursor.slot = crate::pass::parser::CursorSlot::Value;
    }
    let mut result = CompletionList {
        is_incomplete: argument_head == Some(crate::scope_walk::CallHead::Unknown)
            || probe.facts.cursor.is_none()
                && probe.facts.suppression.is_none()
                && (probe.lexical_error.is_some() || probe.parse_error.is_some()),
        items: Vec::new(),
    };
    if probe.facts.suppression.is_some() {
        return Some(CompletionResult::syntax(result));
    }
    if let Some(path) = &probe.facts.module_path
        && let crate::lsp::util::FileUriPath::Path(source_path) =
            crate::lsp::util::file_uri_path(uri)
        && crate::package_collection::ModuleFileParserContext::from_module_path(&source_path, path)
            .is_err()
    {
        return None;
    }
    let Some(cursor) = &probe.facts.cursor else {
        return Some(CompletionResult::syntax(result));
    };
    if matches!(
        cursor.slot,
        crate::pass::parser::CursorSlot::ImportProvider
            | crate::pass::parser::CursorSlot::ModulePath
    ) {
        let prefix = cursor
            .path
            .as_ref()
            .map(|path| {
                path.prefix
                    .iter()
                    .map(|part| part.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some((names, incomplete)) = sources.provider_names() {
            result.is_incomplete |= incomplete;
            for name in names {
                let parts: Vec<_> = name.split('/').collect();
                if parts.len() > prefix.len() && parts[..prefix.len()] == prefix {
                    let label = parts[prefix.len()..].join("/");
                    result.items.push(CompletionItem {
                        label,
                        kind: Some(CompletionItemKind::MODULE),
                        ..Default::default()
                    });
                }
            }
        } else {
            result.is_incomplete = true;
        }
        if cursor.slot == crate::pass::parser::CursorSlot::ImportProvider && prefix.is_empty() {
            for module in [
                crate::builtin_docs::BuiltinModule::Intrinsics,
                crate::builtin_docs::BuiltinModule::Comptime,
            ] {
                result.items.push(CompletionItem {
                    label: module.name().to_owned(),
                    kind: Some(CompletionItemKind::MODULE),
                    ..Default::default()
                });
            }
        }
    }
    if cursor.import.is_some()
        && let Some(importer) = &probe.facts.module_path
    {
        return Some(CompletionResult::syntax(import_list_candidates(
            source, cursor, importer, &sources,
        )));
    }
    let scope_module = crate::scope_walk::tooling_module(&probe);
    let module = scope_module.as_deref();
    if cursor.slot == crate::pass::parser::CursorSlot::BlockLabel {
        return Some(CompletionResult::syntax(block_calls::complete_labels(
            &probe, module, &sources,
        )));
    }
    if let Some(path) = &cursor.path
        && let Some(module) = module
        && crate::scope_walk::tooling_path_allowed(&probe, None)
    {
        let (providers, incomplete) =
            crate::scope_walk::qualified_providers(module, &path.prefix, cursor.slot, |name| {
                sources.provider(name)
            });
        result.is_incomplete |= incomplete;
        let modules: Vec<_> = providers.values().map(|(_, module)| module).collect();
        result.items.extend(
            crate::scope_walk::qualified_candidates(module, &path.prefix, cursor.slot, &modules)
                .into_iter()
                .map(|candidate| CompletionItem {
                    label: candidate.label,
                    kind: Some(to_lsp_kind(candidate.kind)),
                    detail: candidate.detail,
                    documentation: providers
                        .get(&candidate.owner)
                        .map(|(source, _)| *source)
                        .or_else(|| {
                            (candidate.owner == module.path.segments.join("/")).then_some(source)
                        })
                        .and_then(|source| {
                            doc_comment_documentation(source, candidate.doc.as_ref())
                        }),
                    ..Default::default()
                }),
        );
    }
    let entries = crate::scope_walk::tooling_candidates(&probe);
    let needs_local_metadata = argument_head == Some(crate::scope_walk::CallHead::Unknown)
        || entries.iter().any(|entry| {
            matches!(
                entry.origin,
                CandidateOrigin::Local(_) | CandidateOrigin::Declaration(_)
            )
        });
    let mut metadata = CompletionMetadata::empty();
    if let Some(path) = &probe.facts.module_path {
        if let Some(local) = sources.local
            && let Some(typed) = CompletionMetadata::new(uri, source, path, local)
        {
            metadata = typed;
        }
        metadata.add_imports(path, &entries, &sources);
    }
    result.items.extend(entries.into_iter().filter_map(|entry| {
        if let CandidateOrigin::Import { provider, .. } = &entry.origin
            && let Some(items) = metadata.imports.get(provider)
        {
            return items.get(&entry.label).and_then(Option::as_ref).cloned();
        }
        Some(entry_to_lsp(entry, Some(source), module, Some(&metadata)))
    }));
    result.items.extend(
        crate::scope_walk::tooling_keywords(&probe)
            .into_iter()
            .map(|keyword| CompletionItem {
                label: keyword.to_owned(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            }),
    );
    if let Some(module) = module {
        let grammars = crate::scope_walk::expression_operators(module, byte_offset);
        result.items.extend(
            crate::scope_walk::tooling_operators(&probe, &grammars)
                .into_iter()
                .map(|(label, grammar)| CompletionItem {
                    label,
                    kind: Some(CompletionItemKind::OPERATOR),
                    detail: Some(grammar),
                    ..Default::default()
                }),
        );
    }
    let range = LineIndex::new(probe.source).to_range(cursor.atom.replacement);
    for item in &mut result.items {
        item.text_edit = Some(CompletionTextEdit::Edit(TextEdit {
            range: lsp_types::Range::new(
                Position::new(range.start.line, range.start.character),
                Position::new(range.end.line, range.end.character),
            ),
            new_text: item
                .insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone()),
        }));
    }
    result.items.sort_by(|a, b| a.label.cmp(&b.label));
    result.items.dedup_by(|a, b| a.label == b.label);
    Some(CompletionResult {
        list: result,
        needs_local_metadata,
    })
}
fn import_list_candidates(
    source: &str,
    cursor: &crate::pass::parser::CursorContext,
    importer: &crate::ast::ModulePath,
    sources: &CompletionSources<'_>,
) -> CompletionList {
    use crate::pass::parser::ImportSelectionKind;
    let context = cursor.import.as_ref().expect("import cursor");
    let mut result = CompletionList {
        is_incomplete: false,
        items: Vec::new(),
    };
    let name = context.provider.segments.join("/");
    let Some((provider_source, provider)) = sources.provider(&name) else {
        result.is_incomplete = true;
        return result;
    };
    let selected: std::collections::HashSet<_> = context
        .selected
        .iter()
        .map(|item| match item {
            crate::ast::ImportItem::Name { name, .. } => name.clone(),
            crate::ast::ImportItem::Label { name, .. } => format!("{{{name}}}"),
            crate::ast::ImportItem::OperatorPattern { grammar, .. } => grammar.render(),
        })
        .collect();
    let range = LineIndex::new(source).to_range(cursor.atom.replacement);
    let mut push = |entry: &crate::doc_entry::DocEntry<'_>, label: String, kind, selection| {
        if selected.contains(&label)
            || !matches!(context.kind, ImportSelectionKind::Any) && context.kind != selection
        {
            return;
        }
        result.items.push(CompletionItem {
            label: label.clone(),
            kind: Some(kind),
            detail: Some(entry.signature()),
            documentation: doc_comment_documentation(provider_source, entry.doc()),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range: lsp_types::Range::new(
                    Position::new(range.start.line, range.start.character),
                    Position::new(range.end.line, range.end.character),
                ),
                new_text: label,
            })),
            ..Default::default()
        });
    };
    for entry in crate::doc_entry::documented_items_module(&provider) {
        use crate::doc_entry::DocEntry;
        if !crate::pass::resolve::is_visible(entry.visibility(), importer) {
            continue;
        }
        let (kind, selection) = match &entry {
            DocEntry::Fn(_) | DocEntry::HostFn(_) | DocEntry::Elaborator(_) => {
                (CompletionItemKind::FUNCTION, ImportSelectionKind::Name)
            }
            DocEntry::TypeAlias(..)
            | DocEntry::Newtype(..)
            | DocEntry::HostType(_)
            | DocEntry::Labels(..)
            | DocEntry::LabelNominal { .. } => {
                (CompletionItemKind::STRUCT, ImportSelectionKind::Name)
            }
            DocEntry::LabelForward(..) => (CompletionItemKind::FIELD, ImportSelectionKind::Label),
            DocEntry::LiteralAlias(_) => (CompletionItemKind::CONSTANT, ImportSelectionKind::Name),
            DocEntry::Op(..) => (
                CompletionItemKind::OPERATOR,
                ImportSelectionKind::FixedOperator,
            ),
            DocEntry::VariadicOperator(..) => (
                CompletionItemKind::OPERATOR,
                ImportSelectionKind::VariadicOperator,
            ),
        };
        push(&entry, entry.name().to_owned(), kind, selection);
        match &entry {
            DocEntry::Labels(labels, _, _) => {
                for label in &labels.entries {
                    if !label.is_reuse_marker() {
                        push(
                            &entry,
                            format!("{{{}}}", label.name),
                            CompletionItemKind::FIELD,
                            ImportSelectionKind::Label,
                        );
                    }
                }
            }
            DocEntry::LabelNominal {
                labels,
                entry: label,
                ..
            } if labels.type_alias_name.is_none() => {
                push(
                    &entry,
                    format!("{{{}}}", label.name),
                    CompletionItemKind::FIELD,
                    ImportSelectionKind::Label,
                );
            }
            _ => {}
        }
    }
    result.items.sort_by(|a, b| a.label.cmp(&b.label));
    result.items.dedup_by(|a, b| a.label == b.label);
    result
}

/// The LSP `CompletionItemKind` a scope-walk candidate kind renders as.
fn to_lsp_kind(kind: EntryKind) -> CompletionItemKind {
    match kind {
        EntryKind::Function => CompletionItemKind::FUNCTION,
        EntryKind::Variable => CompletionItemKind::VARIABLE,
        EntryKind::Constant => CompletionItemKind::CONSTANT,
        EntryKind::Type => CompletionItemKind::STRUCT,
        EntryKind::TypeParameter => CompletionItemKind::TYPE_PARAMETER,
        EntryKind::Module => CompletionItemKind::MODULE,
    }
}

/// Convert an intermediate `CompletionEntry` into an LSP
/// `CompletionItem`.  Looks up the type in the position index and
/// formats it as `detail`.
fn entry_to_lsp(
    entry: CompletionEntry,
    source: Option<&str>,
    module: Option<&Module<Surface>>,
    metadata: Option<&CompletionMetadata<'_>>,
) -> CompletionItem {
    if let CandidateOrigin::Import { provider, .. } = &entry.origin
        && let Some(imported) = metadata
            .and_then(|metadata| metadata.imports.get(provider))
            .and_then(|items| items.get(&entry.label))
            .and_then(Option::as_ref)
    {
        return imported.clone();
    }
    let mut detail = metadata.and_then(|metadata| metadata.type_detail(&entry));
    let mut documentation = module
        .zip(source)
        .and_then(|(module, source)| item_documentation(module, source, &entry));

    if matches!(entry.origin, CandidateOrigin::Builtin(_))
        && let Some((builtin_detail, builtin_doc)) = builtin_completion(&entry.label)
    {
        if detail.is_none() {
            detail = builtin_detail;
        }
        if documentation.is_none() {
            documentation = Some(builtin_doc);
        }
    }

    let mut item = CompletionItem {
        label: entry.label,
        kind: Some(to_lsp_kind(entry.kind)),
        detail,
        documentation,
        ..Default::default()
    };
    if let CandidateOrigin::Declaration(span) = entry.origin
        && let Some(module) = module
        && let Some(declaration) = module
            .items
            .iter()
            .find_map(|source_item| match source_item {
                Item::Elaborator(declaration, _)
                    if declaration.meta.span.start <= span.start
                        && span.end <= declaration.meta.span.end
                        && (declaration.name == item.label
                            || format!("{}!", declaration.name) == item.label) =>
                {
                    Some(declaration)
                }
                _ => None,
            })
    {
        block_calls::enrich(&mut item, declaration);
    }
    item
}

fn item_documentation(
    module: &Module<Surface>,
    source: &str,
    candidate: &CompletionEntry,
) -> Option<Documentation> {
    let CandidateOrigin::Declaration(span) = candidate.origin else {
        return None;
    };
    let label = candidate.label.as_str();
    for item in &module.items {
        if !(item.span().start <= span.start && span.end <= item.span().end) {
            continue;
        }
        match item {
            Item::FnDef(d) if d.name == label => {
                return doc_comment_documentation(source, d.doc.as_ref());
            }
            Item::RecGroup(group, _) => {
                if let Some(member) = group.members.iter().find(|member| member.name == label) {
                    return doc_comment_documentation(source, member.doc.as_ref());
                }
            }
            Item::TypeAlias(a) if a.name == label => {
                return doc_comment_documentation(source, a.doc.as_ref());
            }
            Item::LiteralAlias(l, _) if l.name == label => {
                return doc_comment_documentation(source, l.doc.as_ref());
            }
            Item::Newtype(n) if n.name == label => {
                return doc_comment_documentation(source, n.doc.as_ref());
            }
            Item::TypeRecGroup(_)
            | Item::Labels(_, _)
            | Item::Op(_, _)
            | Item::VariadicOperator(_, _) => {
                if let Some(entry) = crate::doc_entry::doc_entry_for_name(item, label) {
                    return doc_comment_documentation(source, entry.doc());
                }
            }
            Item::Elaborator(e, _) if e.name == label || format!("{}!", e.name) == label => {
                return doc_comment_documentation(source, e.doc.as_ref());
            }
            _ => {}
        }
    }
    None
}

struct DeclarationType<'a> {
    span: crate::span::Span,
    local: bool,
    ty: &'a crate::ast::Type<crate::ast::Lowered>,
}

/// The dispatcher authenticates the dependency snapshot before supplying
/// analysis. URI/source identity and declaration indexing are request-owned,
/// so candidate count never multiplies filesystem checks or occurrence scans.
struct CompletionMetadata<'a> {
    declarations: std::collections::HashMap<&'a str, Vec<DeclarationType<'a>>>,
    imports: std::collections::HashMap<
        String,
        std::collections::HashMap<String, Option<CompletionItem>>,
    >,
}

impl<'a> CompletionMetadata<'a> {
    fn empty() -> Self {
        Self {
            declarations: Default::default(),
            imports: Default::default(),
        }
    }

    fn new(
        uri: &Uri,
        source: &str,
        importer: &crate::ast::ModulePath,
        analysis: &'a LspAnalysis,
    ) -> Option<Self> {
        use crate::pass::typecheck_full::ResolvedBinder;
        let canonical = crate::lsp::util::uri_to_canonical(uri)?;
        if analysis.sources.get(&canonical).map(String::as_str) != Some(source) {
            return None;
        }
        let module_path = analysis.file_to_module.get(&canonical)?;
        if importer.segments.join("/") != *module_path {
            return None;
        }
        let mut result = Self {
            declarations: Default::default(),
            imports: Default::default(),
        };
        for ((mp, span), binder) in analysis.position_index.declaration_binders_iter() {
            if mp != module_path {
                continue;
            }
            let Some(label) = binder_label(binder) else {
                continue;
            };
            let local = match binder {
                ResolvedBinder::Local { decl_span, .. }
                | ResolvedBinder::TypeParam { decl_span, .. } => {
                    if *decl_span != Some(*span) {
                        continue;
                    }
                    true
                }
                _ => false,
            };
            let Some(ty) = analysis.position_index.type_at(mp, *span) else {
                continue;
            };
            result
                .declarations
                .entry(label)
                .or_default()
                .push(DeclarationType {
                    span: *span,
                    local,
                    ty,
                });
        }
        Some(result)
    }

    fn add_imports(
        &mut self,
        importer: &crate::ast::ModulePath,
        candidates: &[CompletionEntry],
        sources: &CompletionSources<'_>,
    ) {
        let providers: std::collections::HashSet<_> = candidates
            .iter()
            .filter_map(|candidate| {
                if let CandidateOrigin::Import { provider, .. } = &candidate.origin {
                    Some(provider)
                } else {
                    None
                }
            })
            .collect();
        for provider_name in providers {
            let Some((provider_source, provider)) = sources.provider(provider_name) else {
                continue;
            };
            let items = self.imports.entry(provider_name.clone()).or_default();
            for entry in crate::doc_entry::documented_items_module(&provider) {
                use crate::doc_entry::DocEntry;
                if !crate::pass::resolve::is_visible(entry.visibility(), importer) {
                    continue;
                }
                let (label, kind) = match &entry {
                    DocEntry::Fn(_) | DocEntry::HostFn(_) => {
                        (entry.name().to_owned(), CompletionItemKind::FUNCTION)
                    }
                    DocEntry::Elaborator(_) => {
                        (format!("{}!", entry.name()), CompletionItemKind::FUNCTION)
                    }
                    DocEntry::LiteralAlias(_) => {
                        (entry.name().to_owned(), CompletionItemKind::CONSTANT)
                    }
                    DocEntry::TypeAlias(..)
                    | DocEntry::Newtype(..)
                    | DocEntry::HostType(_)
                    | DocEntry::Labels(..)
                    | DocEntry::LabelNominal { .. } => {
                        (entry.name().to_owned(), CompletionItemKind::STRUCT)
                    }
                    DocEntry::LabelForward(..)
                    | DocEntry::Op(..)
                    | DocEntry::VariadicOperator(..) => continue,
                };
                let mut item = CompletionItem {
                    label,
                    kind: Some(kind),
                    detail: match &entry {
                        DocEntry::Elaborator(elaborator) => {
                            Some(crate::pretty::pretty_type(&elaborator.call_ty))
                        }
                        _ => entry.ty(),
                    },
                    documentation: doc_comment_documentation(provider_source, entry.doc()),
                    ..Default::default()
                };
                if let DocEntry::Elaborator(declaration) = &entry {
                    block_calls::enrich(&mut item, declaration);
                }
                items
                    .entry(entry.name().to_owned())
                    .and_modify(|ambiguous| *ambiguous = None)
                    .or_insert(Some(item));
            }
        }
    }

    fn type_detail(&self, candidate: &CompletionEntry) -> Option<String> {
        let (origin_span, local) = match candidate.origin {
            CandidateOrigin::Local(span) => (span, true),
            CandidateOrigin::Declaration(span) => (span, false),
            _ => return None,
        };
        let mut declarations = self
            .declarations
            .get(candidate.label.as_str())?
            .iter()
            .filter(|entry| {
                entry.local == local
                    && origin_span.start <= entry.span.start
                    && entry.span.end <= origin_span.end
            });
        let declaration = declarations.next()?;
        if declarations.next().is_some() {
            return None;
        }
        let mut detail = String::new();
        write_type(declaration.ty, &mut detail);
        Some(detail)
    }
}

#[cfg(test)]
fn type_detail_for(
    candidate: &CompletionEntry,
    uri: &Uri,
    source: &str,
    analysis: &LspAnalysis,
) -> Option<String> {
    let module = parse_module(source)?;
    CompletionMetadata::new(uri, source, &module.path, analysis)?.type_detail(candidate)
}

/// Extract the user-visible name from a `ResolvedBinder`, if any.
fn binder_label(binder: &crate::pass::typecheck_full::ResolvedBinder) -> Option<&str> {
    use crate::pass::typecheck_full::ResolvedBinder;
    match binder {
        ResolvedBinder::Local { name, .. } => Some(name),
        ResolvedBinder::Fn { name, .. } => Some(name),
        ResolvedBinder::HostEnvFn { name, .. } => Some(name),
        ResolvedBinder::TypeAlias { name, .. } | ResolvedBinder::HostType { name, .. } => {
            Some(name)
        }
        ResolvedBinder::Newtype { name, .. } => Some(name),
        ResolvedBinder::TypeParam { name, .. } => Some(name),
        ResolvedBinder::Intrinsic { name } => Some(name),
        // Qualified aliases, member paths — no single short name.
        ResolvedBinder::QualifiedImport { alias } => Some(alias),
        ResolvedBinder::NewtypeMember { .. }
        | ResolvedBinder::BlockLabel { .. }
        | ResolvedBinder::QualifiedImportMember { .. } => None,
    }
}

#[cfg(test)]
fn parse_module(source: &str) -> Option<Module<Surface>> {
    crate::pass::parser::parse_module_file(source)
        .map(|module_file| module_file.module)
        .ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn block_snippet_uses_an_editable_prefix_region_and_ordered_descriptors() {
        let source = "module app; elab choose : (. & .) -> . { trailing product; trailing thunk fallback; impl implementation } fn run() { cho }";
        let items = complete_at_marker(source, "{ cho");
        let item = items.iter().find(|item| item.label == "choose!").unwrap();
        assert_eq!(
            item.insert_text_format,
            Some(lsp_types::InsertTextFormat::SNIPPET)
        );
        assert_eq!(
            item.insert_text.as_deref(),
            Some("choose! ${1} { ${2} } fallback { ${3} }$0")
        );
        let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
            panic!("edit");
        };
        assert_eq!(Some(edit.new_text.as_str()), item.insert_text.as_deref());
        assert!(
            item.detail
                .as_ref()
                .unwrap()
                .contains("trailing thunk fallback")
        );
        let shadowed = source.replace("fn run()", "fn run(choose: .)");
        let items = complete_at_marker(&shadowed, "{ cho");
        let local = items.iter().find(|item| item.label == "choose").unwrap();
        assert!(local.insert_text_format.is_none());
        assert!(
            !local
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("trailing"))
        );
        assert!(items.iter().any(|item| item.label == "choose!"
            && item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET)));
    }

    fn import_list_completions(
        source: &str,
        byte_offset: u32,
        _module: Option<&Module<Surface>>,
        analysis: Option<&LspAnalysis>,
    ) -> Option<CompletionList> {
        let probe = crate::pass::parser::probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(byte_offset),
            None,
        );
        let cursor = probe.facts.cursor.as_ref()?;
        cursor.import.as_ref()?;
        Some(import_list_candidates(
            source,
            cursor,
            probe.facts.module_path.as_ref()?,
            &CompletionSources {
                local: analysis,
                catalog: analysis,
                state: None,
            },
        ))
    }

    fn import_list_context(
        source: &str,
        offset: u32,
        _module: Option<&Module<Surface>>,
    ) -> Option<crate::pass::parser::CursorContext> {
        let probe = crate::pass::parser::probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(offset),
            None,
        );
        let cursor = probe.facts.cursor?;
        cursor.import.as_ref()?;
        Some(cursor)
    }

    use super::*;
    use crate::lsp::util::{test_file_path, test_file_uri};
    use std::str::FromStr;

    fn make_pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        let mut v: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        v.sort();
        v
    }

    // Helper: collect entries from a source string at a given position,
    // without analysis (no type detail).
    fn complete_at(source: &str, line: u32, character: u32) -> Vec<CompletionItem> {
        let uri = lsp_types::Uri::from_str("untitled:///test.kio").unwrap();
        complete_at_uri(&uri, source, line, character)
    }

    fn complete_at_uri(uri: &Uri, source: &str, line: u32, character: u32) -> Vec<CompletionItem> {
        let pos = make_pos(line, character);
        let list = handle_completion(uri, &pos, Some(source), None);
        list.map(|l| l.items).unwrap_or_default()
    }

    fn complete_at_marker(source: &str, marker: &str) -> Vec<CompletionItem> {
        let offset = source.find(marker).expect("marker in source") + marker.len();
        let pos = LineIndex::new(source).to_position(offset as u32);
        complete_at(source, pos.line, pos.character)
    }

    #[test]
    fn exact_scope_shadowing_does_not_borrow_declaration_documentation() {
        let source = "module pkg/main;\n/// Outer documentation.\nfn value() -> . { () }\nfn consumer(value: .) -> . { value }";
        let items = complete_at_marker(source, "{ value");
        let local = items.iter().find(|item| item.label == "value").unwrap();
        assert_eq!(local.kind, Some(CompletionItemKind::VARIABLE));
        assert!(local.documentation.is_none(), "{local:?}");
    }

    #[test]
    fn completion_metadata_preserves_selected_provider_kinds() {
        let source = "module pkg/main; import provider(expand, answer); fn f() -> . { () }";
        let provider = "module provider; /// Expansion docs.\npub elab expand : . -> . { impl expand_body } pub literal answer = 1;";
        let uri = test_file_uri("/pkg/main.kio");
        let path = crate::lsp::util::uri_to_canonical(&uri).unwrap();
        let provider_path = test_file_path("/provider.kio");
        let mut analysis = operator_catalog(source);
        analysis.file_to_module = std::collections::BTreeMap::from([
            (path.clone(), "pkg/main".into()),
            (provider_path.clone(), "provider".into()),
        ]);
        analysis.sources = std::collections::HashMap::from([
            (path, source.into()),
            (provider_path, provider.into()),
        ]);
        let pos = LineIndex::new(source).to_position((source.find("{ ()").unwrap() + 2) as u32);
        let result = handle_completion(
            &uri,
            &Position {
                line: pos.line,
                character: pos.character,
            },
            Some(source),
            Some(&analysis),
        )
        .unwrap();
        let elaborator = result
            .items
            .iter()
            .find(|item| item.label == "expand!")
            .expect("selected provider elaborator insertion");
        assert_eq!(elaborator.kind, Some(CompletionItemKind::FUNCTION));
        assert!(format!("{:?}", elaborator.documentation).contains("Expansion docs."));
        assert_eq!(
            result
                .items
                .iter()
                .find(|item| item.label == "answer")
                .unwrap()
                .kind,
            Some(CompletionItemKind::CONSTANT)
        );
    }

    #[test]
    fn completion_metadata_does_not_substitute_a_use_site_type() {
        use crate::ast::{Lowered, Meta, Type};
        use crate::pass::typecheck_full::ResolvedBinder;
        use crate::span::Span;
        let source = "module pkg/main; fn f(value: .) -> . { value }";
        let module = crate::pass::parser::parse(source).unwrap();
        let candidate = crate::scope_walk::in_scope_candidates(&module, (source.len() - 2) as u32)
            .into_iter()
            .find(|candidate| candidate.label == "value")
            .unwrap();
        let CandidateOrigin::Local(decl_span) = candidate.origin else {
            panic!("local")
        };
        let uri = test_file_uri("/completion-use-site.kio");
        let path = crate::lsp::util::uri_to_canonical(&uri).unwrap();
        let mut analysis = operator_catalog(source);
        analysis.file_to_module =
            std::collections::BTreeMap::from([(path.clone(), "pkg/main".into())]);
        analysis.sources = std::collections::HashMap::from([(path, source.into())]);
        let start = source.rfind("value").unwrap() as u32;
        let occurrence = Span::new(start, start + 5);
        analysis
            .position_index
            .record_local_decl("pkg/main", "value", decl_span);
        analysis.position_index.record_binder(
            "pkg/main",
            occurrence,
            ResolvedBinder::Local {
                name: "value".into(),
                decl_span: Some(decl_span),
            },
        );
        analysis.position_index.record_type(
            "pkg/main",
            occurrence,
            Type::<Lowered>::Unit {
                meta: Meta::new(occurrence),
            },
        );
        assert!(
            type_detail_for(&candidate, &uri, source, &analysis).is_none(),
            "an adapted use is not the bound declaration type"
        );
    }

    #[test]
    fn exact_scope_details_require_current_exact_declaration() {
        use crate::ast::{Lowered, Meta, Type};
        use crate::span::Span;
        let source =
            "module pkg/main; fn before(value: .) -> . { value } fn after(value: .) -> . { value }";
        let module = crate::pass::parser::parse(source).unwrap();
        let candidate = crate::scope_walk::in_scope_candidates(&module, (source.len() - 2) as u32)
            .into_iter()
            .find(|candidate| candidate.label == "value")
            .unwrap();
        let CandidateOrigin::Local(local_span) = candidate.origin else {
            panic!("local origin")
        };
        let uri = test_file_uri("/completion-identity.kio");
        let path = crate::lsp::util::uri_to_canonical(&uri).unwrap();
        let mut analysis = operator_catalog(source);
        analysis.file_to_module =
            std::collections::BTreeMap::from([(path.clone(), "pkg/main".to_owned())]);
        analysis.sources = std::collections::HashMap::from([(path.clone(), source.to_owned())]);
        let other_start = source.find("value").unwrap() as u32;
        let other_span = Span::new(other_start, other_start + 5);
        let ty = Type::<Lowered>::Unit {
            meta: Meta::new(Span::new(0, 0)),
        };
        analysis
            .position_index
            .record_local_decl("pkg/main", "value", other_span);
        analysis
            .position_index
            .record_type("pkg/main", other_span, ty.clone());
        assert!(type_detail_for(&candidate, &uri, source, &analysis).is_none());
        analysis
            .position_index
            .record_local_decl("pkg/main", "value", local_span);
        analysis
            .position_index
            .record_type("pkg/main", local_span, ty);
        assert_eq!(
            type_detail_for(&candidate, &uri, source, &analysis).as_deref(),
            Some(".")
        );
        analysis.sources.insert(path, format!("{source} "));
        assert!(type_detail_for(&candidate, &uri, source, &analysis).is_none());
    }

    fn operator_catalog(source: &str) -> LspAnalysis {
        let path = test_file_path("/workspace/syntax.kio");
        let sources = std::collections::HashMap::from([(path.clone(), source.to_owned())]);
        LspAnalysis {
            position_index: crate::pass::typecheck_full::PositionIndex::new(),
            file_to_module: std::collections::BTreeMap::from([(path, "syntax".to_owned())]),
            label_reuse_indexes: crate::lsp::label_reuse::indexes_from_sources(&sources),
            sources,
            generated_label_nominals: Default::default(),
            root_package_lowered: crate::pass::resolve::Package::from_parts(
                std::collections::BTreeMap::new(),
                None,
            ),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn qualified_provider_members_are_visibility_and_namespace_exact() {
        let analysis = operator_catalog(
            "module syntax; pub fn visible() { () } fn hidden() { () } pub type Item = .; pub newtype Box : . { pub constructor make; projector read }",
        );
        for (fragment, expected) in [
            ("selected.", vec!["visible"]),
            ("selected.Box.", vec!["make"]),
        ] {
            let source = format!("module app; import syntax as selected; fn run() {{ {fragment}");
            let uri: Uri = test_file_uri("/app.kio");
            let position = LineIndex::new(&source).to_position(source.len() as u32);
            let result = handle_completion(
                &uri,
                &Position::new(position.line, position.character),
                Some(&source),
                Some(&analysis),
            )
            .unwrap();
            assert_eq!(labels(&result.items), expected, "{fragment}");
        }
        let source = "module app; import syntax as selected; fn run(selected: .) { selected.";
        let uri: Uri = test_file_uri("/app.kio");
        let position = LineIndex::new(source).to_position(source.len() as u32);
        assert!(
            handle_completion(
                &uri,
                &Position::new(position.line, position.character),
                Some(source),
                Some(&analysis)
            )
            .unwrap()
            .items
            .is_empty()
        );
    }

    #[test]
    fn qualified_completion_reads_only_selected_identity_edges() {
        let mut analysis = operator_catalog(
            "module syntax; import terminal as owner; import absent as unrelated; pub type Forward = owner.Box; pub fn visible() { () }",
        );
        let terminal = test_file_path("/workspace/terminal.kio");
        analysis
            .file_to_module
            .insert(terminal.clone(), "terminal".to_owned());
        analysis.sources.insert(
            terminal,
            "module terminal; pub newtype Box : . { pub constructor make; pub projector read }"
                .to_owned(),
        );
        let mut failures = Vec::new();
        for (fragment, expected_reads, expected_labels) in [
            ("Box.", vec![], vec!["make", "read"]),
            ("selected.", vec!["syntax"], vec!["visible"]),
            ("Alias.", vec!["syntax", "terminal"], vec!["make", "read"]),
        ] {
            let source = format!(
                "module app; import syntax as selected; import missing as unused; type Alias = selected.Forward; newtype Box : . {{ constructor make; projector read; }}; fn run() {{ {fragment}"
            );
            let uri: Uri = test_file_uri("/app.kio");
            let position = LineIndex::new(&source).to_position(source.len() as u32);
            COMPLETION_PROVIDER_READS.with(|reads| reads.borrow_mut().clear());
            let started = std::time::Instant::now();
            let result = handle_completion(
                &uri,
                &Position::new(position.line, position.character),
                Some(&source),
                Some(&analysis),
            )
            .unwrap();
            let elapsed = started.elapsed();
            let reads =
                COMPLETION_PROVIDER_READS.with(|reads| std::mem::take(&mut *reads.borrow_mut()));
            eprintln!("qualified route {fragment}: provider_reads={reads:?}, elapsed={elapsed:?}");
            if reads != expected_reads
                || labels(&result.items) != expected_labels
                || result.is_incomplete
            {
                failures.push(format!(
                    "{fragment}: reads={reads:?}, labels={:?}, incomplete={}",
                    labels(&result.items),
                    result.is_incomplete
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    const OPERATOR_PROVIDER: &str = "module syntax;
        fn pair(a: ., b: .) -> . { a }
        fn zero() -> . { () }
        /// Adds two values.
        pub op _ + _ { impl pair }
        pub(app) op (_ ? __) { impl pair }
        op _ - _ { impl pair }
        /// Collects values.
        pub varop [%?:+- -+:?%] { foldl pair zero }";

    #[test]
    fn operator_import_completion_uses_full_grammar_and_visibility() {
        let analysis = operator_catalog(OPERATOR_PROVIDER);
        for (importer, expected) in [("app/main", 3), ("outside/main", 2)] {
            for tail in ["", "op", "varop [%", "op (_ ?"] {
                let source = format!("module {importer}; import syntax({tail}");
                let list =
                    import_list_completions(&source, source.len() as u32, None, Some(&analysis))
                        .expect("operator selection");
                assert!(!list.is_incomplete);
                let count = match tail {
                    "varop [%" => 1,
                    "op (_ ?" => expected - 1,
                    _ => expected,
                };
                assert_eq!(list.items.len(), count, "{source}");
                assert!(!labels(&list.items).contains(&"op _ - _"));
                assert_eq!(
                    labels(&list.items).contains(&"op _ + _"),
                    tail != "varop [%"
                );
                assert_eq!(
                    labels(&list.items).contains(&"varop [%?:+- -+:?%]"),
                    tail != "op (_ ?"
                );
                for item in list.items {
                    assert_eq!(item.kind, Some(CompletionItemKind::OPERATOR));
                    let Some(CompletionTextEdit::Edit(edit)) = item.text_edit else {
                        panic!("full grammar insertion");
                    };
                    assert_eq!(edit.new_text, item.label);
                    assert!(
                        item.detail.as_deref().unwrap().contains("impl")
                            || item.detail.as_deref().unwrap().contains("foldl")
                    );
                    let line_index = LineIndex::new(&source);
                    let offset = line_index.position_to_offset(LspPosition {
                        line: edit.range.start.line,
                        character: edit.range.start.character,
                    }) as usize;
                    let inserted = format!("{}{});", &source[..offset], edit.new_text);
                    let module = crate::pass::parser::parse(&inserted).expect("inserted import");
                    let crate::ast::ImportKind::Selective { items, .. } = &module.imports[0].kind
                    else {
                        panic!("selective import")
                    };
                    let crate::ast::ImportItem::OperatorPattern { grammar, .. } = &items[0] else {
                        panic!("operator import")
                    };
                    assert_eq!(grammar.render(), item.label);
                }
            }
        }
    }

    #[test]
    fn import_list_completion_preserves_every_export_namespace() {
        let source = "module syntax;
            host type Host;
            host fn host_value() -> .;
            pub fn visible() -> . { () }
            pub fn op() -> . { () }
            fn hidden() -> . { () }
            pub(app) fn scoped() -> . { () }
            pub type Value = .;
            pub newtype Wrapped : . { pub constructor make; pub projector get }
            pub literal answer = 1;
            pub labels Packet = { alpha: ., beta: . };
            pub labels { gamma: . };
            pub(app) labels { local: . };
            /// The forwarded field's own documentation.
            pub type {forward} = {gamma};
            pub(app) type {local_forward} = {local};
            type {hidden_forward} = {gamma};
            pure fn expand_value(value: .) -> . { value }
            pub elab expand : . -> . { impl expand_value }
            pub op _ + _ { impl visible }
            pub varop [* *] { foldr visible visible }";
        let analysis = operator_catalog(source);
        let ordinary = [
            "Alpha",
            "Beta",
            "Gamma",
            "Host",
            "Packet",
            "Value",
            "Wrapped",
            "answer",
            "expand",
            "host_value",
            "op",
            "visible",
        ];
        for importer in ["app/main", "outside/main"] {
            for prefix in ["", "op", "vis", "{", "op _", "varop ["] {
                let text = format!("module {importer}; import syntax({prefix}");
                let list = import_list_completions(&text, text.len() as u32, None, Some(&analysis))
                    .unwrap();
                let names = labels(&list.items);
                assert!(!list.is_incomplete);
                assert!(!names.contains(&"hidden"));
                assert!(!names.contains(&"expand_value"));
                assert!(!names.contains(&"make"));
                assert!(!names.contains(&"get"));
                assert!(!names.contains(&"Forward"));
                assert!(!names.contains(&"Local_forward"));
                assert!(!names.contains(&"{hidden_forward}"));
                match prefix {
                    "" | "op" => {
                        for name in ordinary {
                            assert!(names.contains(&name), "{importer}: {names:?}");
                        }
                        for name in [
                            "{alpha}",
                            "{beta}",
                            "{gamma}",
                            "{forward}",
                            "op _ + _",
                            "varop [* *]",
                        ] {
                            assert!(names.contains(&name), "{importer}: {names:?}");
                        }
                        assert_eq!(names.contains(&"scoped"), importer == "app/main");
                        assert_eq!(names.contains(&"{local}"), importer == "app/main");
                        assert_eq!(names.contains(&"Local"), importer == "app/main");
                        assert_eq!(names.contains(&"{local_forward}"), importer == "app/main");
                    }
                    "vis" => assert!(list.items.iter().all(|item| !matches!(
                        item.kind,
                        Some(CompletionItemKind::FIELD | CompletionItemKind::OPERATOR)
                    ))),
                    "{" => assert!(
                        list.items
                            .iter()
                            .all(|item| item.kind == Some(CompletionItemKind::FIELD))
                    ),
                    _ => assert!(
                        list.items
                            .iter()
                            .all(|item| item.kind == Some(CompletionItemKind::OPERATOR))
                    ),
                }
                assert!(!list.items.is_empty(), "{prefix}");
                if matches!(prefix, "" | "op" | "{") {
                    let forwards: Vec<_> = list
                        .items
                        .iter()
                        .filter(|item| item.label == "{forward}")
                        .collect();
                    assert_eq!(forwards.len(), 1);
                    assert_eq!(forwards[0].kind, Some(CompletionItemKind::FIELD));
                    let Some(Documentation::MarkupContent(doc)) = &forwards[0].documentation else {
                        panic!("forwarding completion keeps its own documentation");
                    };
                    assert!(doc.value.contains("forwarded field's own documentation"));
                }
                for item in list.items {
                    let Some(CompletionTextEdit::Edit(edit)) = item.text_edit else {
                        panic!("selection edit")
                    };
                    assert_eq!(edit.new_text, item.label);
                    let offset = LineIndex::new(&text).position_to_offset(LspPosition {
                        line: edit.range.start.line,
                        character: edit.range.start.character,
                    }) as usize;
                    let inserted = format!("{}{});", &text[..offset], edit.new_text);
                    crate::pass::parser::parse(&inserted).expect("namespace-correct selection");
                }
            }
        }
    }

    #[test]
    fn operator_import_completion_keeps_selection_and_token_boundaries() {
        for prefix in [
            "module app/main; import syntax(",
            "module app/main; import syntax(\n  , ",
            "module app/main; import syntax(\n , // Leading trivia\n , ",
            "module app/main; import syntax(foo, op",
            "module app/main; import syntax(varop [* *], op",
            "module app/main; import syntax(op (_ ? ___), op",
            "module app/main; import syntax(varop [%",
            "module app/main; import syntax // Provider\n (foo, // Item\n op",
        ] {
            let context = import_list_context(prefix, prefix.len() as u32, None).expect(prefix);
            assert_eq!(
                context.import.as_ref().unwrap().provider.segments,
                ["syntax"]
            );
            let selected = &prefix[context.atom.replacement.start as usize..];
            assert!(
                selected.is_empty() || selected.starts_with("op") || selected.starts_with("varop"),
                "{selected}"
            );
        }
        for source in [
            "module app/main; import syntax as alias",
            "module app/main; import syntax(op _ + _);",
            "module app/main; fn import() -> . { () }",
        ] {
            assert!(
                import_list_context(source, source.len() as u32, None).is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn import_list_completion_preserves_comments_around_replaced_selections() {
        let analysis = operator_catalog(
            "module syntax; pub labels { field: . }; pub fn foo() -> . { () }
             pub op _ + _ { impl foo }",
        );
        for (selection, prefix, requested) in [
            ("foo", "f", "foo"),
            (
                "{ // Name context\n field // Closing context\n }",
                "{",
                "{field}",
            ),
            ("op _ + _", "op", "op _ + _"),
        ] {
            let source =
                format!("module app/main; import syntax({selection} // Beside the delimiter\n);");
            let parsed = crate::pass::parser::parse(&source).unwrap();
            let start = source.find(selection).unwrap();
            let cursor = (start + prefix.len()) as u32;
            let list =
                import_list_completions(&source, cursor, Some(&parsed), Some(&analysis)).unwrap();
            let item = list
                .items
                .into_iter()
                .find(|item| item.label == requested)
                .unwrap();
            let Some(CompletionTextEdit::Edit(edit)) = item.text_edit else {
                panic!("edit")
            };
            let index = LineIndex::new(&source);
            let start = index.position_to_offset(LspPosition {
                line: edit.range.start.line,
                character: edit.range.start.character,
            }) as usize;
            let end = index.position_to_offset(LspPosition {
                line: edit.range.end.line,
                character: edit.range.end.character,
            }) as usize;
            assert_eq!(&source[start..end], selection);
            let result = format!("{}{}{}", &source[..start], edit.new_text, &source[end..]);
            assert!(result.contains("// Beside the delimiter"));
            crate::pass::parser::parse(&result).expect("completed selection preserves delimiters");
        }
    }

    #[test]
    fn operator_import_completion_replaces_the_whole_existing_projection() {
        let source = "module app/main; import syntax(op _ + _); fn run() -> . { () }";
        let module = crate::pass::parser::parse(source).unwrap();
        let offset = (source.find("op _").unwrap() + 2) as u32;
        let analysis = operator_catalog(OPERATOR_PROVIDER);
        let list = import_list_completions(source, offset, Some(&module), Some(&analysis)).unwrap();
        let selected = list
            .items
            .into_iter()
            .find(|item| item.label.starts_with("varop"))
            .unwrap();
        let Some(CompletionTextEdit::Edit(edit)) = selected.text_edit else {
            panic!("edit")
        };
        let index = LineIndex::new(source);
        let start = index.position_to_offset(LspPosition {
            line: edit.range.start.line,
            character: edit.range.start.character,
        }) as usize;
        let end = index.position_to_offset(LspPosition {
            line: edit.range.end.line,
            character: edit.range.end.character,
        }) as usize;
        assert_eq!(&source[start..end], "op _ + _");
        let rewritten = format!("{}{}{}", &source[..start], edit.new_text, &source[end..]);
        crate::pass::parser::parse(&rewritten).expect("whole selection replacement");
        assert!(import_list_context(source, source.len() as u32 - 2, Some(&module)).is_none());
    }

    #[test]
    fn module_level_fn_names_visible() {
        let source = "module pkg/main;\npub fn run() -> . { () }\nfn helper() -> . { () }\n";
        // Position on line 1 inside `run`'s body (character 22 = after `{`)
        let items = complete_at(source, 1, 22);
        let lbls = labels(&items);
        assert!(
            !lbls.contains(&"run"),
            "ordinary fn cannot refer to itself; got {lbls:?}"
        );
        assert!(
            !lbls.contains(&"helper"),
            "later fn is not in scope; got {lbls:?}"
        );
    }

    #[test]
    fn rec_member_completion_excludes_rec_only_callees_but_keeps_params() {
        let source = "module pkg/main;\n\
rec(loop) {\n\
  fn first(x: .) -> . { rec second(x) };\n\
  fn second(y: .) -> . { rec first(y) }\n\
}\n";
        let items = complete_at_marker(source, "rec first(");
        let names = labels(&items);
        assert!(
            names.contains(&"y"),
            "missing member parameter; got {names:?}"
        );
        assert!(!names.contains(&"first"), "got {names:?}");
        assert!(!names.contains(&"second"), "got {names:?}");
    }

    #[test]
    fn fn_params_visible_inside_body() {
        let source = "module pkg/main;\nfn f(x: .) -> . { x }\n";
        let items = complete_at_marker(source, "{ x");
        let lbls = labels(&items);
        assert!(
            lbls.contains(&"x"),
            "fn param `x` should be visible in body; got {lbls:?}"
        );
    }

    #[test]
    fn let_binding_visible_in_body_but_not_before() {
        // `let y = …` — `y` is in scope in the body (after the `;`)
        let source = "module pkg/main;\nfn f(x: .) -> . {\n    let y = x;\n    y\n}\n";
        // Position inside the `y` on the last expr line: line 3, char 4
        let items = complete_at(source, 3, 4);
        let lbls = labels(&items);
        assert!(
            lbls.contains(&"y"),
            "let-bound `y` should be in scope after the `;`; got {lbls:?}"
        );
    }

    #[test]
    fn shadowed_binding_only_inner_visible() {
        // `fn f(x: .) -> . { let x = (); x }` — the let `x` shadows
        // the parameter `x`.  Only one `x` should appear.
        let source = "module pkg/main;\nfn f(x: .) -> . { let x = (); x }\n";
        // cursor on the trailing `x` — inside the let body
        let items = complete_at(source, 1, 31);
        let lbls = labels(&items);
        // `x` appears exactly once (the dedup keeps innermost = let-bound).
        let x_count = items.iter().filter(|i| i.label == "x").count();
        assert_eq!(
            x_count, 1,
            "shadowed `x` must appear exactly once in completions; got {lbls:?}"
        );
    }

    #[test]
    fn imported_names_visible() {
        // `import pkg/helper(h);` brings `h` into scope.
        let source = "module pkg/main;\nimport pkg/helper(h);\nfn f() -> . { () }\n";
        // cursor inside f's body
        let items = complete_at_marker(source, "{ ");
        let lbls = labels(&items);
        assert!(
            lbls.contains(&"h"),
            "`h` imported via `import` should be in completion list; got {lbls:?}"
        );
    }

    #[test]
    fn selective_label_import_does_not_enter_ordinary_completion_scope() {
        let source =
            "module pkg/main;\nimport pkg/labels(item, Item, {field});\nfn f() -> . { () }\n";
        let items = complete_at_marker(source, "{ ");
        let labels = labels(&items);
        assert!(labels.contains(&"item"));
        assert!(!labels.contains(&"Item"));
        assert!(!labels.contains(&"field"));
    }

    #[test]
    fn ordinary_label_nominal_completion_keeps_owner_prose() {
        let source = "module pkg/main;\n/// The field label.\nlabels { field: . };\nfn f(value: Field) -> . { () }\n";
        let items = complete_at_marker(source, "value: ");
        let item = items
            .iter()
            .find(|item| item.label == "Field")
            .expect("generated nominal completion");
        let Some(Documentation::MarkupContent(markup)) = &item.documentation else {
            panic!("expected documentation: {item:?}");
        };
        assert!(markup.value.contains("The field label."));
        assert!(!markup.value.contains("```"));
        let value_items = complete_at_marker(source, "-> . { ");
        assert!(!labels(&value_items).contains(&"Field"));
    }

    #[test]
    fn literal_decl_completion_is_constant_with_docs() {
        let source = "\
module pkg/main;
/// Shared greeting text.
literal greeting = \"hi\";
fn f() -> . { () }
";
        let items = complete_at_marker(source, "f() -> . { ");
        let item = items
            .iter()
            .find(|item| item.label == "greeting")
            .unwrap_or_else(|| {
                panic!("literal alias completion missing; got {:?}", labels(&items))
            });
        assert_eq!(item.kind, Some(CompletionItemKind::CONSTANT));
        let Some(Documentation::MarkupContent(markup)) = &item.documentation else {
            panic!("expected Markdown documentation for literal completion");
        };
        assert!(markup.value.contains("Shared greeting text."));
    }

    #[test]
    fn recursive_type_group_completion_keeps_member_docs() {
        let source = "\
module pkg/main;
rec {
  /// Transparent recursive chain.
  pub type Chain = Node;
  /// Nominal recursive node.
  pub newtype Node : Chain { pub constructor make_node; pub projector read_node }
}
fn use_site(value: Node) -> . { () }
";
        let items = complete_at_marker(source, "value: ");
        let node = items
            .iter()
            .find(|item| item.label == "Node")
            .unwrap_or_else(|| {
                panic!(
                    "recursive group member completion missing: {:?}",
                    labels(&items)
                )
            });
        let Some(Documentation::MarkupContent(markup)) = &node.documentation else {
            panic!("expected recursive group member documentation")
        };
        assert!(markup.value.contains("Nominal recursive node."));
    }

    #[test]
    fn type_position_after_elaborator_colon_filters_to_type_names() {
        let source = "\
module pkg/main;
import pkg/types(Imported, _Imported);
import pkg/values(value_fn);
type Pair = .;
type _Pair = .;
newtype Box : . { pub constructor mk_box; pub projector un_box }
newtype _Box : . { pub constructor mk_marked; pub projector un_marked }
fn helper(x: .) -> . { x }
pub elab foo : [Source] Source -> [Target] Target { impl helper }
";
        let items = complete_at_marker(source, "foo : ");
        let lbls = labels(&items);

        for label in ["Box", "Imported", "Pair", "_Box", "_Imported", "_Pair"] {
            assert!(
                lbls.contains(&label),
                "type-position completion should include `{label}`; got {lbls:?}"
            );
        }
        for label in ["helper", "value_fn"] {
            assert!(
                !lbls.contains(&label),
                "type-position completion must not include value `{label}`; got {lbls:?}"
            );
        }
        for label in ["Pair", "_Pair"] {
            let pair = items.iter().find(|item| item.label == label).unwrap();
            assert_eq!(pair.kind, Some(CompletionItemKind::STRUCT));
        }
    }

    #[test]
    fn type_position_in_fn_body_includes_type_params_not_values() {
        let source = "\
module pkg/main;
type Pair = .;
fn f[A](x: A) -> A { x }
";
        let items = complete_at_marker(source, "x: ");
        let lbls = labels(&items);

        assert!(
            lbls.contains(&"A"),
            "type param should be visible: {lbls:?}"
        );
        assert!(
            lbls.contains(&"Pair"),
            "module type alias should be visible: {lbls:?}"
        );
        for label in ["f", "x"] {
            assert!(
                !lbls.contains(&label),
                "type-position completion must not include value `{label}`; got {lbls:?}"
            );
        }
        let type_param = items.iter().find(|item| item.label == "A").unwrap();
        assert_eq!(type_param.kind, Some(CompletionItemKind::TYPE_PARAMETER));
    }

    #[test]
    fn imported_elaborator_completion_uses_bang_name_and_docs() {
        let source = "\
module pkg/elabs;
/// Builds the identity term.
pub elab identity : [A] A -> A { impl identity_impl }
fn identity_impl() -> . { () }
fn use_site() -> . { () }
";
        let items = complete_at_marker(source, "use_site() -> . { ");
        let lbls = labels(&items);
        let item = items
            .iter()
            .find(|item| item.label == "identity!")
            .unwrap_or_else(|| panic!("imported elaborator completion; got {lbls:?}"));
        assert_eq!(item.kind, Some(CompletionItemKind::FUNCTION));
        let Some(Documentation::MarkupContent(markup)) = &item.documentation else {
            panic!("expected Markdown documentation for elaborator completion");
        };
        assert!(markup.value.contains("Builds the identity term."));
    }

    #[test]
    fn qualified_import_alias_visible() {
        let source = "module pkg/main;\nimport pkg/helper as h;\nfn f() -> . { () }\n";
        let items = complete_at_marker(source, "{ ");
        let lbls = labels(&items);
        assert!(
            lbls.contains(&"h"),
            "qualified-import alias `h` should be in completion list; got {lbls:?}"
        );
    }

    #[test]
    fn top_level_position_no_fn_locals() {
        // At the top-level (module header line), fn-local binders
        // must not appear.
        let source = "module pkg/main;\nfn f(x: .) -> . { x }\n";
        // cursor on the module header line (line 0)
        let items = complete_at(source, 0, 5);
        let lbls = labels(&items);
        assert!(
            !lbls.contains(&"x"),
            "fn-local `x` must not appear at module-level cursor; got {lbls:?}"
        );
    }

    #[test]
    fn package_build_block_keywords_visible_without_module_parse() {
        let uri = test_file_uri("/pkg.pkg.kio");
        let source = "package pkg;\n\nbuild {\n  \n}\n";
        let items = complete_at_uri(&uri, source, 3, 2);
        let lbls = labels(&items);
        for label in ["cache", "docs", "target"] {
            assert!(
                lbls.contains(&label),
                "package build block should complete `{label}`; got {lbls:?}"
            );
        }
    }

    #[test]
    fn package_target_block_keys_visible_without_module_parse() {
        let uri = test_file_uri("/pkg.pkg.kio");
        let source = "package pkg;\n\nbuild {\n  target rust {\n    \n  }\n}\n";
        let items = complete_at_uri(&uri, source, 4, 4);
        let lbls = labels(&items);
        for label in ["out", "namespace", "thread_safety"] {
            assert!(
                lbls.contains(&label),
                "package target block should complete `{label}`; got {lbls:?}"
            );
        }
    }

    #[test]
    fn non_module_variant_files_complete_their_own_header() {
        let cases = [
            ("file:///pkg.sig.kio", "signature pkg v(1);\n"),
            (
                "file:///mathlib.dep.kio",
                "dependency mathlib;\n\nsource {\n}\n",
            ),
            (
                "file:///mathlib.lock.kio",
                "lock mathlib;\n\nresolved {\n}\n",
            ),
        ];

        for (uri, source) in cases {
            let uri = Uri::from_str(uri).unwrap();
            let pos = make_pos(0, 0);
            let list = handle_completion(&uri, &pos, Some(source), None);
            assert_eq!(
                labels(&list.expect("file-kind completion").items),
                [source.split_whitespace().next().unwrap()]
            );
        }
    }
}
