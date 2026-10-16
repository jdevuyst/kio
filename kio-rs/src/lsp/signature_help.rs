//! LSP `textDocument/signatureHelp` handler.
//!
//! V1 supports direct path callees whose path segment resolved to a
//! top-level Kio function or host function in the latest typed
//! snapshot. Signature help consumes committed semantic facts and renders
//! declaration spelling; it does not participate in name resolution and
//! cannot affect open-world compilation.

use lsp_types::{
    ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
    TextDocumentPositionParams,
};

use crate::ast::{
    CallArg, ElaboratorCall, Expr, HostFnParam, Item, Module, OpChainKind, SignatureGroupRef,
    SignatureParam, Surface, TypeParam,
};
use crate::cmd::check::LspAnalysis;
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::pass::typecheck_core::write_type;
use crate::pass::typecheck_full::{CallArgumentRole, CallValueProjection, ResolvedBinder};
use crate::span::Span;

pub fn handle_signature_help(
    params: &TextDocumentPositionParams,
    source: &str,
    module: &Module<Surface>,
    analysis: Option<&LspAnalysis>,
    label_offsets: bool,
) -> Option<SignatureHelp> {
    let analysis = analysis?;
    let uri = &params.text_document.uri;
    let canonical = crate::lsp::util::uri_to_canonical(uri)?;
    let module_path = analysis.file_to_module.get(canonical.as_path())?;
    let line_index = LineIndex::new(source);
    let offset = line_index.position_to_offset(LspPosition {
        line: params.position.line,
        character: params.position.character,
    });
    let call = nearest_call(module, offset)?;
    let Expr::Path { segments, .. } = call.callee else {
        return None;
    };
    // The leaf's recorded binder carries the selected declaration's owner;
    // a qualified path must not look up its spelling in the consumer module.
    let segment = segments.last()?;
    let callable = resolved_callable(analysis, module_path, call.callee.span(), segment.span)?;
    let rendered = declaration_signature_label(analysis, &callable)?;
    let label_offsets = label_offsets.then(|| rendered.label_offsets());
    let layers = analysis
        .position_index
        .call_presentation(module_path, call.span)?;
    let mut parameters = Vec::new();
    let mut sources = Vec::new();
    let mut stage = 0;
    for (ordinal, layer) in layers.values().enumerate() {
        let selected = layer.layer?;
        // A context-inferred returned forall has no written application of
        // its residual value layer. It contributes no phantom parameter.
        if ordinal > 0 && layer.arguments.is_empty() && !selected.implicit_unit {
            continue;
        }
        let mut has_written_value = false;
        let first_value = parameters.len() as u32;
        for ((start, end), role) in &layer.arguments {
            let parameter = match role {
                CallArgumentRole::Type => selected.has_value_layer.then_some(first_value),
                CallArgumentRole::Value(projection) => {
                    has_written_value = true;
                    let parameter = parameters.len() as u32;
                    let region = rendered.value_region(stage, *projection);
                    parameters.push(rendered.parameter(region, label_offsets.as_ref()));
                    Some(parameter)
                }
            };
            sources.push((Span::new(*start, *end), parameter));
        }
        if !has_written_value && selected.has_value_layer {
            let region =
                rendered.value_region(stage, CallValueProjection::Range { first: 0, width: 1 });
            parameters.push(rendered.parameter(region, label_offsets.as_ref()));
            if layer.arguments.is_empty() {
                sources.push((Span::new(call.span.end, call.span.end), Some(first_value)));
            }
        }
        stage += usize::from(selected.consumed);
    }
    let active_parameter = active_parameter(&sources, offset);
    // LSP clients default an omitted active parameter to zero. A written
    // type-only scalar stage has no value to highlight, including after an
    // earlier consumed value layer, so expose no parameter in that context.
    if active_parameter.is_none() {
        parameters.clear();
    }
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: rendered.label,
            documentation: None,
            parameters: Some(parameters),
            active_parameter,
        }],
        active_signature: Some(0),
        active_parameter,
    })
}

#[derive(Debug, Clone)]
enum CallableResolution {
    Fn { module_path: String, name: String },
    HostEnvFn { module_path: String, name: String },
}

