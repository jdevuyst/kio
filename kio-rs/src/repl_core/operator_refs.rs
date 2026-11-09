//! Operator references use the retained Surface grammar, before folding turns
//! an application into its ordinary callable. Callable identity cannot tell
//! two operators using the same implementation apart.

use crate::ast::{CallArg, Expr, Item, Module, OpChainKind, OpPart, OperatorGrammar, Surface};
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
            | Item::HostType(_)
            | Item::HostFn(_)
            | Item::Macro(_, _)
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
                        Some("["),
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
            Expr::RecCall { stages, .. } | Expr::UserMacro { stages, .. } => {
                for stage in stages {
                    values(&stage.args, &mut pending);
                }
            }
            Expr::Ufcs {
                receiver,
                callee,
                args,
                ..
            } => {
                pending.push(receiver);
                pending.push(callee);
                values(args, &mut pending);
            }
            Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => pending.push(body),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                pending.extend([value.as_ref(), body.as_ref()]);
            }
            Expr::Tuple { items, .. } => pending.extend(items),
            Expr::BlockCall { prefix, blocks, .. } => {
                for stage in prefix {
                    values(&stage.args, &mut pending);
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::parser::{parse, parse_operator_grammar};

    #[test]
    fn variadic_alias_uses_follow_structural_openings() {
        for (grammar, expression) in [
            ("varop []", "[(), ([])]"),
            ("varop [% %]", "[% (), ([% () %]) %]"),
            ("varop [! !]", "[! ([!()!]) !]"),
            ("varop [** **]", "[ ** ([ // edge\n ** () ** ]) ** ]"),
            ("varop [$? ?$]", "[$? ([$? () ?$]) ?$]"),
        ] {
            let source = format!(
                "module app; import ops(varop [* *] as {}); fn run() {{ {expression} }}",
                grammar.strip_prefix("varop ").unwrap()
            );
            let module = parse(&source).unwrap();
            let local = parse_operator_grammar(grammar).unwrap();
            let hits = uses(&module, &source, &local);
            for span in &hits {
                assert_eq!(&source[span.start as usize..span.end as usize], "[");
            }
            let mut actual: Vec<_> = hits.into_iter().map(|span| span.start as usize).collect();
            actual.sort_unstable();
            let body_start = source.find(expression).unwrap();
            let expected: Vec<_> = expression
                .match_indices('[')
                .map(|(offset, _)| body_start + offset)
                .collect();
            assert_eq!(actual, expected, "{source}");
            assert!(
                uses(
                    &module,
                    &source,
                    &parse_operator_grammar("varop [* *]").unwrap()
                )
                .is_empty(),
                "the source grammar is not the local alias: {source}"
            );
        }
    }

    #[test]
    fn variadic_uses_ignore_brackets_in_strings_and_comments() {
        let source = "module app; import ops(varop [% %]);
            fn run() { [% \"[\", // [
                \"[]\" %] }";
        let module = parse(source).unwrap();
        let hits = uses(
            &module,
            source,
            &parse_operator_grammar("varop [% %]").unwrap(),
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].start as usize, source.find("[% \"[").unwrap());
        assert_eq!(&source[hits[0].start as usize..hits[0].end as usize], "[");
    }

    #[test]
    fn operator_uses_keep_exact_local_grammar_identity() {
        let source = "module app;
            import ops(op _ + _ as _ ++ _, varop [* *] as [% %], varop [!! !!] as [! !]);
            fn run() { [% ([! (() ++ ()) ++ () !]) %] }";
        let module = parse(source).unwrap();
        for (grammar, spelling) in [("varop [% %]", "[%"), ("varop [! !]", "[!")] {
            let hits = uses(&module, source, &parse_operator_grammar(grammar).unwrap());
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].start as usize, source.rfind(spelling).unwrap());
        }
        let mut fixed: Vec<_> = uses(
            &module,
            source,
            &parse_operator_grammar("op _ ++ _").unwrap(),
        )
        .into_iter()
        .map(|span| span.start as usize)
        .collect();
        fixed.sort_unstable();
        let expected: Vec<_> = source
            .match_indices("++")
            .skip(1)
            .map(|(offset, _)| offset)
            .collect();
        assert_eq!(fixed, expected);
    }
}
