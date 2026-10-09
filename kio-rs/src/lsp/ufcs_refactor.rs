//! Source-preserving inversion of an authenticated ordinary call spine.

use std::collections::HashMap;

use lsp_types::{Range, TextEdit};

use crate::ast::{
    CallArg, ElaboratorCall, Expr, Item, Lowered, Module, OpChainKind, Phase, Surface, UfcsFlavor,
};
use crate::cmd::check::LspAnalysis;
use crate::pass::typecheck_core::apply::{
    CallArgRef, call_arg_refs, splice_ufcs_arg_refs_interned,
};
use crate::pass::typecheck_core::{InternedType, ModuleEnv, call_arg_type_slot_candidate};
use crate::pass::typecheck_full::{PositionIndex, ResolvedBinder};
use crate::span::Span;

use super::positions::{LineIndex, LspPosition};

mod source;
#[cfg(test)]
mod tests;

pub(super) fn edit(
    source: &str,
    module: &Module<Surface>,
    analysis: &LspAnalysis,
    module_path: &str,
    selection: Range,
) -> Option<TextEdit> {
    let lines = LineIndex::new(source);
    let offset = |position: lsp_types::Position| {
        let position = LspPosition {
            line: position.line,
            character: position.character,
        };
        let offset = lines.position_to_offset(position);
        (lines.to_position(offset) == position).then_some(offset)
    };
    let selected = Span::new(offset(selection.start)?, offset(selection.end)?);
    if selected.end < selected.start {
        return None;
    }
    let written_calls = calls(module);
    let by_span: HashMap<_, _> = written_calls
        .iter()
        .enumerate()
        .map(|(i, call)| (call.span(), i))
        .collect();
    if by_span.len() != written_calls.len() {
        return None;
    }
    let selected_call = written_calls
        .iter()
        .enumerate()
        .filter(|(_, call)| {
            let span = call.span();
            if selected.start == selected.end {
                span.start <= selected.start && selected.end <= span.end
            } else {
                span == selected
            }
        })
        .min_by_key(|(_, call)| call.span().end - call.span().start)?
        .0;
    let index = &analysis.position_index;
    let selected_identity = identity(index, module_path, written_calls[selected_call])?;
    let mut parents = HashMap::new();
    for (parent, call) in written_calls.iter().enumerate() {
        let (_, args) = parts(call)?;
        for slot in boundaries(args) {
            if let CallArg::Value(child) = &args[slot]
                && let Some(child) = by_span.get(&child.span())
                && parents.insert(*child, parent).is_some()
            {
                return None;
            }
        }
    }
    let child = |node: usize| -> Option<Result<(usize, usize, UfcsFlavor), ()>> {
        let (_, args) = parts(written_calls[node])?;
        let mut found = None;
        for slot in boundaries(args) {
            let CallArg::Value(value) = &args[slot] else {
                unreachable!()
            };
            let Some(&candidate) = by_span.get(&value.span()) else {
                continue;
            };
            let Some(candidate_identity) = identity(index, module_path, written_calls[candidate])
            else {
                return Some(Err(()));
            };
            if !same_callable(selected_identity, candidate_identity) {
                continue;
            }
            if found.is_some() {
                return Some(Err(()));
            }
            let flavor = if Some(slot) == boundaries(args).first().copied() {
                UfcsFlavor::ReceiverFirst
            } else {
                UfcsFlavor::ReceiverLast
            };
            found = Some(Ok((candidate, slot, flavor)));
        }
        found
    };
    let mut root = selected_call;
    if selected.start == selected.end {
        while let Some(&parent) = parents.get(&root) {
            let parent_identity = identity(index, module_path, written_calls[parent])?;
            if !same_callable(selected_identity, parent_identity) {
                break;
            }
            match child(parent) {
                Some(Ok((candidate, _, _))) if candidate == root => root = parent,
                Some(Err(())) => return None,
                _ => break,
            }
        }
    }
    let mut chain = Vec::new();
    let mut current = root;
    loop {
        match child(current) {
            Some(Ok((next, slot, flavor))) => {
                chain.push((written_calls[current], slot, flavor));
                current = next;
            }
            Some(Err(())) => return None,
            None => break,
        }
    }
    let flavor = chain.last()?.2;
    let (_, innermost_args) = parts(written_calls[current])?;
    let edges = boundaries(innermost_args);
    let slot = if flavor.inserts_first() {
        *edges.first()?
    } else {
        *edges.last()?
    };
    chain.push((written_calls[current], slot, flavor));

    let entry = analysis.root_package_lowered.module(module_path)?;
    let lowered_calls = calls(&entry.module);
    let mut lowered = HashMap::new();
    for call in lowered_calls {
        lowered
            .entry(call.span())
            .and_modify(|found| *found = None)
            .or_insert(Some(call));
    }
    let env = ModuleEnv::build(
        &entry.module,
        None,
        None,
        Some(&analysis.root_package_lowered),
    )
    .ok()?;
    for (written, slot, flavor) in &chain {
        let typed = lowered.get(&written.span()).copied().flatten()?;
        prove(written, typed, *slot, *flavor, index, module_path, &env)?;
    }
    let (span, new_text) = source::render(source, module, &chain)?;
    let range = lines.to_range(span);
    Some(TextEdit {
        range: Range {
            start: lsp_types::Position::new(range.start.line, range.start.character),
            end: lsp_types::Position::new(range.end.line, range.end.character),
        },
        new_text,
    })
}