fn resolved_callable(
    analysis: &LspAnalysis,
    module_path: &str,
    path: Span,
    span: Span,
) -> Option<CallableResolution> {
    match analysis
        .position_index
        .signature_callee_binder(module_path, path, span)?
    {
        ResolvedBinder::Fn { module_path, name } => Some(CallableResolution::Fn {
            module_path: module_path.clone(),
            name: name.clone(),
        }),
        ResolvedBinder::HostEnvFn { module_path, name } => Some(CallableResolution::HostEnvFn {
            module_path: module_path.clone(),
            name: name.clone(),
        }),
        _ => None,
    }
}

fn declaration_signature_label(
    analysis: &LspAnalysis,
    callable: &CallableResolution,
) -> Option<RenderedSignature> {
    let (decl_module_path, name) = match callable {
        CallableResolution::Fn { module_path, name }
        | CallableResolution::HostEnvFn { module_path, name } => (module_path, name),
    };
    let entry = analysis.root_package_lowered.module(decl_module_path)?;
    for item in &entry.module.items {
        match item {
            Item::FnDef(def)
                if matches!(callable, CallableResolution::Fn { .. })
                    && def.name == name.as_str() =>
            {
                return Some(fn_signature_label(def));
            }
            Item::RecGroup(_, ext) => match *ext {},
            Item::HostFn(host)
                if matches!(callable, CallableResolution::HostEnvFn { .. })
                    && host.name == name.as_str() =>
            {
                return Some(host_signature_label(host));
            }
            _ => {}
        }
    }
    None
}

struct RenderedValueGroup {
    parameters: Vec<std::ops::Range<usize>>,
    empty: std::ops::Range<usize>,
}

struct RenderedSignature {
    label: String,
    values: Vec<RenderedValueGroup>,
    result: std::ops::Range<usize>,
}

impl RenderedSignature {
    fn label_offsets(&self) -> std::collections::HashMap<usize, u32> {
        let mut offsets = std::collections::HashMap::new();
        let mut chars = self.label.chars();
        let (mut byte, mut utf16) = (0, 0);
        let regions = self
            .values
            .iter()
            .flat_map(|group| {
                if group.parameters.is_empty() {
                    std::slice::from_ref(&group.empty)
                } else {
                    group.parameters.as_slice()
                }
            })
            .chain(std::iter::once(&self.result));
        // Rendered declaration regions are in source order. Scan the label
        // once, retaining only the endpoints later projections can select.
        for region in regions {
            for endpoint in [region.start, region.end] {
                while byte < endpoint {
                    let ch = chars.next().expect("rendered region is inside its label");
                    byte += ch.len_utf8();
                    utf16 += ch.len_utf16() as u32;
                    #[cfg(test)]
                    LABEL_OFFSET_CHARACTERS.with(|count| count.set(count.get() + 1));
                }
                offsets.insert(endpoint, utf16);
            }
        }
        offsets
    }

    fn value_region(
        &self,
        stage: usize,
        projection: CallValueProjection,
    ) -> std::ops::Range<usize> {
        let Some(group) = self.values.get(stage) else {
            return self.result.clone();
        };
        let Some(last) = group.parameters.len().checked_sub(1) else {
            return group.empty.clone();
        };
        // The declaration group displays a right-fold product. Only its last
        // binder can occupy several selected outer-spine components. This
        // projection runs after public-type selection and never feeds typing.
        let (first, end) = match projection {
            CallValueProjection::Range { first, width } => (
                first.min(last),
                first.saturating_add(width.saturating_sub(1)).min(last),
            ),
            CallValueProjection::Tail { first } => (first.min(last), last),
        };
        group.parameters[first].start..group.parameters[end].end
    }

    fn parameter(
        &self,
        region: std::ops::Range<usize>,
        offsets: Option<&std::collections::HashMap<usize, u32>>,
    ) -> ParameterInformation {
        let label = if let Some(offsets) = offsets {
            ParameterLabel::LabelOffsets([offsets[&region.start], offsets[&region.end]])
        } else {
            ParameterLabel::Simple(self.label[region].to_owned())
        };
        ParameterInformation {
            label,
            documentation: None,
        }
    }
}

