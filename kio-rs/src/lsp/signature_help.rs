//! LSP `textDocument/signatureHelp` handler.
//!
//! V1 supports direct path callees whose path segment resolved to a
//! top-level Kio function or host function in the latest typed
//! snapshot. Signature help reads resolved binder facts only; it does
//! not participate in name resolution and cannot affect open-world
//! compilation.

use lsp_types::{SignatureHelp, SignatureInformation, TextDocumentPositionParams};

use crate::ast::{
    CallArg, ElaboratorCall, Expr, HostFnParam, Item, Module, OpChainKind, SignatureGroupRef,
    SignatureParam, Surface, TypeParam,
};
use crate::cmd::check::LspAnalysis;
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::pass::typecheck_core::write_type;
use crate::pass::typecheck_full::ResolvedBinder;
use crate::span::Span;

pub fn handle_signature_help(
    params: &TextDocumentPositionParams,
    source: &str,
    module: &Module<Surface>,
    analysis: Option<&LspAnalysis>,
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
    let [segment] = segments.as_slice() else {
        return None;
    };
    let callable = resolved_callable(analysis, module_path, segment.span)?;
    let ty = analysis
        .position_index
        .type_at(module_path, call.callee.span())
        .or_else(|| analysis.position_index.type_at(module_path, segment.span))?;
    let label = declaration_signature_label(analysis, &callable).unwrap_or_else(|| {
        let mut rendered = String::new();
        write_type(ty, &mut rendered);
        format!("{}: {rendered}", callable.name())
    });
    let active_value_arg = active_value_arg(call.args, offset, |slot_index| {
        analysis
            .position_index
            .is_value_at_type_slot(module_path, call.span, slot_index)
    });
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: None,
            parameters: None,
            active_parameter: Some(active_value_arg),
        }],
        active_signature: Some(0),
        active_parameter: Some(active_value_arg),
    })
}

#[derive(Debug, Clone)]
enum CallableResolution {
    Fn { module_path: String, name: String },
    HostEnvFn { module_path: String, name: String },
}

impl CallableResolution {
    fn name(&self) -> &str {
        match self {
            CallableResolution::Fn { name, .. } | CallableResolution::HostEnvFn { name, .. } => {
                name
            }
        }
    }
}

fn resolved_callable(
    analysis: &LspAnalysis,
    module_path: &str,
    span: Span,
) -> Option<CallableResolution> {
    analysis
        .position_index
        .binders_iter()
        .find_map(|((mp, s), binder)| {
            if mp != module_path || *s != span {
                return None;
            }
            match binder {
                ResolvedBinder::Fn { module_path, name } => Some(CallableResolution::Fn {
                    module_path: module_path.clone(),
                    name: name.clone(),
                }),
                ResolvedBinder::HostEnvFn { module_path, name } => {
                    Some(CallableResolution::HostEnvFn {
                        module_path: module_path.clone(),
                        name: name.clone(),
                    })
                }
                _ => None,
            }
        })
}

fn declaration_signature_label(
    analysis: &LspAnalysis,
    callable: &CallableResolution,
) -> Option<String> {
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
            Item::RecGroup(group, _) if matches!(callable, CallableResolution::Fn { .. }) => {
                if let Some(member) = group
                    .members
                    .iter()
                    .find(|member| member.name == name.as_str())
                {
                    return Some(fn_signature_label(member));
                }
            }
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

fn fn_signature_label(def: &crate::ast::FnDef<crate::ast::Lowered>) -> String {
    let mut out = format!("fn {}", def.name);
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
                    out.push_str(&param.name);
                    out.push_str(": ");
                    if let Some(ty) = &param.ty {
                        write_type(ty, &mut out);
                    } else {
                        out.push('.');
                    }
                }
                out.push(')');
            }
        }
    }
    out.push_str(" -> ");
    write_type(&def.ret, &mut out);
    out
}

fn host_signature_label(host: &crate::ast::HostFn<crate::ast::Lowered>) -> String {
    let mut out = format!("host fn {}", host.name);
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
                    out.push_str(param.name.as_deref().unwrap_or("_"));
                    out.push_str(": ");
                    write_type(&param.ty, &mut out);
                }
                out.push(')');
            }
        }
    }
    out.push_str(" -> ");
    write_type(&host.ret, &mut out);
    out
}

fn type_binder_text(param: &TypeParam) -> String {
    let stars = "*".repeat(param.effective_kind().arity());
    format!("[{stars}{}]", param.name)
}

#[derive(Debug, Clone, Copy)]
struct CallSite<'a> {
    callee: &'a Expr<Surface>,
    args: &'a [CallArg<Surface>],
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
            record_call(callee, args, meta.span, offset, best);
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
        | Expr::EnrichedRecord { ext, .. }
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
    args: &'a [CallArg<Surface>],
    span: Span,
    offset: u32,
    best: &mut Option<CallSite<'a>>,
) {
    if !contains(span, offset) {
        return;
    }
    let candidate = CallSite { callee, args, span };
    if best.is_none_or(|current| span_len(candidate.span) < span_len(current.span)) {
        *best = Some(candidate);
    }
}

fn active_value_arg(
    args: &[CallArg<Surface>],
    offset: u32,
    is_value_at_type_slot: impl Fn(usize) -> bool,
) -> u32 {
    let mut index = 0u32;
    for (slot_index, arg) in args.iter().enumerate() {
        if is_value_at_type_slot(slot_index) {
            continue;
        }
        match arg {
            CallArg::Type(_) => {}
            CallArg::Value(value) if offset > value.span().end => index += 1,
            CallArg::Value(_) => break,
        }
    }
    index
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
    fn active_value_argument_ignores_type_arguments() {
        let source = "module pkg/main;\n\nfn run() -> . { foo(Int, (), ()) }\n";
        let parsed = parse_lazy(source).unwrap().force_all().unwrap();
        let offset = source.rfind("()").unwrap() as u32;
        let call = nearest_call(&parsed, offset).unwrap();
        let mut index = crate::pass::typecheck_full::PositionIndex::new();
        index.record_value_at_type_slot("pkg/main", call.span, 0);
        let active = active_value_arg(call.args, offset, |slot_index| {
            index.is_value_at_type_slot("pkg/main", call.span, slot_index)
        });
        assert_eq!(active, 1);
    }
}
