use std::collections::HashMap;

use crate::ast::{CallArg, Expr, Module, Surface, UfcsFlavor};
use crate::pass::lexer::{Token, TokenKind, Trivia};
use crate::pass::parser::{self, ToolingSyntax};
use crate::pass::tree_skeleton::{GroupKind, SkeletonNode};
use crate::span::Span;

struct CallText {
    callee: Span,
    body: Span,
    arguments: Vec<Span>,
    comma: Option<Span>,
}

enum Fragment {
    Source(Span),
    Inserted(&'static str),
}

pub(super) fn render(
    source: &str,
    module: &Module<Surface>,
    chain: &[(&Expr<Surface>, usize, UfcsFlavor)],
) -> Option<(Span, String)> {
    let skeleton = parser::parse_tree_skeleton(source).ok()?;
    let mut tokens = Vec::new();
    let mut groups = HashMap::new();
    let mut pending = skeleton.children.iter().collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        match node {
            SkeletonNode::Leaf(token) => tokens.push(token),
            SkeletonNode::Group {
                open,
                close: Some(close),
                recovered: false,
                children,
                ..
            } => {
                tokens.extend([open, close]);
                groups.insert(open.span.start, node);
                pending.extend(children);
            }
            _ => return None,
        }
    }
    tokens.sort_unstable_by_key(|token| token.span.start);
    let call_texts = chain
        .iter()
        .map(|(call, receiver, _)| call_text(call, *receiver, &tokens, &groups))
        .collect::<Option<Vec<_>>>()?;
    let whole = chain.first()?.0.span();
    let mut owners = Vec::new();
    // A direct call is tight in its enclosing expression; a UFCS chain is
    // not. Group the complete replacement so `g.<f(f(a,b),c)` keeps g outside
    // the chain, and ordinary postfix applications keep their callee boundary.
    let mut fragments = vec![Fragment::Inserted("(")];
    for (i, ((call, receiver, _), text)) in chain.iter().zip(&call_texts).enumerate() {
        owners.push(text.callee);
        for (slot, span) in text.arguments.iter().enumerate() {
            if slot != *receiver || i + 1 == chain.len() {
                owners.push(*span);
            }
        }
        if let Some(next) = chain.get(i + 1) {
            let envelope = text.arguments[*receiver];
            if !contains(envelope, next.0.span()) {
                return None;
            }
            let prefix = Span::new(envelope.start, next.0.span().start);
            let suffix = Span::new(next.0.span().end, envelope.end);
            owners.extend([prefix, suffix]);
            fragments.push(Fragment::Source(prefix));
        }
        if call.span().start != text.callee.start {
            return None;
        }
    }
    // Only trivia wholly owned by a retained expression, callee or grouping
    // moves with it. A delimiter-gap comment has no unambiguous new attachment.
    owners.sort_unstable_by_key(|span| span.start);
    for comment in tokens
        .iter()
        .flat_map(|token| &token.leading_trivia)
        .filter_map(|trivia| match trivia {
            Trivia::LineComment { span, .. } | Trivia::DocCommentLine { span, .. } => Some(*span),
            _ => None,
        })
        .filter(|span| contains(whole, *span))
    {
        let owner = owners
            .partition_point(|span| span.start <= comment.start)
            .checked_sub(1)?;
        if !contains(owners[owner], comment) {
            return None;
        }
    }
    let (inner, receiver, _) = chain.last()?;
    let (_, args) = super::parts(inner)?;
    let CallArg::Value(base) = &args[*receiver] else {
        return None;
    };
    let base_span = call_texts.last()?.arguments[*receiver];
    let wrap = !matches!(
        base,
        Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. }
            | Expr::Tuple { .. }
    );
    if wrap {
        fragments.push(Fragment::Inserted("("));
    }
    fragments.push(Fragment::Source(base_span));
    if wrap {
        fragments.push(Fragment::Inserted(")"));
    }
    for (i, ((_, receiver, flavor), text)) in chain.iter().zip(&call_texts).enumerate().rev() {
        fragments.push(Fragment::Inserted(if flavor.inserts_first() {
            ".>"
        } else {
            ".>>"
        }));
        fragments.push(Fragment::Source(text.callee));
        if text.arguments.len() > 1 {
            let receiver = text.arguments[*receiver];
            let comma = text.comma?;
            let mut body = if comma.start < receiver.start {
                vec![
                    Span::new(text.body.start, comma.start),
                    Span::new(comma.end, receiver.start),
                    Span::new(receiver.end, text.body.end),
                ]
            } else {
                vec![
                    Span::new(text.body.start, receiver.start),
                    Span::new(receiver.end, comma.start),
                    Span::new(comma.end, text.body.end),
                ]
            };
            body.retain(|span| span.start < span.end);
            let mut preceding_space = false;
            for span in &mut body {
                let raw = &source[span.start as usize..span.end as usize];
                if preceding_space {
                    span.start += (raw.len() - raw.trim_start_matches([' ', '\t']).len()) as u32;
                }
                if span.start < span.end {
                    preceding_space = source[..span.end as usize].ends_with([' ', '\t']);
                }
            }
            if !body
                .iter()
                .any(|span| source[span.start as usize..span.end as usize].contains(['\n', '\r']))
            {
                if let Some(first) = body.first_mut() {
                    let raw = &source[first.start as usize..first.end as usize];
                    first.start += (raw.len() - raw.trim_start_matches([' ', '\t']).len()) as u32;
                }
                if let Some(last) = body.last_mut() {
                    let raw = &source[last.start as usize..last.end as usize];
                    last.end -= (raw.len() - raw.trim_end_matches([' ', '\t']).len()) as u32;
                }
            }
            fragments.push(Fragment::Inserted("("));
            fragments.extend(body.into_iter().map(Fragment::Source));
            fragments.push(Fragment::Inserted(")"));
        }
        if i > 0 {
            let parent_receiver = chain[i - 1].1;
            fragments.push(Fragment::Source(Span::new(
                chain[i].0.span().end,
                call_texts[i - 1].arguments[parent_receiver].end,
            )));
        }
    }
    fragments.push(Fragment::Inserted(")"));
    let mut output = String::new();
    let mut mapping = Vec::new();
    for fragment in fragments {
        match fragment {
            Fragment::Source(original) => {
                let copied = source.get(original.start as usize..original.end as usize)?;
                let start = output.len() as u32;
                output.push_str(copied);
                if !copied.is_empty() {
                    mapping.push((Span::new(start, output.len() as u32), original));
                }
            }
            Fragment::Inserted(text) => output.push_str(text),
        }
    }
    let context = parser::expression_parse_context(module).ok()?;
    let probe = parser::probe_tooling(&output, None, None, Some(&context));
    if probe.lexical_error.is_some() || probe.parse_error.is_some() {
        return None;
    }
    let ToolingSyntax::Expression(parsed) = probe.syntax? else {
        return None;
    };
    let mapped = |span: Span| {
        let position = mapping
            .partition_point(|(copy, _)| copy.start <= span.start)
            .checked_sub(1)?;
        let (copy, original) = mapping[position];
        contains(copy, span).then(|| {
            Span::new(
                original.start + span.start - copy.start,
                original.start + span.end - copy.start,
            )
        })
    };
    let mut parsed = &parsed;
    for (original, slot, flavor) in chain {
        let Expr::Ufcs {
            receiver,
            callee_segments,
            callee_span,
            args,
            flavor: actual_flavor,
            bang: None,
            ..
        } = parsed
        else {
            return None;
        };
        let (callee, original_args) = super::parts(original)?;
        let Expr::Path { segments, .. } = callee else {
            return None;
        };
        if actual_flavor != flavor
            || mapped(*callee_span) != Some(callee.span())
            || callee_segments
                .iter()
                .map(|segment| &segment.name)
                .ne(segments.iter().map(|segment| &segment.name))
            || args.len() + 1 != original_args.len()
        {
            return None;
        }
        for (arg, original_arg) in args.iter().zip(
            original_args
                .iter()
                .enumerate()
                .filter_map(|(i, arg)| (i != *slot).then_some(arg)),
        ) {
            if std::mem::discriminant(arg) != std::mem::discriminant(original_arg)
                || mapped(arg.meta().span) != Some(original_arg.meta().span)
            {
                return None;
            }
        }
        parsed = receiver;
    }
    if std::mem::discriminant(parsed) != std::mem::discriminant(base)
        || mapped(parsed.span()) != Some(base.span())
    {
        return None;
    }
    Some((whole, output))
}