#[cfg(test)]
thread_local! {
    static LABEL_OFFSET_CHARACTERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn fn_signature_label(def: &crate::ast::FnDef<crate::ast::Lowered>) -> RenderedSignature {
    let mut out = format!("fn {}", def.name);
    let mut values = Vec::new();
    for group in def.sig.canonical_groups() {
        match group {
            SignatureGroupRef::Type(params) => {
                for param in params {
                    let SignatureParam::Type(param) = param else {
                        continue;
                    };
                    out.push_str(&type_binder_text(param));
                }
            }
            SignatureGroupRef::Value(params) => {
                let start = out.len();
                let mut parameters = Vec::new();
                out.push('(');
                let mut first = true;
                for param in params {
                    let SignatureParam::Value(param) = param else {
                        continue;
                    };
                    if !first {
                        out.push_str(", ");
                    }
                    first = false;
                    let parameter_start = out.len();
                    out.push_str(&param.name);
                    out.push_str(": ");
                    if let Some(ty) = &param.ty {
                        write_type(ty, &mut out);
                    } else {
                        out.push('.');
                    }
                    parameters.push(parameter_start..out.len());
                }
                out.push(')');
                values.push(RenderedValueGroup {
                    parameters,
                    empty: start..out.len(),
                });
            }
        }
    }
    out.push_str(" -> ");
    let result_start = out.len();
    write_type(&def.ret, &mut out);
    let result = result_start..out.len();
    RenderedSignature {
        label: out,
        values,
        result,
    }
}

fn host_signature_label(host: &crate::ast::HostFn<crate::ast::Lowered>) -> RenderedSignature {
    let mut out = format!("host fn {}", host.name);
    let mut values = Vec::new();
    for group in crate::ast::host_fn_group_refs(&host.params, &host.param_groups) {
        match group {
            crate::ast::HostFnGroupRef::Type(params) => {
                for param in params {
                    let HostFnParam::Type(param) = param else {
                        continue;
                    };
                    out.push_str(&type_binder_text(param));
                }
            }
            crate::ast::HostFnGroupRef::Value(params) => {
                let start = out.len();
                let mut parameters = Vec::new();
                out.push('(');
                let mut first = true;
                for param in params {
                    let HostFnParam::Value(param) = param else {
                        continue;
                    };
                    if !first {
                        out.push_str(", ");
                    }
                    first = false;
                    let parameter_start = out.len();
                    out.push_str(param.name.as_deref().unwrap_or("_"));
                    out.push_str(": ");
                    write_type(&param.ty, &mut out);
                    parameters.push(parameter_start..out.len());
                }
                out.push(')');
                values.push(RenderedValueGroup {
                    parameters,
                    empty: start..out.len(),
                });
            }
        }
    }
    out.push_str(" -> ");
    let result_start = out.len();
    write_type(&host.ret, &mut out);
    let result = result_start..out.len();
    RenderedSignature {
        label: out,
        values,
        result,
    }
}

fn type_binder_text(param: &TypeParam) -> String {
    let stars = "*".repeat(param.effective_kind().arity());
    format!("[{stars}{}]", param.name)
}

#[derive(Debug, Clone, Copy)]
struct CallSite<'a> {
    callee: &'a Expr<Surface>,
    span: Span,
}

pub(super) fn nearest_call_start(module: &Module<Surface>, offset: u32) -> Option<u32> {
    nearest_call(module, offset).map(|call| call.span.start)
}

fn nearest_call(module: &Module<Surface>, offset: u32) -> Option<CallSite<'_>> {
    let mut best = None;
    for item in &module.items {
        visit_item(item, offset, &mut best);
    }
    best
}

fn visit_item<'a>(item: &'a Item<Surface>, offset: u32, best: &mut Option<CallSite<'a>>) {
    match item {
        Item::FnDef(d) => visit_expr(&d.body, offset, best),
        Item::RecGroup(group, _) => {
            for member in &group.members {
                visit_expr(&member.body, offset, best);
            }
        }
        Item::Equiv(e, _) => {
            for term in &e.terms {
                visit_expr(&term.body, offset, best);
            }
        }
        Item::Elaborator(_, _) => {}
        Item::VariadicOperator(_, _) => {}
        Item::TypeRecGroup(_) => {}
        Item::TypeAlias(_)
        | Item::LiteralAlias(_, _)
        | Item::Newtype(_)
        | Item::Labels(_, _)
        | Item::LabelForward(_, _)
        | Item::Op(_, _)
        | Item::HostType(_)
        | Item::HostFn(_) => {}
    }
}

