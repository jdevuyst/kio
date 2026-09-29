use super::*;
use crate::ast::{BlockExposure, UserElaboratorDef};
use crate::pass::parser::{BlockCursorRegion, ToolingProbe};
use crate::scope_walk::BlockHeader as Header;
use lsp_types::{
    InsertTextFormat, ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
};

fn exposure(exposure: BlockExposure) -> &'static str {
    match exposure {
        BlockExposure::Product => "product",
        BlockExposure::Thunk => "thunk",
        BlockExposure::Sequence => "sequence",
    }
}

pub(super) fn enrich(item: &mut CompletionItem, declaration: &UserElaboratorDef) {
    if declaration.trailing_blocks.is_empty() {
        return;
    }
    let header = Header::from(declaration);
    if !header.valid_labels() {
        return;
    }
    // A single editable prefix region does not assert a public argument count.
    let mut snippet = format!("{}! ${{1}}", header.name);
    for (index, (_, label)) in header.blocks.iter().enumerate() {
        if let Some(label) = label {
            snippet.push_str(&format!(" {label}"));
        }
        snippet.push_str(&format!(" {{ ${{{}}} }}", index + 2));
    }
    snippet.push_str("$0");
    item.label = format!("{}!", header.name);
    item.detail = Some(format!(
        "{}; {}",
        header.public_type,
        header
            .blocks
            .iter()
            .map(|(kind, label)| format!(
                "trailing {}{}",
                exposure(*kind),
                label
                    .as_ref()
                    .map_or(String::new(), |label| format!(" {label}"))
            ))
            .collect::<Vec<_>>()
            .join("; ")
    ));
    item.insert_text = Some(snippet);
    item.insert_text_format = Some(InsertTextFormat::SNIPPET);
}

fn selected_header(
    module: &Module<Surface>,
    name: &str,
    offset: u32,
    sources: &CompletionSources<'_>,
) -> Option<Header> {
    crate::scope_walk::selected_block_header(module, name, offset, |name| {
        sources.provider(name).map(|(_, module)| module)
    })
}

pub(super) fn complete_labels(
    probe: &ToolingProbe<'_>,
    module: Option<&Module<Surface>>,
    sources: &CompletionSources<'_>,
) -> CompletionList {
    let mut result = CompletionList {
        is_incomplete: false,
        items: Vec::new(),
    };
    let (Some(module), Some(fact), Some(cursor)) =
        (module, &probe.facts.block_cursor, &probe.facts.cursor)
    else {
        return result;
    };
    let Some(header) = selected_header(module, fact.head.as_str(), fact.head.span.start, sources)
    else {
        result.is_incomplete = true;
        return result;
    };
    if !header.matches_prefix(&fact.labels) {
        return result;
    }
    let BlockCursorRegion::Label(index) = fact.region else {
        return result;
    };
    if let Some((kind, Some(label))) = header.blocks.get(index) {
        let range = LineIndex::new(probe.source).to_range(cursor.atom.replacement);
        result.items.push(CompletionItem {
            label: label.clone(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(format!("trailing {} {label}", exposure(*kind))),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range: lsp_types::Range::new(
                    Position::new(range.start.line, range.start.character),
                    Position::new(range.end.line, range.end.character),
                ),
                new_text: label.clone(),
            })),
            ..Default::default()
        });
    }
    result
}

pub(crate) fn handle_signature_request(
    uri: &Uri,
    position: &Position,
    workspace_root: Option<&std::path::Path>,
    state: &crate::lsp::state::ServerState,
) -> Option<SignatureHelp> {
    let path = crate::lsp::util::uri_to_canonical(uri)?;
    if !crate::file_kind::is_module_file(&filename_from_uri(uri)) {
        return None;
    }
    let source = state
        .document(uri)
        .map(|document| document.text().to_owned())
        .or_else(|| std::fs::read_to_string(&path).ok())?;
    let offset = LineIndex::new(&source).position_to_offset(LspPosition {
        line: position.line,
        character: position.character,
    });
    let probe = crate::pass::parser::probe_tooling(
        &source,
        Some(crate::ast::KioFileKind::Module),
        Some(offset),
        None,
    );
    if probe.facts.suppression.is_some() {
        return None;
    }
    let fact = probe.facts.block_cursor.as_ref()?;
    let module = crate::scope_walk::tooling_module(&probe)?;
    if crate::lsp::signature_help::nearest_call_start(&module, offset)
        .is_some_and(|start| start > fact.head.span.start)
    {
        return None;
    }
    crate::package_collection::ModuleFileParserContext::from_module(&path, &module).ok()?;
    let catalog = state
        .analysis_for_file(&path)
        .or_else(|| workspace_root.and_then(|root| state.analysis_for_package_root(root)));
    let sources = CompletionSources {
        local: None,
        catalog,
        state: Some(state),
    };
    let header = selected_header(&module, fact.head.as_str(), fact.head.span.start, &sources)?;
    if !header.matches_prefix(&fact.labels) {
        return None;
    }
    let mut labels = vec!["prefix values".to_owned()];
    labels.extend(header.blocks.iter().map(|(kind, label)| {
        format!(
            "{}{} block",
            label
                .as_ref()
                .map_or(String::new(), |label| format!("{label}: ")),
            exposure(*kind)
        )
    }));
    let active = match fact.region {
        BlockCursorRegion::Prefix => 0,
        BlockCursorRegion::Body(index) | BlockCursorRegion::Label(index) => index + 1,
    };
    if active >= labels.len() {
        return None;
    }
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: format!("{}! [{}]: {}", header.name, labels.join(", "), header.public_type),
            documentation: Some(Documentation::String("Prefix values follow the public call type; this region does not specify a fixed argument count.".to_owned())),
            parameters: Some(labels.into_iter().map(|label| ParameterInformation { label: ParameterLabel::Simple(label), documentation: None }).collect()),
            active_parameter: Some(active as u32),
        }],
        active_signature: Some(0), active_parameter: Some(active as u32),
    })
}