fn parts<P: Phase>(expr: &Expr<P>) -> Option<(&Expr<P>, &[CallArg<P>])> {
    match expr {
        Expr::Call { callee, args, .. } if matches!(**callee, Expr::Path { .. }) => {
            Some((callee, args))
        }
        _ => None,
    }
}

fn boundaries<P: Phase>(args: &[CallArg<P>]) -> Vec<usize> {
    let first = args.iter().position(|arg| matches!(arg, CallArg::Value(_)));
    let last = args
        .iter()
        .rposition(|arg| matches!(arg, CallArg::Value(_)));
    match (first, last) {
        (Some(first), Some(last)) if first != last => vec![first, last],
        (Some(first), _) => vec![first],
        _ => Vec::new(),
    }
}

fn identity<'a>(
    index: &'a PositionIndex,
    module: &str,
    call: &Expr<Surface>,
) -> Option<&'a ResolvedBinder> {
    let (callee @ Expr::Path { segments, .. }, _) = parts(call)? else {
        return None;
    };
    // Discovery must authenticate boundaries too: a conflicted last writer
    // cannot make an ancestor look different and authorize a shorter prefix.
    index
        .authenticated_callee(module, callee.span(), segments.last()?.span, call.span())
        .map(|(_, _, binder)| binder)
}

fn same_callable(left: &ResolvedBinder, right: &ResolvedBinder) -> bool {
    match (left, right) {
        (
            ResolvedBinder::Local {
                decl_span: Some(left),
                ..
            },
            ResolvedBinder::Local {
                decl_span: Some(right),
                ..
            },
        ) => left == right,
        _ => left == right,
    }
}

