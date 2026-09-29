//! Source-ordered lexical scope over the Surface AST.
//!
//! LSP and REPL expression completion share this syntactic walk. Candidates
//! retain the declaration they denote; optional semantic detail must enrich
//! that exact declaration, never another occurrence with the same spelling.
//! Inner binders precede outer binders, and namespaces are collected separately.

use crate::ast::{
    CallArg, ElaboratorCall, Expr, FnDef, HostFnParam, ImportKind, Item, Labels, Module,
    OpChainKind, ParamPattern, ParamPatternElem, Signature, SignatureParam, Surface, Type,
    TypeParam, TypeRecMember,
};
use crate::span::Span;

#[cfg(any(test, feature = "lsp"))]
mod tooling;
#[cfg(feature = "lsp")]
pub(crate) use tooling::{
    expression_operators, tooling_module, tooling_operators, tooling_path_allowed,
};
#[cfg(any(test, feature = "lsp"))]
pub(crate) use tooling::{tooling_candidates, tooling_keywords};
#[cfg(feature = "lsp")]
mod blocks;
#[cfg(feature = "lsp")]
mod qualified;
#[cfg(feature = "lsp")]
pub(crate) use blocks::{BlockHeader, selected_block_header};
#[cfg(feature = "lsp")]
pub(crate) use qualified::{qualified_candidates, qualified_providers};
#[cfg(feature = "lsp")]
mod call_types;
#[cfg(feature = "repl-core")]
pub(crate) use call_types::expression_callee_shadowed;
#[cfg(feature = "lsp")]
pub(crate) use call_types::{CallHead, public_call_head};

/// One lexically reachable name and its exact syntactic origin.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub label: String,
    pub kind: CandidateKind,
    pub origin: CandidateOrigin,
}