fn visit_expr<'a>(expr: &'a Expr<Surface>, offset: u32, best: &mut Option<CallSite<'a>>) {
    if !contains(expr.span(), offset) {
        return;
    }
    match expr {
        Expr::BlockCall { prefix, blocks, .. } => {
            for value in prefix {
                visit_expr(value, offset, best);
            }
            for block in blocks {
                for item in &block.items {
                    visit_expr(item.value(), offset, best);
                }
            }
        }
        Expr::Call {
            callee, args, meta, ..
        } => {
            visit_expr(callee, offset, best);
            visit_call_args(args, offset, best);
            record_call(callee, meta.span, offset, best);
        }
        Expr::RecCall { args, .. } => visit_call_args(args, offset, best),
        Expr::FnExpr { body, .. } => visit_expr(body, offset, best),
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            visit_expr(value, offset, best);
            visit_expr(body, offset, best);
        }
        Expr::RowLet { value, body, .. } => {
            visit_expr(value, offset, best);
            visit_expr(body, offset, best);
        }
        Expr::Tuple { items, .. } => {
            for item in items {
                visit_expr(item, offset, best);
            }
        }
        Expr::FnPlaceholder { body, .. } => {
            visit_expr(body, offset, best);
        }
        Expr::LabelValue { labels, .. } => {
            for label in labels {
                visit_expr(&label.value, offset, best);
            }
        }
        Expr::Elaborator { call, .. } => visit_elaborator_call(call, offset, best),
        Expr::UserElaborator { args, .. } => visit_call_args(args, offset, best),
        Expr::Ufcs { receiver, args, .. } => {
            visit_expr(receiver, offset, best);
            visit_call_args(args, offset, best);
        }
        Expr::OpChain { kind, .. } => visit_op_chain(kind, offset, best),
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. }
        | Expr::LowHostCall { ext, .. }
        | Expr::LowModuleCall { ext, .. }
        | Expr::LowQualifiedModuleCall { ext, .. }
        | Expr::LowQualifiedNewtypeMember { ext, .. }
        | Expr::LowNewtypeCtor { ext, .. }
        | Expr::LowNewtypeProj { ext, .. }
        | Expr::LowClosureCall { ext, .. }
        | Expr::LowIndirectCall { ext, .. }
        | Expr::LowTypeApplication { ext, .. }
        | Expr::LowAbsurdCall { ext, .. }
        | Expr::LowCpsProjectorApply { ext, .. }
        | Expr::LowBoundRef { ext, .. }
        | Expr::LowHostFnValueRef { ext, .. }
        | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
    }
}

fn visit_call_args<'a>(args: &'a [CallArg<Surface>], offset: u32, best: &mut Option<CallSite<'a>>) {
    for arg in args {
        if let CallArg::Value(value) = arg {
            visit_expr(value, offset, best);
        }
    }
}

fn visit_elaborator_call<'a>(
    call: &'a ElaboratorCall<Surface>,
    offset: u32,
    best: &mut Option<CallSite<'a>>,
) {
    match call {
        ElaboratorCall::FieldAccess { receiver, .. } => visit_expr(receiver, offset, best),
        ElaboratorCall::FieldUpdate {
            receiver, updates, ..
        } => {
            visit_expr(receiver, offset, best);
            for update in updates {
                visit_expr(&update.value, offset, best);
            }
        }
    }
}

fn visit_op_chain<'a>(
    kind: &'a OpChainKind<Surface>,
    offset: u32,
    best: &mut Option<CallSite<'a>>,
) {
    match kind {
        OpChainKind::Normal { slots, .. } => {
            for slot in slots {
                visit_expr(slot, offset, best);
            }
        }
        OpChainKind::Variadic { elements, .. } => {
            for element in elements {
                visit_expr(element, offset, best);
            }
        }
    }
}

