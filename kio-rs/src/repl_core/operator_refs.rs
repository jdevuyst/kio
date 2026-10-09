//! Operator references use the retained Surface grammar, before folding turns
//! an application into its ordinary callable. Callable identity cannot tell
//! two operators using the same implementation apart.

use crate::ast::{
    CallArg, ElaboratorCall, Expr, Item, Module, OpChainKind, OpPart, OperatorGrammar, Surface,
};
use crate::pass::lexer::{TokenKind, lex};
use crate::span::Span;

pub(super) fn uses(module: &Module<Surface>, source: &str, grammar: &OperatorGrammar) -> Vec<Span> {
    let Ok(tokens) = lex(source) else {
        return Vec::new();
    };
    let mut pending = Vec::new();
    for item in &module.items {
        match item {
            Item::FnDef(def) => pending.push(&def.body),
            Item::RecGroup(group, _) => pending.extend(group.members.iter().map(|def| &def.body)),
            Item::Equiv(equiv, _) => pending.extend(equiv.terms.iter().map(|term| &term.body)),
            Item::TypeAlias(_)
            | Item::TypeRecGroup(_)
            | Item::Newtype(_)
            | Item::LiteralAlias(_, _)
            | Item::Labels(_, _)
            | Item::LabelForward(_, _)
            | Item::HostType(_)
            | Item::HostFn(_)
            | Item::Elaborator(_, _)
            | Item::Op(_, _)
            | Item::VariadicOperator(_, _) => {}
        }
    }
    let mut hits = Vec::new();
    while let Some(expr) = pending.pop() {
        match expr {
            Expr::OpChain { kind, .. } => {
                let (actual, children, first) = match kind {
                    OpChainKind::Normal { pattern, slots } => (
                        OperatorGrammar::fixed(pattern),
                        slots,
                        pattern.iter().find_map(|part| match part {
                            OpPart::Token { content, .. } => Some(content.as_str()),
                            _ => None,
                        }),
                    ),
                    OpChainKind::Variadic {
                        open_tokens,
                        close_tokens,
                        elements,
                        ..
                    } => (
                        OperatorGrammar::Variadic {
                            open: open_tokens.clone(),
                            close: close_tokens.clone(),
                        },
                        elements,
                        open_tokens.first().map(String::as_str),
                    ),
                };
                if &actual == grammar {
                    // Pattern spans belong to the declaration. Locate the written
                    // operator token in this application, outside all operand spans.
                    let span = expr.span();
                    if let Some(token) = tokens.iter().find(|token| {
                        token.span.start >= span.start && token.span.end <= span.end
                            && matches!(&token.kind, TokenKind::SymbolRun(text) if Some(text.as_str()) == first)
                            && !children.iter().any(|child| {
                                let child = child.span();
                                child.start <= token.span.start && token.span.end <= child.end
                            })
                    }) { hits.push(token.span); }
                }
                pending.extend(children);
            }
            Expr::Call { callee, args, .. } => {
                pending.push(callee);
                values(args, &mut pending);
            }
            Expr::RecCall { args, .. } | Expr::UserElaborator { args, .. } => {
                values(args, &mut pending)
            }
            Expr::Ufcs { receiver, args, .. } => {
                pending.push(receiver);
                values(args, &mut pending);
            }
            Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => pending.push(body),
            Expr::Let { value, body, .. }
            | Expr::RowLet { value, body, .. }
            | Expr::Seq { value, body, .. } => {
                pending.extend([value.as_ref(), body.as_ref()]);
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
            Expr::BlockCall { prefix, blocks, .. } => {
                pending.extend(prefix);
                pending.extend(
                    blocks
                        .iter()
                        .flat_map(|block| block.items.iter().map(|item| item.value())),
                );
            }
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
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
    hits
}

fn values<'a>(args: &'a [CallArg<Surface>], pending: &mut Vec<&'a Expr<Surface>>) {
    pending.extend(args.iter().filter_map(|arg| match arg {
        CallArg::Value(value) => Some(value),
        CallArg::Type(_) => None,
    }));
}