/// Source identity retained independently of optional typechecking data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateOrigin {
    /// A local binder's written token or parameter range.
    Local(Span),
    /// Numbered members of the nearest source placeholder family.
    Placeholder(Span),
    /// The written declaration head or owning declaration range.
    Declaration(Span),
    /// An explicit selection from this exact provider.
    Import { provider: String, span: Span },
    /// A qualified module import, not an ordinary selected declaration.
    QualifiedImport(Span),
    /// A name admitted by a written builtin-module import.
    Builtin(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Function,
    Variable,
    Constant,
    Type,
    TypeParameter,
    Module,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Namespace {
    Value,
    Type,
}

/// Value names at the cursor, innermost first and shadow-deduplicated.
pub fn in_scope_candidates(module: &Module<Surface>, byte_offset: u32) -> Vec<Candidate> {
    candidates(module, byte_offset, Namespace::Value)
}

/// Type names at the cursor, innermost first and shadow-deduplicated.
pub fn type_candidates(module: &Module<Surface>, byte_offset: u32) -> Vec<Candidate> {
    candidates(module, byte_offset, Namespace::Type)
}

/// Type scope from the same parser facts used by interactive completion.
#[cfg(test)]
fn lexical_type_candidates(source: &str, byte_offset: u32) -> Vec<Candidate> {
    let probe = crate::pass::parser::probe_tooling_source(source, Some(byte_offset));
    tooling::namespace_candidates(&probe, byte_offset, Namespace::Type)
}

fn contains(span: Span, offset: u32) -> bool {
    span.start <= offset && offset <= span.end
}

fn push(out: &mut Vec<Candidate>, name: &str, kind: CandidateKind, origin: CandidateOrigin) {
    out.push(Candidate {
        label: name.to_owned(),
        kind,
        origin,
    });
}

fn local(out: &mut Vec<Candidate>, name: &str, span: Span, kind: CandidateKind) {
    if name != "_" && !crate::naming::is_compiler_reserved_name(name) {
        push(out, name, kind, CandidateOrigin::Local(span));
    }
}

fn declaration(out: &mut Vec<Candidate>, name: &str, span: Span, kind: CandidateKind) {
    push(out, name, kind, CandidateOrigin::Declaration(span));
}

fn candidates(module: &Module<Surface>, offset: u32, ns: Namespace) -> Vec<Candidate> {
    let mut out = Vec::new();
    let active = module
        .items
        .iter()
        .position(|item| contains(item.span(), offset));
    if let Some(index) = active {
        item_scope(&module.items[index], offset, ns, &mut out);
    }
    preceding_items_scope(&module.items, offset, active, ns, &mut out);
    imports_scope(&module.imports, offset, ns, &mut out);
    deduplicate(out)
}

fn preceding_items_scope(
    items: &[Item<Surface>],
    offset: u32,
    active: Option<usize>,
    ns: Namespace,
    out: &mut Vec<Candidate>,
) {
    for (index, item) in items.iter().enumerate() {
        if active.map_or(item.span().end <= offset, |active| index < active) {
            item_names(item, ns, out);
        }
    }
}

fn deduplicate(out: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen = std::collections::HashSet::new();
    out.into_iter()
        .filter(|candidate| seen.insert(candidate.label.clone()))
        .collect()
}

fn imports_scope(
    imports: &[crate::ast::Import],
    offset: u32,
    ns: Namespace,
    out: &mut Vec<Candidate>,
) {
    for import in imports.iter().filter(|import| import.span.end <= offset) {
        match &import.kind {
            ImportKind::Selective { items, from } => {
                for item in items {
                    let Some((name, span)) = item.as_name_with_span() else {
                        continue;
                    };
                    if crate::naming::is_type_name(name) == (ns == Namespace::Type) {
                        push(
                            out,
                            name,
                            if ns == Namespace::Type {
                                CandidateKind::Type
                            } else {
                                CandidateKind::Function
                            },
                            CandidateOrigin::Import {
                                provider: from
                                    .segments
                                    .iter()
                                    .map(|s| s.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join("/"),
                                span,
                            },
                        );
                    }
                }
            }
            ImportKind::Qualified { alias, .. } if ns == Namespace::Value => {
                push(
                    out,
                    alias,
                    CandidateKind::Module,
                    CandidateOrigin::QualifiedImport(import.span),
                );
            }
            ImportKind::Intrinsics if ns == Namespace::Value => {
                for name in crate::pass::resolve::PRIME_INTRINSICS {
                    push(
                        out,
                        name,
                        CandidateKind::Function,
                        CandidateOrigin::Builtin("__intrinsics__"),
                    );
                }
            }
            ImportKind::Comptime => {
                for name in crate::comptime::PUBLIC_COMPTIME_NAMES {
                    let Some(builtin) = crate::comptime::ComptimeBuiltin::from_public_name(name)
                    else {
                        continue;
                    };
                    if builtin.is_type_name() == (ns == Namespace::Type) {
                        push(
                            out,
                            name,
                            if ns == Namespace::Type {
                                CandidateKind::Type
                            } else {
                                CandidateKind::Function
                            },
                            CandidateOrigin::Builtin("__comptime__"),
                        );
                    }
                }
            }
            ImportKind::Intrinsics | ImportKind::Qualified { .. } => {}
        }
    }
}

fn item_scope(item: &Item<Surface>, offset: u32, ns: Namespace, out: &mut Vec<Candidate>) {
    match item {
        Item::FnDef(f) => fn_scope(f, offset, ns, out),
        Item::RecGroup(group, _) => {
            if let Some(f) = group.members.iter().find(|f| contains(f.meta.span, offset)) {
                fn_scope(f, offset, ns, out);
            }
            // Current group members are rec callees, not ordinary values.
        }
        Item::Equiv(equiv, _) => {
            for term in &equiv.terms {
                expr_scope(&term.body, offset, ns, out);
            }
            signature_scope(
                &equiv.sig,
                equiv
                    .terms
                    .iter()
                    .any(|term| contains(term.body.span(), offset)),
                offset,
                ns,
                out,
            );
        }
        Item::TypeAlias(alias) if ns == Namespace::Type => {
            type_scope(&alias.body, offset, out);
            type_params(&alias.type_params, offset, out);
        }
        Item::Newtype(newtype) if ns == Namespace::Type => {
            type_scope(&newtype.payload, offset, out);
            type_params(&newtype.existential_params, offset, out);
            type_params(&newtype.type_params, offset, out);
            if newtype.rec_span.is_some() {
                declaration(out, &newtype.name, newtype.name_span, CandidateKind::Type);
            }
        }
        Item::Labels(labels, _) if ns == Namespace::Type => {
            labels_scope(labels, offset, out);
            if labels.rec_span.is_some() {
                labels_names(labels, out);
            } else {
                for entry in &labels.entries {
                    if entry.meta.span.end < offset && !entry.is_reuse_marker() {
                        declaration(
                            out,
                            &crate::ast::mint_label_newtype_name(&entry.name),
                            entry.name_span,
                            CandidateKind::Type,
                        );
                    }
                }
            }
        }
        Item::TypeRecGroup(group) if ns == Namespace::Type => {
            for member in &group.members {
                if contains(member.meta().span, offset) {
                    type_member_scope(member, offset, out);
                }
            }
            for member in &group.members {
                type_member_names(member, out);
            }
        }
        Item::HostType(host) if ns == Namespace::Type => {
            type_params(&host.type_params, offset, out)
        }
        Item::HostFn(host) if ns == Namespace::Type => {
            type_scope(&host.ret, offset, out);
            for param in host.params.iter().rev() {
                match param {
                    HostFnParam::Type(param) => {
                        type_params(std::slice::from_ref(param), offset, out)
                    }
                    HostFnParam::Value(param) => type_scope(&param.ty, offset, out),
                }
            }
        }
        Item::Elaborator(elab, _) if ns == Namespace::Type => {
            type_scope(&elab.call_ty, offset, out)
        }
        _ => {}
    }
}

fn item_names(item: &Item<Surface>, ns: Namespace, out: &mut Vec<Candidate>) {
    match item {
        Item::FnDef(f) if ns == Namespace::Value => {
            declaration(out, &f.name, f.meta.span, CandidateKind::Function)
        }
        Item::RecGroup(group, _) if ns == Namespace::Value => {
            for f in &group.members {
                declaration(out, &f.name, f.meta.span, CandidateKind::Function);
            }
        }
        Item::HostFn(host) if ns == Namespace::Value => {
            declaration(out, &host.name, host.meta.span, CandidateKind::Function)
        }
        Item::LiteralAlias(alias, _) if ns == Namespace::Value => {
            declaration(out, &alias.name, alias.meta.span, CandidateKind::Constant)
        }
        Item::Elaborator(elab, _) if ns == Namespace::Value => declaration(
            out,
            &format!("{}!", elab.name),
            elab.meta.span,
            CandidateKind::Function,
        ),
        Item::TypeAlias(alias) if ns == Namespace::Type => {
            declaration(out, &alias.name, alias.name_span, CandidateKind::Type)
        }
        Item::Newtype(newtype) if ns == Namespace::Type => {
            declaration(out, &newtype.name, newtype.name_span, CandidateKind::Type)
        }
        Item::HostType(host) if ns == Namespace::Type => {
            declaration(out, &host.name, host.meta.span, CandidateKind::Type)
        }
        Item::Labels(labels, _) if ns == Namespace::Type => labels_names(labels, out),
        Item::TypeRecGroup(group) if ns == Namespace::Type => {
            for member in &group.members {
                type_member_names(member, out);
            }
        }
        _ => {}
    }
}

fn labels_names(labels: &Labels<Surface>, out: &mut Vec<Candidate>) {
    if let Some(name) = &labels.type_alias_name {
        declaration(
            out,
            name,
            labels.type_alias_span.unwrap_or(labels.meta.span),
            CandidateKind::Type,
        );
    }
    for entry in &labels.entries {
        if !entry.is_reuse_marker() {
            declaration(
                out,
                &crate::ast::mint_label_newtype_name(&entry.name),
                entry.name_span,
                CandidateKind::Type,
            );
        }
    }
}

fn labels_scope(labels: &Labels<Surface>, offset: u32, out: &mut Vec<Candidate>) {
    if let Some(entry) = labels
        .entries
        .iter()
        .find(|entry| contains(entry.meta.span, offset))
    {
        type_scope(&entry.payload, offset, out);
        type_params(&entry.existential_params, offset, out);
        type_params(&entry.type_params, offset, out);
    }
    type_params(&labels.type_alias_params, offset, out);
}

fn type_member_names(member: &TypeRecMember<Surface>, out: &mut Vec<Candidate>) {
    match member {
        TypeRecMember::TypeAlias(alias) => {
            declaration(out, &alias.name, alias.name_span, CandidateKind::Type)
        }
        TypeRecMember::Newtype(newtype) => {
            declaration(out, &newtype.name, newtype.name_span, CandidateKind::Type)
        }
        TypeRecMember::Labels(labels, _) => labels_names(labels, out),
    }
}

fn type_member_scope(member: &TypeRecMember<Surface>, offset: u32, out: &mut Vec<Candidate>) {
    match member {
        TypeRecMember::TypeAlias(alias) => {
            type_scope(&alias.body, offset, out);
            type_params(&alias.type_params, offset, out);
        }
        TypeRecMember::Newtype(newtype) => {
            type_scope(&newtype.payload, offset, out);
            type_params(&newtype.existential_params, offset, out);
            type_params(&newtype.type_params, offset, out);
        }
        TypeRecMember::Labels(labels, _) => labels_scope(labels, offset, out),
    }
}

fn type_params(params: &[TypeParam], offset: u32, out: &mut Vec<Candidate>) {
    for param in params.iter().rev().filter(|p| p.span.end <= offset) {
        local(out, &param.name, param.span, CandidateKind::TypeParameter);
    }
}

fn type_scope(ty: &Type<Surface>, offset: u32, out: &mut Vec<Candidate>) {
    if !contains(ty.span(), offset) {
        return;
    }
    match ty {
        Type::Forall { param, body, .. } => {
            if contains(body.span(), offset) {
                type_scope(body, offset, out);
                type_params(std::slice::from_ref(param), offset, out);
            }
        }
        Type::Path { args, .. } => {
            for arg in args {
                type_scope(arg, offset, out);
            }
        }
        Type::Function {
            param: left,
            ret: right,
            ..
        }
        | Type::Product { left, right, .. }
        | Type::Sum { left, right, .. } => {
            type_scope(left, offset, out);
            type_scope(right, offset, out);
        }
        Type::LabelSugar { labels, .. } => {
            for label in labels {
                if let Some(payload) = &label.payload {
                    type_scope(payload, offset, out);
                }
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn pattern_scope(
    pattern: &ParamPattern,
    offset: u32,
    ns: Namespace,
    bind: bool,
    out: &mut Vec<Candidate>,
) {
    for elem in pattern.elems.iter().rev() {
        match elem {
            ParamPatternElem::Bind {
                name,
                name_span,
                ty,
            } => {
                if ns == Namespace::Type {
                    type_scope(ty, offset, out);
                } else if bind {
                    local(out, name, *name_span, CandidateKind::Variable);
                }
            }
            ParamPatternElem::Tuple(inner) => pattern_scope(inner, offset, ns, bind, out),
            ParamPatternElem::BindTuple {
                name,
                name_span,
                inner,
            } => {
                pattern_scope(inner, offset, ns, bind, out);
                if bind && ns == Namespace::Value {
                    local(out, name, *name_span, CandidateKind::Variable);
                }
            }
        }
    }
}

fn binding_scope(name: &str, span: Span, pattern: Option<&ParamPattern>, out: &mut Vec<Candidate>) {
    if let Some(pattern) = pattern {
        pattern_scope(pattern, 0, Namespace::Value, true, out);
    }
    local(out, name, span, CandidateKind::Variable);
}

fn signature_scope(
    sig: &Signature<Surface>,
    in_body: bool,
    offset: u32,
    ns: Namespace,
    out: &mut Vec<Candidate>,
) {
    for param in sig.params.iter().rev() {
        match param {
            SignatureParam::Type(param) if ns == Namespace::Type => {
                type_params(std::slice::from_ref(param), offset, out);
            }
            SignatureParam::Value(param) => {
                if ns == Namespace::Value && in_body {
                    binding_scope(&param.name, param.meta.span, param.pattern.as_ref(), out);
                } else if ns == Namespace::Type {
                    if let Some(ty) = &param.ty {
                        type_scope(ty, offset, out);
                    }
                    if let Some(pattern) = &param.pattern {
                        pattern_scope(pattern, offset, ns, false, out);
                    }
                }
            }
            SignatureParam::Type(_) => {}
        }
    }
}

fn fn_scope(f: &FnDef<Surface>, offset: u32, ns: Namespace, out: &mut Vec<Candidate>) {
    expr_scope(&f.body, offset, ns, out);
    if ns == Namespace::Type {
        type_scope(&f.ret, offset, out);
    }
    signature_scope(&f.sig, contains(f.body.span(), offset), offset, ns, out);
}

fn args_scope(args: &[CallArg<Surface>], offset: u32, ns: Namespace, out: &mut Vec<Candidate>) {
    for arg in args {
        match arg {
            CallArg::Value(expr) => expr_scope(expr, offset, ns, out),
            CallArg::Type(ty) if ns == Namespace::Type => type_scope(ty, offset, out),
            CallArg::Type(_) => {}
        }
    }
}

fn placeholder_scope(stem: &crate::ast::PathSegment, slot_count: usize, out: &mut Vec<Candidate>) {
    for slot in 1..=slot_count {
        push(
            out,
            &format!("{}{slot}", stem.name),
            CandidateKind::Variable,
            CandidateOrigin::Placeholder(stem.span),
        );
    }
}

fn expr_scope(expr: &Expr<Surface>, offset: u32, ns: Namespace, out: &mut Vec<Candidate>) {
    if !contains(expr.span(), offset) {
        return;
    }
    match expr {
        Expr::BlockCall { prefix, blocks, .. } => {
            for value in prefix {
                expr_scope(value, offset, ns, out);
            }
            for block in blocks {
                if !contains(Span::new(block.open.end, block.close.end), offset) {
                    continue;
                }
                for item in block.items.iter().rev() {
                    let in_tail = item.span().end <= offset;
                    expr_scope(item.value(), offset, ns, out);
                    match item {
                        crate::ast::NeutralItem::Binding {
                            name,
                            name_span,
                            pattern,
                            ty,
                            ..
                        } => {
                            if in_tail && ns == Namespace::Value {
                                binding_scope(name, *name_span, pattern.as_ref(), out);
                            } else if !in_tail && ns == Namespace::Type {
                                if let Some(ty) = ty {
                                    type_scope(ty, offset, out);
                                }
                                if let Some(pattern) = pattern {
                                    pattern_scope(pattern, offset, ns, false, out);
                                }
                            }
                        }
                        crate::ast::NeutralItem::ExistentialBinding {
                            name,
                            name_span,
                            pattern,
                            type_params: params,
                            ..
                        } => {
                            if in_tail {
                                if ns == Namespace::Value {
                                    binding_scope(name, *name_span, pattern.as_ref(), out);
                                } else {
                                    type_params(params, offset, out);
                                }
                            }
                        }
                        crate::ast::NeutralItem::RowBinding { entries, .. }
                            if in_tail && ns == Namespace::Value =>
                        {
                            for entry in entries.iter().rev() {
                                local(out, &entry.local, entry.local_span, CandidateKind::Variable);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Expr::Let {
            name,
            name_span,
            pattern,
            ty,
            value,
            body,
            ..
        } => {
            if contains(body.span(), offset) {
                expr_scope(body, offset, ns, out);
                if ns == Namespace::Value {
                    binding_scope(name, *name_span, pattern.as_ref(), out);
                }
            } else {
                expr_scope(value, offset, ns, out);
                if ns == Namespace::Type {
                    if let Some(ty) = ty {
                        type_scope(ty, offset, out);
                    }
                    if let Some(pattern) = pattern {
                        pattern_scope(pattern, offset, ns, false, out);
                    }
                }
            }
        }
        Expr::RowLet {
            entries,
            value,
            body,
            ..
        } => {
            if contains(body.span(), offset) {
                expr_scope(body, offset, ns, out);
                if ns == Namespace::Value {
                    for entry in entries.iter().rev() {
                        local(out, &entry.local, entry.local_span, CandidateKind::Variable);
                    }
                }
            } else {
                expr_scope(value, offset, ns, out);
            }
        }
        Expr::Seq { value, body, .. } => {
            if contains(body.span(), offset) {
                expr_scope(body, offset, ns, out);
            } else {
                expr_scope(value, offset, ns, out);
            }
        }
        Expr::FnExpr {
            sig, ret_ty, body, ..
        } => {
            expr_scope(body, offset, ns, out);
            if ns == Namespace::Type
                && let Some(ty) = ret_ty
            {
                type_scope(ty, offset, out);
            }
            signature_scope(sig, contains(body.span(), offset), offset, ns, out);
        }
        Expr::Call { callee, args, .. } => {
            // Existential-open syntax gives its synthetic continuation an
            // enclosing span. Its binders must not leak back into the RHS.
            if contains(callee.span(), offset) {
                expr_scope(callee, offset, ns, out);
            } else {
                args_scope(args, offset, ns, out);
            }
        }
        Expr::RecCall { args, .. } | Expr::UserElaborator { args, .. } => {
            args_scope(args, offset, ns, out)
        }
        Expr::Tuple { items, .. } => {
            for child in items {
                expr_scope(child, offset, ns, out);
            }
        }
        Expr::FnPlaceholder {
            stem, state, body, ..
        } => {
            expr_scope(body, offset, ns, out);
            if ns == Namespace::Value
                && contains(body.span(), offset)
                && !out
                    .iter()
                    .any(|candidate| matches!(candidate.origin, CandidateOrigin::Placeholder(_)))
            {
                let (crate::ast::PlaceholderState::Source { slot_count }
                | crate::ast::PlaceholderState::Classified { slot_count }) = state;
                placeholder_scope(stem, *slot_count, out);
            }
        }
        Expr::LabelValue { labels, .. } => {
            for label in labels {
                expr_scope(&label.value, offset, ns, out);
            }
        }
        Expr::Elaborator { call, .. } => match call {
            ElaboratorCall::FieldAccess { receiver, .. } => expr_scope(receiver, offset, ns, out),
            ElaboratorCall::FieldUpdate { receiver, updates } => {
                expr_scope(receiver, offset, ns, out);
                for update in updates {
                    expr_scope(&update.value, offset, ns, out);
                }
            }
        },
        Expr::Ufcs { receiver, args, .. } => {
            expr_scope(receiver, offset, ns, out);
            args_scope(args, offset, ns, out);
        }
        Expr::OpChain { kind, .. } => match kind {
            OpChainKind::Normal { slots, .. } => {
                for child in slots {
                    expr_scope(child, offset, ns, out);
                }
            }
            OpChainKind::Variadic { elements, .. } => {
                for child in elements {
                    expr_scope(child, offset, ns, out);
                }
            }
        },
        Expr::StrLit { annotation, .. }
        | Expr::IntLit { annotation, .. }
        | Expr::FloatLit { annotation, .. }
        | Expr::BoolLit { annotation, .. } => {
            if ns == Namespace::Type
                && let Some(ty) = annotation
            {
                type_scope(ty, offset, out);
            }
        }
        Expr::Path { .. } | Expr::Unit { .. } => {}
        Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::EnrichedTuple { ext, .. }
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

#[cfg(test)]
mod tooling_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Module<Surface> {
        crate::pass::parser::parse(src).expect("test module parses")
    }

    /// Byte offset one past `marker`'s first occurrence in `src`.
    fn offset_after(src: &str, marker: &str) -> u32 {
        (src.find(marker).expect("marker in source") + marker.len()) as u32
    }

    fn labels(candidates: &[Candidate]) -> Vec<&str> {
        candidates.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn exact_scope_excludes_self_later_and_other_namespace() {
        let src = "module pkg/main; import other/api(Value, earlier);\nfn before() -> . { () }\nfn here(arg: .) -> . { arg }\nfn later() -> . { () }";
        let candidates = in_scope_candidates(&parse(src), offset_after(src, "{ arg"));
        let names = labels(&candidates);
        assert!(names.contains(&"arg") && names.contains(&"before") && names.contains(&"earlier"));
        assert!(
            !names.contains(&"here") && !names.contains(&"later") && !names.contains(&"Value"),
            "{names:?}"
        );
    }

    #[test]
    fn exact_scope_traverses_tuple_and_pattern_binders() {
        let src = "module pkg/main; fn f((left: ., right: .)) -> . { ((), .(inner: .) { let .(one: ., two: .) = (left, right); inner }) }";
        let candidates = in_scope_candidates(&parse(src), offset_after(src, "; inner"));
        let names = labels(&candidates);
        for name in ["left", "right", "inner", "one", "two"] {
            assert!(names.contains(&name), "missing {name}: {names:?}");
        }
        assert!(
            names.iter().all(|name| !name.starts_with("__")),
            "{names:?}"
        );
    }

    #[test]
    fn expression_containers_preserve_nested_binding_scope() {
        for container in [
            "((), INNER)",
            "build!(INNER)",
            "rec loop(INNER)",
            "scope! { INNER }",
            ".x. { let _ = x1; INNER }",
            "{ field = INNER }",
            "(INNER).?{field}",
            "().!{field = INNER}",
            "().>apply(INNER)",
            "INNER + ()",
            "[* INNER, *]",
            "if! () { INNER } else { () }",
        ] {
            let source = format!(
                "module pkg/main; fn pair(a: ., b: .) -> . {{ a }} fn zero() -> . {{ () }} op _ + _ {{ impl pair; }}; varop [* *] {{ foldl pair zero; }}; fn outer(parent: .) -> . {{ {} }}",
                container.replace("INNER", ".[A](nested: A) { nested }")
            );
            let offset = offset_after(&source, "{ nested");
            let module = parse(&source);
            let values = in_scope_candidates(&module, offset);
            assert!(
                labels(&values).contains(&"nested"),
                "{container}: {values:?}"
            );
            assert!(
                labels(&values).contains(&"parent"),
                "{container}: {values:?}"
            );
            let types = type_candidates(&module, offset);
            assert!(labels(&types).contains(&"A"), "{container}: {types:?}");
        }
    }

    #[test]
    fn neutral_bindings_are_sequential_and_do_not_escape() {
        let source = "module pkg/main; fn f(parent: .) -> . { (do! bind { let .(first: ., second: .) <- parent; let third = first; third }, parent) }";
        let module = parse(source);
        let rhs = in_scope_candidates(&module, offset_after(source, "<- parent"));
        assert!(!labels(&rhs).contains(&"first"));
        let later_rhs = in_scope_candidates(&module, offset_after(source, "= first"));
        assert!(labels(&later_rhs).contains(&"first"));
        assert!(!labels(&later_rhs).contains(&"third"));
        let tail = in_scope_candidates(&module, offset_after(source, "; third"));
        for name in ["first", "second", "third", "parent"] {
            assert!(labels(&tail).contains(&name), "{tail:?}");
        }
        let outside = in_scope_candidates(&module, offset_after(source, "}, parent"));
        assert_eq!(labels(&outside), vec!["parent"]);
    }

    #[test]
    fn structural_forall_scope_and_signature_order_are_exact() {
        let source = "module pkg/main; type A = .; fn f[B](value: [A] A)[C] -> B { value }";
        let module = parse(source);
        let inside = type_candidates(&module, offset_after(source, "[A] A"));
        assert!(labels(&inside).contains(&"B"));
        assert!(!labels(&inside).contains(&"C"));
        assert_eq!(
            inside.iter().find(|c| c.label == "A").unwrap().kind,
            CandidateKind::TypeParameter
        );
        let after = type_candidates(&module, offset_after(source, "-> B"));
        assert!(labels(&after).contains(&"C"));
        assert_eq!(
            after.iter().find(|c| c.label == "A").unwrap().kind,
            CandidateKind::Type
        );
    }

    #[test]
    fn builtin_imports_are_explicit_and_namespace_separated() {
        let source =
            "module pkg/main; import __intrinsics__; import __comptime__; fn f() -> . { () }";
        let module = parse(source);
        assert!(in_scope_candidates(&module, offset_after(source, "module pkg/main;")).is_empty());
        let offset = offset_after(source, "{ ()");
        let values = in_scope_candidates(&module, offset);
        assert!(labels(&values).contains(&"__pair__"));
        assert!(!labels(&values).contains(&"__Type__"));
        let types = type_candidates(&module, offset);
        assert!(labels(&types).contains(&"__Type__"));
        assert!(!labels(&types).contains(&"__pair__"));
    }

    #[test]
    fn existential_open_binders_do_not_leak_into_the_rhs() {
        let source = "module pkg/main; fn f(input: .) -> . { let .(<U> payload) = input; payload }";
        let module = parse(source);
        let rhs = offset_after(source, "= input");
        assert!(!labels(&type_candidates(&module, rhs)).contains(&"U"));
        assert!(!labels(&in_scope_candidates(&module, rhs)).contains(&"payload"));
        let body = offset_after(source, "; payload");
        assert!(labels(&type_candidates(&module, body)).contains(&"U"));
        assert!(labels(&in_scope_candidates(&module, body)).contains(&"payload"));
    }

    #[test]
    fn module_level_fns_follow_source_order() {
        let src = "module pkg/main;\npub fn run() -> . { () }\nfn helper() -> . { () }\n";
        let inside_run = in_scope_candidates(&parse(src), offset_after(src, "run() -> . { "));
        assert!(!labels(&inside_run).contains(&"run"));
        assert!(!labels(&inside_run).contains(&"helper"));
        let inside_helper = in_scope_candidates(&parse(src), offset_after(src, "helper() -> . { "));
        assert!(labels(&inside_helper).contains(&"run"));
        assert!(!labels(&inside_helper).contains(&"helper"));
    }

    #[test]
    fn current_rec_members_are_not_ordinary_values_in_member_bodies() {
        let src = "module pkg/main;\n\
rec(loop) {\n\
  fn first(x: .) -> . { rec second(x) };\n\
  fn second(y: .) -> . { rec first(y) }\n\
}\n";
        let candidates = in_scope_candidates(&parse(src), offset_after(src, "rec first("));
        let names = labels(&candidates);
        assert!(!names.contains(&"first"), "got: {names:?}");
        assert!(!names.contains(&"second"), "got: {names:?}");
        assert!(names.contains(&"y"), "got: {names:?}");
    }

    #[test]
    fn rec_members_are_ordinary_module_values_outside_their_group() {
        let src = "module pkg/main;\n\
rec(loop) {\n\
  fn first(x: .) -> . { rec first(x) }\n\
}\n\
fn caller(value: .) -> . { first(value) }\n";
        let candidates = in_scope_candidates(&parse(src), offset_after(src, "first(value"));
        assert!(labels(&candidates).contains(&"first"));
    }

    #[test]
    fn fn_params_and_let_locals_visible_only_inside_their_spans() {
        let src = "module pkg/main;\nfn f(x: .) -> . {\n    let y = x;\n    y\n}\n";
        let module = parse(src);
        // Inside the let body both the param and the binding are in scope.
        let in_body = in_scope_candidates(&module, offset_after(src, "    y"));
        assert!(labels(&in_body).contains(&"x"));
        assert!(labels(&in_body).contains(&"y"));
        // In the let value only the param is in scope.
        let in_value = in_scope_candidates(&module, offset_after(src, "let y = "));
        assert!(labels(&in_value).contains(&"x"));
        assert!(!labels(&in_value).contains(&"y"));
        // At module level neither is.
        let at_header = in_scope_candidates(&module, offset_after(src, "module"));
        assert!(!labels(&at_header).contains(&"x"));
        assert!(!labels(&at_header).contains(&"y"));
    }

    #[test]
    fn shadowing_keeps_only_the_innermost_binding() {
        let src = "module pkg/main;\nfn f(x: .) -> . { let x = (); x }\n";
        let candidates = in_scope_candidates(&parse(src), offset_after(src, "let x = (); "));
        let xs: Vec<&Candidate> = candidates.iter().filter(|c| c.label == "x").collect();
        assert_eq!(xs.len(), 1, "shadowed `x` must appear exactly once");
        assert_eq!(xs[0].kind, CandidateKind::Variable);
    }

    #[test]
    fn type_candidates_offer_type_params_and_type_names_not_values() {
        let src =
            "module pkg/main;\ntype Pair = .;\ntype _Pair = .;\nfn f[A][_A](x: A) -> A { x }\n";
        let candidates = type_candidates(&parse(src), offset_after(src, "x: "));
        let lbls = labels(&candidates);
        assert!(lbls.contains(&"A"));
        assert!(lbls.contains(&"Pair"));
        assert!(lbls.contains(&"_A"));
        assert!(lbls.contains(&"_Pair"));
        assert!(!lbls.contains(&"f"));
        assert!(!lbls.contains(&"x"));
        for label in ["A", "_A"] {
            let parameter = candidates.iter().find(|c| c.label == label).unwrap();
            assert_eq!(parameter.kind, CandidateKind::TypeParameter);
        }
    }

    #[test]
    fn lexical_type_candidates_recover_underscored_type_declarations_once() {
        let source = "type Pair = .; type _Pair = .; type Box = .; type _Box = .; \
                      fn f(x: Pair) -> Box { _value(_FooBar, _1Foo, FooBar, __Foo) }";
        let candidates = lexical_type_candidates(source, source.len() as u32);
        assert_eq!(labels(&candidates), vec!["Pair", "_Pair", "Box", "_Box"]);
    }

    #[test]
    fn type_candidates_follow_source_order_outside_recursive_scopes() {
        let src = "module pkg/main;\n\
type Earlier = .;\n\
newtype Current : Later { constructor mk; projector un; };\n\
type Later = .;\n";
        let candidates = type_candidates(&parse(src), offset_after(src, "Current : "));
        let names = labels(&candidates);
        assert!(names.contains(&"Earlier"), "got: {names:?}");
        assert!(!names.contains(&"Current"), "got: {names:?}");
        assert!(!names.contains(&"Later"), "got: {names:?}");
    }

    #[test]
    fn type_candidates_include_only_explicit_recursive_singleton_scope() {
        let src = "module pkg/main;\n\
rec newtype Tree[A] : A & Tree(A) { constructor mk; projector un; };\n";
        let candidates = type_candidates(&parse(src), offset_after(src, "Tree[A] : "));
        let names = labels(&candidates);
        assert!(names.contains(&"Tree"), "got: {names:?}");
        assert!(names.contains(&"A"), "got: {names:?}");
    }

    #[test]
    fn type_candidates_include_every_mutual_group_head_and_respect_binder_shadowing() {
        let src = "module pkg/main;\n\
rec {\n\
  type A = B;\n\
  newtype B[A] : A & A { constructor mk; projector un; };\n\
}\n";
        let candidates = type_candidates(&parse(src), offset_after(src, "B[A] : "));
        let names = labels(&candidates);
        assert!(names.contains(&"A"), "got: {names:?}");
        assert!(names.contains(&"B"), "got: {names:?}");
        let a = candidates
            .iter()
            .find(|candidate| candidate.label == "A")
            .unwrap();
        assert_eq!(a.kind, CandidateKind::TypeParameter);
    }

    #[test]
    fn type_candidates_include_recursive_label_alias_and_generated_heads() {
        let src = "module pkg/main;\n\
rec labels Tree[A] = { leaf: A } | { branch: Tree(A) & Tree(A) };\n";
        let candidates = type_candidates(&parse(src), offset_after(src, "branch: "));
        let names = labels(&candidates);
        for expected in ["A", "Tree", "Leaf", "Branch"] {
            assert!(
                names.contains(&expected),
                "missing {expected}; got: {names:?}"
            );
        }
    }

    #[test]
    fn type_candidates_include_only_earlier_unmarked_label_heads_inside_the_declaration() {
        let src = "module pkg/main;\n\
labels Tree = { leaf: . } | { branch: Leaf & Tree };\n\
type After = Leaf;\n";
        let inside = type_candidates(&parse(src), offset_after(src, "branch: "));
        let inside_names = labels(&inside);
        assert!(
            inside_names.contains(&"Leaf"),
            "missing prior generated label head; got: {inside_names:?}"
        );
        for excluded in ["Tree", "Branch", "After"] {
            assert!(
                !inside_names.contains(&excluded),
                "unexpected {excluded}; got: {inside_names:?}"
            );
        }
        let after = type_candidates(&parse(src), offset_after(src, "After = "));
        let after_names = labels(&after);
        for expected in ["Tree", "Leaf", "Branch"] {
            assert!(
                after_names.contains(&expected),
                "missing {expected}; got: {after_names:?}"
            );
        }
    }

    #[test]
    fn lexical_type_candidates_use_declarations_not_arbitrary_capitalized_words() {
        let source = "module pkg/main; type Earlier = .; fn f(x: Pair) -> Box { \"StringLeak\" } // CommentLeak\n";
        let candidates = lexical_type_candidates(source, source.len() as u32);
        assert_eq!(labels(&candidates), vec!["Earlier"]);
    }

    #[test]
    fn lexical_type_candidates_recover_explicit_recursive_scope() {
        let source =
            "module pkg/main; rec { type A = B; newtype B : A { constructor mk; projector un; };";
        let cursor = source.find("A = ").unwrap() as u32 + 4;
        let candidates = lexical_type_candidates(source, cursor);
        assert_eq!(labels(&candidates), vec!["A", "B"]);
    }

    #[test]
    fn lexical_type_candidates_recover_complete_recursive_label_scope_without_payload_leaks() {
        for (source, expected, excluded) in [
            (
                "module pkg/main; import pkg/dep(Imported); rec labels { list[A] <U> : Pair(A, bogus[B]: ",
                vec!["A", "U", "List", "Imported"],
                vec!["B", "Bogus"],
            ),
            (
                "module pkg/main; import pkg/dep(Imported); import pkg/other as other; rec labels Tree[A] = { list[B] <U> : Pair(B, bogus[C]: ",
                vec!["A", "B", "U", "Tree", "List", "Imported"],
                vec!["C", "Bogus", "other"],
            ),
            (
                "module pkg/main; rec labels { list[*F,A][B]: ",
                vec!["F", "A", "B", "List"],
                vec![],
            ),
            (
                "module pkg/main; rec labels { first[A]: [X, bogus[B]: ",
                vec!["A", "First"],
                vec!["X", "B", "Bogus"],
            ),
        ] {
            let candidates = lexical_type_candidates(source, source.len() as u32);
            let names = labels(&candidates);
            for expected in expected {
                assert!(
                    names.contains(&expected),
                    "missing {expected} for {source:?}; got: {names:?}"
                );
            }
            for excluded in excluded {
                assert!(
                    !names.contains(&excluded),
                    "payload text {excluded} leaked; got: {names:?}"
                );
            }
        }

        let malformed = "module pkg/main; rec labels { malformed[A B]: ";
        assert!(lexical_type_candidates(malformed, malformed.len() as u32).is_empty());
    }

    #[test]
    fn lexical_type_candidates_partition_label_entries_across_broken_payloads() {
        for (source, marker, expected, excluded) in [
            (
                "module pkg/main; rec labels { first[A]: 123, second[B]: . };",
                "first[A]: ",
                vec!["A", "First", "Second"],
                vec!["B"],
            ),
            (
                "module pkg/main; rec labels { first[A]: ., second[B]: 123 };",
                "second[B]: ",
                vec!["B", "First", "Second"],
                vec!["A"],
            ),
            (
                "module pkg/main; labels { first[A]: ., second[B]: 123 };",
                "second[B]: ",
                vec!["B", "First"],
                vec!["A", "Second"],
            ),
        ] {
            let byte_offset = (source.find(marker).expect("marker") + marker.len()) as u32;
            let candidates = lexical_type_candidates(source, byte_offset);
            let names = labels(&candidates);
            for expected in expected {
                assert!(
                    names.contains(&expected),
                    "missing {expected}; got: {names:?}"
                );
            }
            for excluded in excluded {
                assert!(
                    !names.contains(&excluded),
                    "out-of-scope {excluded} leaked for {source:?}; got: {names:?}"
                );
            }
        }
    }

    #[test]
    fn lexical_type_candidates_preserve_reuse_and_shadowing_semantics() {
        for (source, expected, excluded) in [
            (
                "module pkg/main; labels { first: _, second[A]: ",
                vec!["A"],
                vec!["First", "Second"],
            ),
            (
                "module pkg/main; rec labels { first: _, second[A]: ",
                vec!["A", "Second"],
                vec!["First"],
            ),
        ] {
            let candidates = lexical_type_candidates(source, source.len() as u32);
            let names = labels(&candidates);
            for expected in expected {
                assert!(names.contains(&expected), "missing {expected}: {names:?}");
            }
            for excluded in excluded {
                assert!(!names.contains(&excluded), "leaked {excluded}: {names:?}");
            }
        }

        let shadowed = "module pkg/main; rec labels Tree[A] = { list[A]: ";
        let candidates = lexical_type_candidates(shadowed, shadowed.len() as u32);
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.label == "A")
                .count(),
            1,
            "the active entry binder shadows the alias binder"
        );

        let active_reuse = "module pkg/main; rec labels { first: _";
        let candidates = lexical_type_candidates(active_reuse, active_reuse.len() as u32);
        let names = labels(&candidates);
        assert!(
            !names.contains(&"First"),
            "an active reuse marker became a fresh generated head: {names:?}"
        );
    }

    #[test]
    fn lexical_type_candidates_ignore_an_unrelated_later_malformed_item() {
        for (source, marker, expected) in [
            (
                concat!(
                    "module pkg/main; type Earlier = .; ",
                    "rec newtype Box[A] : 123 { constructor make; projector un; }; ",
                    "type Broken = 123;",
                ),
                "Box[A] : ",
                vec!["A", "Box", "Earlier"],
            ),
            (
                concat!(
                    "module pkg/main; type Earlier = .; rec { ",
                    "type A[X] = 123; ",
                    "newtype B : A(.) { constructor make; projector un; }; ",
                    "} type Broken = 123;",
                ),
                "A[X] = ",
                vec!["X", "A", "B", "Earlier"],
            ),
        ] {
            let cursor = offset_after(source, marker);
            let candidates = lexical_type_candidates(source, cursor);
            let names = labels(&candidates);
            for expected in expected {
                assert!(names.contains(&expected), "missing {expected}: {names:?}");
            }
            assert!(
                !names.contains(&"Broken"),
                "later malformed declaration leaked: {names:?}"
            );
        }
    }

    #[test]
    fn lexical_type_candidates_remove_item_shaped_broken_group_payload_text() {
        let source = concat!(
            "module pkg/main; rec { ",
            "newtype A[X] : type Fake = .; ",
            "newtype B : . { constructor mk; projector un; }; ",
            "}",
        );
        let cursor = offset_after(source, "newtype A[X] : ");
        let candidates = lexical_type_candidates(source, cursor);
        let names = labels(&candidates);
        for expected in ["X", "A", "B"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        assert!(
            !names.contains(&"Fake"),
            "broken payload text became a recursive member: {names:?}"
        );
    }

    #[test]
    fn lexical_type_candidates_preserve_unmarked_source_order_and_active_binders() {
        for (source, expected, excluded) in [
            (
                "module pkg/main; import pkg/dep(Imported); type Earlier = .; newtype Box[A] <E> : ",
                vec!["A", "E", "Earlier", "Imported"],
                vec!["Box"],
            ),
            (
                "module pkg/main; import pkg/dep(Imported); labels Tree[A] = { first[X]: ., list[B] <E> : ",
                vec!["A", "B", "E", "First", "Imported"],
                vec!["Tree", "List", "X"],
            ),
            (
                "module pkg/main; type Earlier = .; newtype Box[A]<E>:",
                vec!["A", "E", "Earlier"],
                vec!["Box"],
            ),
            (
                "module pkg/main; type Earlier = .; type Box[A]=",
                vec!["A", "Earlier"],
                vec!["Box"],
            ),
            (
                "module pkg/main; type Earlier = .; rec newtype Box[A]<E>:",
                vec!["A", "E", "Box", "Earlier"],
                vec![],
            ),
            (
                "module pkg/main; type Earlier = .; labels Tree[A]={first[X]: .,list[B]<E>:",
                vec!["A", "B", "E", "First", "Earlier"],
                vec!["Tree", "List", "X"],
            ),
        ] {
            let candidates = lexical_type_candidates(source, source.len() as u32);
            let names = labels(&candidates);
            for expected in expected {
                assert!(
                    names.contains(&expected),
                    "missing {expected} for {source:?}; got: {names:?}"
                );
            }
            for excluded in excluded {
                assert!(
                    !names.contains(&excluded),
                    "out-of-scope {excluded} leaked for {source:?}; got: {names:?}"
                );
            }
        }
    }

    #[test]
    fn lexical_type_candidates_repair_payload_before_an_unclosed_newtype_body() {
        let source = concat!(
            "module pkg/main; type Earlier = .; ",
            "newtype Box[A] : Pair { constructor make; projector",
        );
        let cursor = offset_after(source, "Box[A] : ");
        let candidates = lexical_type_candidates(source, cursor);
        let names = labels(&candidates);
        for expected in ["A", "Earlier"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        assert!(!names.contains(&"Box"), "unfinished head leaked: {names:?}");
    }

    #[test]
    fn lexical_type_candidates_recover_comptime_imports_and_group_member_binders() {
        let source = "module pkg/main; import __comptime__; type Earlier = .; rec { type A = B; newtype B[X] <E> : ";
        let candidates = lexical_type_candidates(source, source.len() as u32);
        let names = labels(&candidates);
        for expected in ["X", "E", "A", "B", "Earlier", "Comptime_bool"] {
            assert!(
                names.contains(&expected),
                "missing {expected}; got: {names:?}"
            );
        }
    }
}