fn contains(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

fn node_span(node: &SkeletonNode) -> Option<Span> {
    match node {
        SkeletonNode::Leaf(token) => Some(token.span),
        SkeletonNode::Group {
            open,
            close: Some(close),
            recovered: false,
            ..
        } => Some(Span::new(open.span.start, close.span.end)),
        _ => None,
    }
}

fn call_text(
    call: &Expr<Surface>,
    receiver: usize,
    tokens: &[&Token],
    groups: &HashMap<u32, &SkeletonNode>,
) -> Option<CallText> {
    let (callee, args) = super::parts(call)?;
    let next = tokens.partition_point(|token| token.span.start < callee.span().end);
    let group = groups.get(&tokens.get(next)?.span.start)?;
    let SkeletonNode::Group {
        open,
        kind: GroupKind::Paren,
        children,
        close: Some(close),
        recovered: false,
    } = group
    else {
        return None;
    };
    if close.span.end != call.span().end {
        return None;
    }
    let children = children.iter().map(node_span).collect::<Option<Vec<_>>>()?;
    let arguments = args
        .iter()
        .map(|arg| {
            let span = arg.meta().span;
            let first = children.partition_point(|child| child.end <= span.start);
            let last = children
                .partition_point(|child| child.start < span.end)
                .checked_sub(1)?;
            let envelope = Span::new(children.get(first)?.start, children.get(last)?.end);
            contains(envelope, span).then_some(envelope)
        })
        .collect::<Option<Vec<_>>>()?;
    if arguments.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return None;
    }
    let selected = *arguments.get(receiver)?;
    let gap = if let Some(next) = arguments.get(receiver + 1) {
        Span::new(selected.end, next.start)
    } else if receiver > 0 {
        Span::new(arguments[receiver - 1].end, selected.start)
    } else {
        Span::new(selected.end, selected.end)
    };
    let comma_start = tokens.partition_point(|token| token.span.start < gap.start);
    let comma = tokens[comma_start..]
        .iter()
        .take_while(|token| token.span.end <= gap.end)
        .find(|token| token.kind == TokenKind::Comma)
        .map(|token| token.span);
    Some(CallText {
        callee: callee.span(),
        body: Span::new(open.span.end, close.span.start),
        arguments,
        comma,
    })
}
