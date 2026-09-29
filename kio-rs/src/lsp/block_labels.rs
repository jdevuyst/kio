//! Source positions for trailing labels selected by ordinary elaborator resolution.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::ast::{CallArg, ElaboratorCall, Expr, Item, Module, OpChainKind, Surface};
use crate::pass::surface_registry::ResolvedBlockDeclaration;
use crate::pass::typecheck_full::{PositionIndex, ResolvedBinder};

/// Borrow the retained source tree once. Header facts contain only the exact
/// declarations selected by the validated import scope, never provider bodies.
pub(crate) fn record_module(
    module: &Module<Surface>,
    module_path: &str,
    declarations: &BTreeMap<&str, &ResolvedBlockDeclaration>,
    file_to_module: &BTreeMap<PathBuf, String>,
    index: &mut PositionIndex,
    mut is_cancelled: impl FnMut() -> bool,
) -> Option<()> {
    let source_module = module.path.segments.join("/");
    for fact in declarations.values() {
        if is_cancelled() {
            return None;
        }
        let owner = file_to_module.get(&fact.file)?;
        let head = ResolvedBinder::Fn {
            module_path: owner.clone(),
            name: fact.name.clone(),
        };
        let local = fact.module == source_module;
        if local {
            index.record_binder_declaration(module_path, fact.span, head);
        } else {
            index.record_declaration_site_only(owner, fact.span, &head);
        }
        for (ordinal, (exposure, label)) in fact.blocks.iter().enumerate() {
            if is_cancelled() {
                return None;
            }
            let Some(label) = label else { continue };
            let binder = ResolvedBinder::BlockLabel {
                module_path: owner.clone(),
                elaborator: fact.name.clone(),
                ordinal,
                name: label.name.clone(),
                exposure: *exposure,
            };
            if local {
                index.record_binder_declaration(module_path, label.span, binder);
            } else {
                index.record_declaration_site_only(owner, label.span, &binder);
            }
        }
    }

    for import in &module.imports {
        let crate::ast::ImportKind::Selective { items, .. } = &import.kind else {
            continue;
        };
        for item in items {
            if is_cancelled() {
                return None;
            }
            let crate::ast::ImportItem::Name { name, span, .. } = item else {
                continue;
            };
            let Some(fact) = declarations.get(name.as_str()) else {
                continue;
            };
            let owner = file_to_module.get(&fact.file)?;
            index.record_binder(
                module_path,
                *span,
                ResolvedBinder::Fn {
                    module_path: owner.clone(),
                    name: fact.name.clone(),
                },
            );
        }
    }

    let mut pending = Vec::new();
    for item in &module.items {
        match item {
            Item::FnDef(def) => pending.push(&def.body),
            Item::RecGroup(group, _) => pending.extend(group.members.iter().map(|def| &def.body)),
            Item::Equiv(equiv, _) => pending.extend(equiv.terms.iter().map(|term| &term.body)),
            Item::TypeAlias(_)
            | Item::LiteralAlias(_, _)
            | Item::Newtype(_)
            | Item::Labels(_, _)
            | Item::LabelForward(_, _)
            | Item::Elaborator(_, _)
            | Item::TypeRecGroup(_)
            | Item::HostType(_)
            | Item::HostFn(_)
            | Item::Op(_, _)
            | Item::VariadicOperator(_, _) => {}
        }
    }
    while let Some(expr) = pending.pop() {
        if is_cancelled() {
            return None;
        }
        match expr {
            Expr::BlockCall {
                head,
                prefix,
                blocks,
                ..
            } => {
                if let Some(fact) = declarations.get(head.as_str()) {
                    let owner = file_to_module.get(&fact.file)?;
                    index.record_binder(
                        module_path,
                        head.span,
                        ResolvedBinder::Fn {
                            module_path: owner.clone(),
                            name: fact.name.clone(),
                        },
                    );
                    for (ordinal, (block, (exposure, label))) in
                        blocks.iter().zip(&fact.blocks).enumerate()
                    {
                        if is_cancelled() {
                            return None;
                        }
                        if let (Some(written), Some(declared)) = (&block.label, label)
                            && written.name == declared.name
                        {
                            index.record_binder(
                                module_path,
                                written.span,
                                ResolvedBinder::BlockLabel {
                                    module_path: owner.clone(),
                                    elaborator: fact.name.clone(),
                                    ordinal,
                                    name: declared.name.clone(),
                                    exposure: *exposure,
                                },
                            );
                        }
                    }
                }
                pending.extend(prefix);
                pending.extend(
                    blocks
                        .iter()
                        .flat_map(|block| block.items.iter().map(|item| item.value())),
                );
            }
            Expr::Call { callee, args, .. } => {
                pending.push(callee);
                push_args(&mut pending, args);
            }
            Expr::RecCall { args, .. } | Expr::UserElaborator { args, .. } => {
                push_args(&mut pending, args)
            }
            Expr::Ufcs { receiver, args, .. } => {
                pending.push(receiver);
                push_args(&mut pending, args);
            }
            Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => pending.push(body),
            Expr::Let { value, body, .. }
            | Expr::RowLet { value, body, .. }
            | Expr::Seq { value, body, .. } => {
                pending.push(value);
                pending.push(body);
            }
            Expr::Tuple { items, .. } => pending.extend(items),
            Expr::LabelValue { labels, .. } => {
                pending.extend(labels.iter().map(|label| &label.value))
            }
            Expr::Elaborator { call, .. } => match call {
                ElaboratorCall::FieldAccess { receiver, .. } => pending.push(receiver),
                ElaboratorCall::FieldUpdate { receiver, updates } => {
                    pending.push(receiver);
                    pending.extend(updates.iter().map(|update| &update.value));
                }
            },
            Expr::OpChain { kind, .. } => match kind {
                OpChainKind::Normal { slots, .. } => pending.extend(slots),
                OpChainKind::Variadic { elements, .. } => pending.extend(elements),
            },
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::StrLit { .. }
            | Expr::BoolLit { .. } => {}
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
    Some(())
}

fn push_args<'a>(pending: &mut Vec<&'a Expr<Surface>>, args: &'a [CallArg<Surface>]) {
    pending.extend(args.iter().filter_map(|arg| match arg {
        CallArg::Value(value) => Some(value),
        CallArg::Type(_) => None,
    }));
}