fn record_call<'a>(
    callee: &'a Expr<Surface>,
    span: Span,
    offset: u32,
    best: &mut Option<CallSite<'a>>,
) {
    if !contains(span, offset) {
        return;
    }
    let candidate = CallSite { callee, span };
    if best.is_none_or(|current| span_len(candidate.span) < span_len(current.span)) {
        *best = Some(candidate);
    }
}

fn active_parameter(sources: &[(Span, Option<u32>)], offset: u32) -> Option<u32> {
    sources
        .iter()
        .find(|(span, _)| offset <= span.end)
        .or_else(|| sources.last())?
        .1
}

fn contains(span: Span, offset: u32) -> bool {
    span.start <= offset && offset <= span.end
}

fn span_len(span: Span) -> u32 {
    span.end.saturating_sub(span.start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::parser::parse_lazy;

    #[test]
    fn nearest_call_prefers_inner_call() {
        let source = "module pkg/main;\n\nfn run() -> . { outer(inner(())) }\n";
        let parsed = parse_lazy(source).unwrap().force_all().unwrap();
        let offset = source.rfind("()").unwrap() as u32;
        let call = nearest_call(&parsed, offset).unwrap();
        let Expr::Path { segments, .. } = call.callee else {
            panic!("expected path callee");
        };
        assert_eq!(segments[0].name, "inner");
    }

    #[test]
    fn active_parameter_uses_selected_source_spans_and_stays_in_bounds() {
        let sources = [(Span::new(10, 12), Some(0)), (Span::new(20, 22), Some(1))];
        assert_eq!(active_parameter(&sources, 5), Some(0));
        assert_eq!(active_parameter(&sources, 11), Some(0));
        assert_eq!(active_parameter(&sources, 15), Some(1));
        assert_eq!(active_parameter(&sources, 25), Some(1));
        assert_eq!(active_parameter(&[], 5), None);
        let trailing_type = [(Span::new(10, 12), Some(0)), (Span::new(20, 22), None)];
        assert_eq!(active_parameter(&trailing_type, 11), Some(0));
        assert_eq!(active_parameter(&trailing_type, 21), None);
        assert_eq!(active_parameter(&trailing_type, 25), None);
    }

    #[test]
    fn selected_spine_projection_preserves_packing_and_offset_capability() {
        let label = "fn f(a: ., tail: . & .) -> . -> .".to_owned();
        let a = label.find("a: .").unwrap();
        let tail = label.find("tail: . & .").unwrap();
        let result = label.find(". -> .").unwrap();
        let rendered = RenderedSignature {
            result: result..label.len(),
            label,
            values: vec![RenderedValueGroup {
                parameters: vec![a..a + 4, tail..tail + 11],
                empty: 4..22,
            }],
        };
        let head = |first| CallValueProjection::Range { first, width: 1 };
        assert_eq!(
            rendered.value_region(0, head(1)),
            rendered.value_region(0, head(2))
        );
        let packed = rendered.value_region(0, CallValueProjection::Tail { first: 0 });
        assert_eq!(&rendered.label[packed.clone()], "a: ., tail: . & .");
        assert!(matches!(
            rendered.parameter(packed.clone(), None).label,
            ParameterLabel::Simple(_)
        ));
        assert!(matches!(
            rendered
                .parameter(packed, Some(&rendered.label_offsets()))
                .label,
            ParameterLabel::LabelOffsets(_)
        ));
        assert_eq!(&rendered.label[rendered.value_region(1, head(0))], ". -> .");
    }

    #[test]
    fn signature_call_facts_are_editor_only_and_bounded_by_source_calls() {
        use crate::cmd::check::{
            LspUserElaboratorMemos, analyze_workspace_at_with_overlay_lsp_cancellable_with_memos,
        };
        use crate::package_collection::SourceOverlay;
        use std::collections::BTreeMap;
        use std::time::Instant;
        for call_count in [16, 128] {
            let root = std::env::current_dir()
                .unwrap()
                .join("signature-facts-fixture");
            let mut source = String::from("module main; fn pair(value: . & .) { () }\n");
            for index in 0..call_count {
                source.push_str(&format!("pub fn run_n{index}() {{ pair((), ()) }}\n"));
            }
            let overlay = SourceOverlay::complete(
                root.clone(),
                BTreeMap::from([
                    (
                        root.join("probe.pkg.kio"),
                        "package probe; bridge { main; }".into(),
                    ),
                    (root.join("main.kio"), source.clone()),
                ]),
            );
            let call_start = source.find("pair((), ())").unwrap() as u32;
            let span = Span::new(call_start, call_start + "pair((), ())".len() as u32);
            for editor in [false, true] {
                let memos = if editor {
                    LspUserElaboratorMemos::for_editor()
                } else {
                    LspUserElaboratorMemos::default()
                };
                let start = Instant::now();
                let analysis = analyze_workspace_at_with_overlay_lsp_cancellable_with_memos(
                    &root,
                    &overlay,
                    &crate::lsp::cancel::CancellationToken::new(),
                    &memos,
                )
                .unwrap()
                .unwrap();
                let elapsed = start.elapsed();
                assert_eq!(
                    analysis.position_index.call_presentation_counts(),
                    editor.then_some([1, call_count, call_count, call_count, 2 * call_count])
                );
                crate::pass::typecheck_full::take_signature_call_observations();
                let layers = analysis.position_index.call_presentation("main", span);
                assert_eq!(
                    crate::pass::typecheck_full::take_signature_call_observations(),
                    usize::from(editor)
                );
                assert_eq!(layers.is_some(), editor);
                if let Some(layers) = layers {
                    assert_eq!(layers.len(), 1);
                    assert_eq!(layers.values().next().unwrap().arguments.len(), 2);
                }
                let start = Instant::now();
                for _ in 0..10_000 {
                    std::hint::black_box(analysis.position_index.call_presentation("main", span));
                }
                eprintln!(
                    "signature aggregate editor-facts={editor}: {call_count}-call analysis={elapsed:?}; 10000 indexed queries={:?}",
                    start.elapsed()
                );
                if editor {
                    let module = parse_lazy(&source).unwrap().force_all().unwrap();
                    let position = LineIndex::new(&source).to_position(span.end - 2);
                    let params = TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier {
                            uri: crate::lsp::util::test_file_uri(root.join("main.kio")),
                        },
                        position: lsp_types::Position::new(position.line, position.character),
                    };
                    for offsets in [false, true] {
                        let start = Instant::now();
                        for _ in 0..64 {
                            let help = handle_signature_help(
                                &params,
                                &source,
                                &module,
                                Some(&analysis),
                                offsets,
                            )
                            .unwrap();
                            assert_eq!(help.signatures[0].parameters.as_ref().unwrap().len(), 2);
                            std::hint::black_box(help);
                        }
                        eprintln!(
                            "signature {call_count}-call document: 64 complete handlers offsets={offsets}: {:?}",
                            start.elapsed()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn signature_offsets_scan_a_wide_unicode_label_once() {
        for count in [16, 128] {
            let mut label = "fn wide(".to_owned();
            let mut parameters = Vec::new();
            for index in 0..count {
                if index > 0 {
                    label.push_str(", ");
                }
                let start = label.len();
                label.push_str(&format!("value_n{index}: 𝒜"));
                parameters.push(start..label.len());
            }
            label.push_str(") -> 𝒜");
            let result = label.len() - "𝒜".len()..label.len();
            let rendered = RenderedSignature {
                label,
                values: vec![RenderedValueGroup {
                    parameters,
                    empty: 7..9,
                }],
                result,
            };
            LABEL_OFFSET_CHARACTERS.with(|work| work.set(0));
            let offsets = rendered.label_offsets();
            for region in &rendered.values[0].parameters {
                let ParameterLabel::LabelOffsets(actual) =
                    rendered.parameter(region.clone(), Some(&offsets)).label
                else {
                    panic!("offset-capable client received a string label");
                };
                assert_eq!(
                    actual,
                    [
                        rendered.label[..region.start].encode_utf16().count() as u32,
                        rendered.label[..region.end].encode_utf16().count() as u32
                    ]
                );
            }
            assert_eq!(
                LABEL_OFFSET_CHARACTERS.with(|work| work.get()),
                rendered.label.chars().count()
            );
            assert_eq!(offsets.len(), 2 * (count + 1));
        }
    }
}