fn prove(
    source: &Expr<Surface>,
    lowered: &Expr<Lowered>,
    slot: usize,
    flavor: UfcsFlavor,
    index: &PositionIndex,
    module: &str,
    env: &ModuleEnv<'_, Lowered>,
) -> Option<()> {
    let (sc, sa) = parts(source)?;
    let (lc, la) = parts(lowered)?;
    if sc.span() != lc.span()
        || sa.len() != la.len()
        || sa
            .iter()
            .zip(la)
            .any(|(s, l)| call_arg_type_slot_candidate(s) != call_arg_type_slot_candidate(l))
    {
        return None;
    }
    let Expr::Path { segments, .. } = lc else {
        return None;
    };
    let (ty, scope, _) =
        index.authenticated_callee(module, lc.span(), segments.last()?.span, lowered.span())?;
    let locals = scope.names();
    let mut ctx = env.alias_ctx();
    ctx.binder_locals = Some(&locals);
    let original = call_arg_refs(la);
    let CallArg::Value(receiver) = &la[slot] else {
        return None;
    };
    let rest = original
        .iter()
        .enumerate()
        .filter_map(|(i, arg)| (i != slot).then_some(*arg))
        .collect::<Vec<_>>();
    let recovered = splice_ufcs_arg_refs_interned(
        &InternedType::fresh_with_identity(ty.clone(), true),
        receiver,
        &rest,
        &rest,
        flavor,
        lowered.span(),
        &ctx,
    )
    .ok()?;
    (recovered.len() == original.len()
        && recovered
            .iter()
            .zip(&original)
            .all(|(left, right)| match (left, right) {
                (CallArgRef::Value(left), CallArgRef::Value(right)) => std::ptr::eq(*left, *right),
                (CallArgRef::Type(left), CallArgRef::Type(right)) => std::ptr::eq(*left, *right),
                _ => false,
            }))
    .then_some(())
}

fn calls<P: Phase<FnBody = Expr<P>>>(module: &Module<P>) -> Vec<&Expr<P>> {
    let mut pending = Vec::new();
    for item in &module.items {
        match item {
            Item::FnDef(def) => pending.push(&def.body),
            Item::RecGroup(group, _) => pending.extend(group.members.iter().map(|def| &def.body)),
            Item::Equiv(equiv, _) => pending.extend(equiv.terms.iter().map(|term| &term.body)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    while let Some(expr) = pending.pop() {
        if parts(expr).is_some() {
            found.push(expr);
        }
        match expr {
            Expr::Call { callee, args, .. } => {
                pending.push(callee);
                pending.extend(args.iter().filter_map(|arg| {
                    if let CallArg::Value(value) = arg {
                        Some(value)
                    } else {
                        None
                    }
                }));
            }
            Expr::RecCall { args, .. } | Expr::UserElaborator { args, .. } => {
                pending.extend(args.iter().filter_map(|arg| {
                    if let CallArg::Value(value) = arg {
                        Some(value)
                    } else {
                        None
                    }
                }))
            }
            Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => pending.push(body),
            Expr::Let { value, body, .. }
            | Expr::RowLet { value, body, .. }
            | Expr::Seq { value, body, .. } => pending.extend([&**value, &**body]),
            Expr::Tuple { items, .. } => pending.extend(items),
            Expr::LabelValue { labels, .. } => {
                pending.extend(labels.iter().map(|label| &label.value))
            }
            Expr::BlockCall { prefix, blocks, .. } => {
                pending.extend(prefix);
                pending.extend(
                    blocks
                        .iter()
                        .flat_map(|block| block.items.iter().map(|item| item.value())),
                );
            }
            Expr::Elaborator { call, .. } => match call {
                ElaboratorCall::FieldAccess { receiver, .. } => pending.push(receiver),
                ElaboratorCall::FieldUpdate {
                    receiver, updates, ..
                } => {
                    pending.push(receiver);
                    pending.extend(updates.iter().map(|update| &update.value));
                }
            },
            Expr::Ufcs { receiver, args, .. } => {
                pending.push(receiver);
                pending.extend(args.iter().filter_map(|arg| {
                    if let CallArg::Value(value) = arg {
                        Some(value)
                    } else {
                        None
                    }
                }));
            }
            Expr::OpChain { kind, .. } => match kind {
                OpChainKind::Normal { slots, .. } => pending.extend(slots),
                OpChainKind::Variadic { elements, .. } => pending.extend(elements),
            },
            Expr::RecOrder { plan, .. } => pending.extend([&*plan.value, &*plan.body]),
            Expr::RecQuote { plan, .. } => pending.extend(plan.expressions()),
            _ => {}
        }
    }
    found
}
