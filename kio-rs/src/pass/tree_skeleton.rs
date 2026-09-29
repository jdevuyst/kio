//! Tree-skeleton CST pass.
//!
//! Sits between [`crate::pass::lexer`] and the recursive-descent parser
//! in [`crate::pass::parser`]. Walks a token stream and produces a
//! lossless, in-house CST that:
//!
//! - Matches `(` / `)` and `{` / `}` into balanced groups.
//! - Recovers locally on unbalanced groups by marking the
//!   offending subtree as an error and continuing past it.
//!
//! `[` and `]` are *intentionally* not paired here per spec — forall
//! binders such as `[A] A -> A` use them structurally, while ordinary
//! operators may contain either byte; the full parser decides in context. Brace
//! and paren groups, by contrast, are balanced at the macroscopic
//! level (function bodies, paren groupings, `match!` clauses, …) and
//! benefit from skeleton-level pairing.
//!
//! The recursive-descent parser consumes the CST via
//! [`SkeletonCursor`]: paren and brace boundaries come from
//! skeleton groups rather than from running RParen / RBrace match
//! counts. A recovered group (unbalanced input) shows up as a
//! group with `close = None` / `recovered = true`; the parser
//! emits a localized parse error scoped to the group's source
//! range and keeps parsing the rest of the file.

use crate::pass::lexer::Token;
use crate::pass::lexer::TokenKind;
use crate::span::Span;

/// Either a leaf (a single lexer token) or a balanced group
/// (`( … )` / `{ … }`) with its own children. The opening token
/// is tracked separately from the closing token so unbalanced
/// groups (which can arise on user-mid-edit input) record the
/// open token plus a `None` closer; downstream phases see the
/// open token and the recovery boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkeletonNode {
    Leaf(Token),
    Group {
        open: Token,
        kind: GroupKind,
        children: Vec<SkeletonNode>,
        /// `Some(close)` when the group closed cleanly with a
        /// matching delimiter — `close.kind` is `RParen` /
        /// `RBrace` matching `kind`. `None` when the group was
        /// recovered greedily — the close position is implicitly
        /// the `children` vector's last node's end (or the
        /// open's end if the group has no children).
        close: Option<Token>,
        /// `false` when the group closed cleanly. `true` when the
        /// recovery rule (greedy close at the surrounding
        /// boundary) fired because the input had no matching
        /// closer.
        recovered: bool,
    },
}

/// Which delimiter pair opened the group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    Paren,
    Brace,
}

/// The skeleton of one source file. Children are the file's top-
/// level nodes in source order — typically a `module` statement,
/// any `import` statements, and one node per item, separated by
/// statement boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkeletonFile {
    pub children: Vec<SkeletonNode>,
    /// Trivia after the last meaningful token, before end-of-file.
    /// The leading-only trivia model has no token to attach this run
    /// to (see [`crate::pass::lexer::lex_with_trailing`]); the parser
    /// stashes it on the file/module's trailing slot so the formatter
    /// keeps an end-of-file comment across a `kio fmt` round-trip.
    /// Empty when [`build`] was given only a token vector.
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
}

/// Build a [`SkeletonFile`] from a flat token stream. Always
/// succeeds — unbalanced groups are recovered as marked-error
/// subtrees rather than rejected, so the skeleton is available
/// even for mid-edit / partly-broken files.
pub fn build(tokens: Vec<Token>) -> SkeletonFile {
    build_with_trailing(tokens, Vec::new())
}

/// Like [`build`], but also records the end-of-file trivia run from
/// [`crate::pass::lexer::lex_with_trailing`] so the formatter can
/// preserve an end-of-file comment.
pub fn build_with_trailing(
    tokens: Vec<Token>,
    trailing_trivia: Vec<crate::pass::lexer::Trivia>,
) -> SkeletonFile {
    let mut iter = tokens.into_iter().peekable();
    let mut children: Vec<SkeletonNode> = Vec::new();
    while iter.peek().is_some() {
        children.push(read_node(&mut iter, None));
    }
    SkeletonFile {
        children,
        trailing_trivia,
    }
}

fn read_node<I: Iterator<Item = Token>>(
    iter: &mut std::iter::Peekable<I>,
    surrounding: Option<GroupKind>,
) -> SkeletonNode {
    let tok = iter.next().expect("caller checked peek().is_some()");
    match &tok.kind {
        TokenKind::LParen => read_group(iter, tok, GroupKind::Paren, surrounding),
        TokenKind::LBrace => read_group(iter, tok, GroupKind::Brace, surrounding),
        _ => SkeletonNode::Leaf(tok),
    }
}

fn read_group<I: Iterator<Item = Token>>(
    iter: &mut std::iter::Peekable<I>,
    open: Token,
    kind: GroupKind,
    surrounding: Option<GroupKind>,
) -> SkeletonNode {
    let mut children: Vec<SkeletonNode> = Vec::new();
    loop {
        let Some(peek) = iter.peek() else {
            // End of input before close — recover.
            return SkeletonNode::Group {
                open,
                kind,
                children,
                close: None,
                recovered: true,
            };
        };
        let matches_close = matches!(
            (kind, &peek.kind),
            (GroupKind::Paren, TokenKind::RParen) | (GroupKind::Brace, TokenKind::RBrace)
        );
        if matches_close {
            let close_tok = iter.next().expect("just peeked");
            return SkeletonNode::Group {
                open,
                kind,
                children,
                close: Some(close_tok),
                recovered: false,
            };
        }
        // Greedy-close recovery: if the next token is the
        // surrounding group's closer, treat this group as
        // recovered (don't consume the surrounding closer — let
        // the outer call handle it).
        let surrounds = match surrounding {
            Some(GroupKind::Paren) => matches!(&peek.kind, TokenKind::RParen),
            Some(GroupKind::Brace) => matches!(&peek.kind, TokenKind::RBrace),
            None => false,
        };
        if surrounds {
            return SkeletonNode::Group {
                open,
                kind,
                children,
                close: None,
                recovered: true,
            };
        }
        // Wrong-shape close (e.g. `}` inside a `(`-group that
        // doesn't surround a `}`-group) — consume and treat as
        // a leaf so the recovery boundary is precise.
        children.push(read_node(iter, Some(kind)));
    }
}

/// Walks a [`SkeletonFile`] or any `&[SkeletonNode]` slice and
/// presents a flat token-stream view to the parser. Each `Group`
/// is unfolded into its open token, its children (recursively),
/// and its close token; the close token of a balanced group is
/// the recorded `Token`, the close of a recovered group is
/// synthesized from the group's open kind + a zero-length span
/// at the children's end (the parser surfaces a recovery error
/// when it sees the recovery flag).
///
/// The cursor is `Clone` because the parser's backtracking
/// (`try_call_arg_alts`) needs a cheap save / restore. Frames
/// hold only `&[SkeletonNode]` slices, so cloning is O(stack
/// depth).
#[derive(Debug, Clone)]
pub struct SkeletonCursor<'a> {
    /// Stack of frames. The bottom frame walks the file's
    /// top-level children; each pushed frame walks a group's
    /// children. The top frame is the current scope.
    stack: Vec<Frame<'a>>,
    /// Synthetic span used for end-of-input reporting (matches
    /// the source length the caller passed to [`Self::new_at`]).
    eof: u32,
    /// Monotonic counter of tokens consumed via [`advance`]. The
    /// parser uses this for "same-position" comparisons during
    /// backtracking (`try_call_arg_alts` compares the
    /// post-state of two speculative parses to decide whether
    /// they consumed the same range).
    consumed: u64,
    /// Memoized "is the next position recovered?" flag. Set when
    /// the previous `advance` consumed the open token of a
    /// recovered group; cleared on the next `advance`. The
    /// parser reads this immediately after consuming the open
    /// token to surface a parse error scoped to the group.
    pub(crate) just_entered_recovered: bool,
    /// Pending residual when the parser peeled an N-byte prefix
    /// off the current `SymbolRun` via
    /// [`split_current_sym`](Self::split_current_sym). [`peek_token`]
    /// returns this in preference to the underlying frame token;
    /// [`advance`] yields it without touching the frame.
    /// Backtracking captures and restores it alongside the frame
    /// stack via the cursor's derived `Clone`.
    residual: Option<Token>,
}

#[derive(Debug, Clone)]
struct Frame<'a> {
    children: &'a [SkeletonNode],
    /// Index of the next child to consider. When `idx ==
    /// children.len()`, the next yield is the close token (if
    /// `close.is_some()`); after yielding the close, the frame
    /// is popped.
    idx: usize,
    /// `None` for the top-level frame (no surrounding group).
    /// `Some(token)` for a group frame — the close token to emit
    /// when `idx == children.len()`. Synthesized for recovered
    /// groups.
    close: Option<Token>,
}

impl<'a> SkeletonCursor<'a> {
    /// Build a cursor over a whole file. The `eof` argument is
    /// the source length, used to synthesize end-of-input spans.
    pub fn new(file: &'a SkeletonFile, eof: u32) -> Self {
        Self::new_at(&file.children, eof)
    }

    /// Build a cursor over a slice of skeleton children. Used by
    /// per-item rayon fan-out, where each item's slice is walked
    /// independently.
    pub fn new_at(children: &'a [SkeletonNode], eof: u32) -> Self {
        Self {
            stack: vec![Frame {
                children,
                idx: 0,
                close: None,
            }],
            eof,
            consumed: 0,
            just_entered_recovered: false,
            residual: None,
        }
    }

    /// Count of tokens consumed by [`advance`] so far. Used by
    /// the parser's backtracking helper to compare speculative
    /// parses' post-positions.
    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    /// Source length / end-of-input position (for synthesizing
    /// EOF error spans).
    pub fn eof(&self) -> u32 {
        self.eof
    }

    /// Peek at the current token kind without consuming.
    pub fn peek_kind(&self) -> Option<&TokenKind> {
        self.peek_token().map(|t| &t.kind)
    }

    /// Peek at the current token (full token). When a residual
    /// from [`split_current_sym`](Self::split_current_sym) is
    /// pending, returns that in preference to the underlying
    /// frame token.
    pub fn peek_token(&self) -> Option<&Token> {
        if let Some(t) = self.residual.as_ref() {
            return Some(t);
        }
        // Walk the stack from the top down. At each frame, if
        // the index is within the children, return the leaf
        // token or the next group's open token. Otherwise (frame
        // exhausted), return the close token if present and the
        // frame isn't the top-level.
        for frame in self.stack.iter().rev() {
            if frame.idx < frame.children.len() {
                return Some(match &frame.children[frame.idx] {
                    SkeletonNode::Leaf(t) => t,
                    SkeletonNode::Group { open, .. } => open,
                });
            }
            // Frame is exhausted: emit the close token if any.
            if let Some(c) = frame.close.as_ref() {
                return Some(c);
            }
            // Top-level frame exhausted: no token. (Continue
            // unwinding — but the top-level frame is the bottom
            // of the stack, so we'll exit the loop.)
        }
        None
    }

    /// Peek `offset` tokens ahead without consuming. Offset 0 is
    /// the current token. Returns `None` past the end of input.
    pub fn peek_kind_at(&self, offset: usize) -> Option<TokenKind> {
        // Clone the cursor's state and walk forward `offset`
        // times. Cloning the stack is O(stack depth) — typical
        // depth is small.
        let mut tmp = self.clone();
        for _ in 0..offset {
            tmp.advance()?;
        }
        tmp.peek_kind().cloned()
    }

    /// Span of the current token, or the EOF span when past the
    /// end of input.
    pub fn peek_span(&self) -> Span {
        self.peek_token()
            .map(|t| t.span)
            .unwrap_or_else(|| Span::new(self.eof, self.eof))
    }

    /// Leading trivia of the current token. Empty at EOF.
    pub fn peek_leading_trivia(&self) -> Vec<crate::pass::lexer::Trivia> {
        self.peek_token()
            .map(|t| t.leading_trivia.clone())
            .unwrap_or_default()
    }

    /// `true` iff there is no current token (all frames are
    /// exhausted).
    pub fn at_end(&self) -> bool {
        self.peek_token().is_none()
    }

    /// Consume and return the current token. Returns `None` at
    /// EOF. Updates the cursor state, including descending into
    /// groups and ascending past close tokens.
    pub fn advance(&mut self) -> Option<Token> {
        // Reset the recovery flag — it's only true for the
        // single advance call that just consumed an open token.
        self.just_entered_recovered = false;
        // If a residual is pending from a prior
        // `split_current_sym`, yield it without touching the
        // underlying frame.
        if let Some(t) = self.residual.take() {
            self.consumed += 1;
            return Some(t);
        }
        let top = self.stack.last_mut()?;
        if top.idx < top.children.len() {
            let child = &top.children[top.idx];
            match child {
                SkeletonNode::Leaf(t) => {
                    let tok = t.clone();
                    top.idx += 1;
                    self.consumed += 1;
                    Some(tok)
                }
                SkeletonNode::Group {
                    open,
                    children,
                    close,
                    recovered,
                    kind,
                } => {
                    // Yield the open token now, and push a group
                    // frame so the next advance returns the first
                    // child of the group.
                    let open_tok = open.clone();
                    let group_close = close.clone().or_else(|| {
                        // Recovered group: synthesize a virtual
                        // close token at the end of the children
                        // (or after the open if there are no
                        // children).
                        let close_span = children
                            .last()
                            .map(skeleton_node_end_span)
                            .unwrap_or(open.span);
                        let close_kind = match kind {
                            GroupKind::Paren => TokenKind::RParen,
                            GroupKind::Brace => TokenKind::RBrace,
                        };
                        Some(Token {
                            kind: close_kind,
                            span: Span::new(close_span.end, close_span.end),
                            leading_trivia: Vec::new(),
                        })
                    });
                    let was_recovered = *recovered;
                    top.idx += 1;
                    self.stack.push(Frame {
                        children,
                        idx: 0,
                        close: group_close,
                    });
                    if was_recovered {
                        self.just_entered_recovered = true;
                    }
                    self.consumed += 1;
                    Some(open_tok)
                }
            }
        } else if let Some(close_tok) = top.close.take() {
            // Frame exhausted: yield close token, then pop.
            self.stack.pop();
            self.consumed += 1;
            Some(close_tok)
        } else {
            // Top-level frame exhausted (no close to yield).
            // Don't pop the bottom — subsequent calls see "at end".
            None
        }
    }

    /// Depth of the cursor's group-frame stack. `1` at the file
    /// top level (no group entered); `> 1` while inside one or
    /// more groups. Used by some recovery paths to bail
    /// gracefully when the parser is deep inside a recovered
    /// group.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// `true` iff the cursor just consumed the open token of a
    /// recovered group. The parser reads this after consuming a
    /// LParen / LBrace and uses it to surface a localized parse
    /// error. The flag clears on the next `advance`.
    pub fn just_entered_recovered(&self) -> bool {
        self.just_entered_recovered
    }

    /// Span of the close token of the currently entered group
    /// (the top group frame, if any). For a recovered group this
    /// is the synthesized end-position span; for a balanced
    /// group it's the recorded close token's span. Returns
    /// `None` at the top level (no group entered).
    pub fn current_group_close_span(&self) -> Option<Span> {
        self.stack
            .last()
            .and_then(|f| f.close.as_ref().map(|t| t.span))
    }

    /// Slice of the current frame's children that haven't been
    /// consumed yet. Returns the entire children slice at frame
    /// entry; an empty slice at frame exhaustion. The frame may be the
    /// file or an entered group. Its closing token and any partially
    /// consumed operator residual are outside this view, so callers
    /// interpreting whole nodes must start at a node boundary.
    pub fn remaining_current_frame(&self) -> &'a [SkeletonNode] {
        self.stack
            .last()
            .map(|f| &f.children[f.idx..])
            .unwrap_or(&[])
    }

    /// Consume `n` leading bytes from the current
    /// [`TokenKind::SymbolRun`], returning them as a freshly-
    /// spanned token. The remaining suffix becomes the new
    /// "current" via the cursor's residual slot until the next
    /// [`advance`](Self::advance) clears it.
    ///
    /// Used at narrow parser-contextual structural-recovery sites
    /// (including function arrows, forall binders, existential closers,
    /// and bang-call dispatch)
    /// when greedy fusion absorbed the structural delimiter into
    /// a longer run — `A ->!` fuses `->!`, and the parser
    /// peels `->` back off via this primitive.
    ///
    /// Panics if the current peek isn't a `SymbolRun`, if `n ==
    /// 0`, or if `n >= content.len()` (a split that consumes the
    /// whole run is just a regular [`advance`](Self::advance)).
    /// Caller's responsibility to use only at structural-
    /// recovery sites — wide use would re-introduce the layering
    /// blur this primitive is scoped to avoid.
    pub fn split_current_sym(&mut self, n: usize) -> Token {
        debug_assert!(n > 0, "split_current_sym requires n > 0");
        let cur = self.peek_token().cloned().expect("split at EOF");
        let TokenKind::SymbolRun(run) = cur.kind else {
            panic!("split_current_sym requires the current token to be a SymbolRun");
        };
        debug_assert!(
            n < run.len(),
            "split_current_sym n must be strictly less than the run length \
             (n = {}, run = {:?})",
            n,
            run
        );
        let head_text = run[..n].to_owned();
        let tail_text = run[n..].to_owned();
        let head_span = Span::new(cur.span.start, cur.span.start + n as u32);
        let tail_span = Span::new(cur.span.start + n as u32, cur.span.end);
        let head_tok = Token {
            kind: TokenKind::SymbolRun(head_text),
            span: head_span,
            leading_trivia: cur.leading_trivia.clone(),
        };
        let tail_tok = Token {
            kind: TokenKind::SymbolRun(tail_text),
            span: tail_span,
            // The tail begins flush against the head — no
            // leading trivia.
            leading_trivia: Vec::new(),
        };
        // Replace the current logical token with the tail
        // residual. If a residual was already pending (a prior
        // split fed into this one), swap it out in place; the
        // underlying frame doesn't move. Otherwise the current
        // token came from the underlying frame, so step the
        // frame past it before installing the tail.
        if self.residual.is_some() {
            self.residual = Some(tail_tok);
            // Logical "head consumption" — bump explicitly
            // because we bypassed `advance` in this branch.
            self.consumed += 1;
        } else {
            let advanced = self
                .advance()
                .expect("advance after a successful peek on a SymbolRun");
            debug_assert!(matches!(
                advanced.kind,
                TokenKind::SymbolRun(ref s) if s == &run
            ));
            // `advance` bumped `consumed` for the underlying
            // token; that counts as the head's consumption.
            self.residual = Some(tail_tok);
        }
        head_tok
    }
}

/// End span of a skeleton node. For a Leaf, it's the leaf
/// token's span. For a Group, it's the close span (if balanced)
/// or the end of its last child (if recovered).
fn skeleton_node_end_span(node: &SkeletonNode) -> Span {
    match node {
        SkeletonNode::Leaf(t) => t.span,
        SkeletonNode::Group {
            open,
            children,
            close,
            ..
        } => close.as_ref().map(|t| t.span).unwrap_or_else(|| {
            children
                .last()
                .map(skeleton_node_end_span)
                .unwrap_or(open.span)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::lexer::lex;

    fn build_str(src: &str) -> SkeletonFile {
        let tokens = lex(src).expect("lex");
        build(tokens)
    }

    fn group_kind(node: &SkeletonNode) -> Option<GroupKind> {
        match node {
            SkeletonNode::Group { kind, .. } => Some(*kind),
            SkeletonNode::Leaf(_) => None,
        }
    }

    fn group_children(node: &SkeletonNode) -> &[SkeletonNode] {
        match node {
            SkeletonNode::Group { children, .. } => children,
            SkeletonNode::Leaf(_) => &[],
        }
    }

    #[test]
    fn empty_file_has_no_children() {
        let file = build_str("");
        assert!(file.children.is_empty());
    }

    #[test]
    fn single_paren_group_balanced() {
        // `(a)` — leaf paren wrapping an ident leaf.
        let file = build_str("(a)");
        assert_eq!(file.children.len(), 1);
        assert_eq!(group_kind(&file.children[0]), Some(GroupKind::Paren));
        let inner = group_children(&file.children[0]);
        assert_eq!(inner.len(), 1);
        assert!(
            matches!(&inner[0], SkeletonNode::Leaf(t) if matches!(&t.kind, TokenKind::Ident(s) if s == "a"))
        );
        if let SkeletonNode::Group {
            close, recovered, ..
        } = &file.children[0]
        {
            assert!(close.is_some());
            assert!(!recovered);
        } else {
            panic!("expected Group");
        }
    }

    #[test]
    fn nested_groups_pair() {
        // `{ ( a ) }` — brace wrapping a paren wrapping an ident.
        let file = build_str("{ (a) }");
        assert_eq!(file.children.len(), 1);
        assert_eq!(group_kind(&file.children[0]), Some(GroupKind::Brace));
        let brace_children = group_children(&file.children[0]);
        assert_eq!(brace_children.len(), 1);
        assert_eq!(group_kind(&brace_children[0]), Some(GroupKind::Paren));
    }

    #[test]
    fn unmatched_open_paren_recovers_at_eof() {
        // `(a` — no matching `)` at end of input. Group recovers
        // greedily with `recovered = true` and `close = None`.
        let file = build_str("(a");
        assert_eq!(file.children.len(), 1);
        let SkeletonNode::Group {
            close, recovered, ..
        } = &file.children[0]
        else {
            panic!("expected Group");
        };
        assert!(close.is_none());
        assert!(*recovered);
    }

    #[test]
    fn unmatched_open_brace_inside_paren_recovers_locally() {
        // `( { a )` — brace inside paren has no matching `}`.
        // The brace recovers when it sees the surrounding `)`,
        // and the paren closes cleanly afterward.
        let file = build_str("( { a )");
        assert_eq!(file.children.len(), 1);
        let outer = &file.children[0];
        assert_eq!(group_kind(outer), Some(GroupKind::Paren));
        let SkeletonNode::Group {
            close: outer_close,
            recovered: outer_rec,
            children: outer_children,
            ..
        } = outer
        else {
            panic!("expected Paren Group");
        };
        // Outer paren closes cleanly.
        assert!(outer_close.is_some());
        assert!(!outer_rec);
        // Single child is the recovered brace.
        assert_eq!(outer_children.len(), 1);
        let SkeletonNode::Group {
            close: inner_close,
            recovered: inner_rec,
            kind: inner_kind,
            ..
        } = &outer_children[0]
        else {
            panic!("expected inner Brace Group");
        };
        assert_eq!(*inner_kind, GroupKind::Brace);
        assert!(inner_close.is_none());
        assert!(*inner_rec);
    }

    #[test]
    fn brackets_are_not_paired() {
        // `[*A][B]` lexes as `[*`, `A`, `][`, `B`, `]`: brackets
        // belong to maximal operator runs, and the skeleton pass leaves
        // those runs as leaves. The full parser peels binder delimiters
        // contextually.
        let file = build_str("[*A][B]");
        assert_eq!(file.children.len(), 5);
        assert!(matches!(&file.children[0], SkeletonNode::Leaf(t) if t.kind.is_sym("[*")));
        assert!(
            matches!(&file.children[1], SkeletonNode::Leaf(t) if matches!(&t.kind, TokenKind::Ident(s) if s == "A"))
        );
        assert!(matches!(&file.children[2], SkeletonNode::Leaf(t) if t.kind.is_sym("][")));
        assert!(
            matches!(&file.children[3], SkeletonNode::Leaf(t) if matches!(&t.kind, TokenKind::Ident(s) if s == "B"))
        );
        assert!(matches!(&file.children[4], SkeletonNode::Leaf(t) if t.kind.is_sym("]")));

        let file = build_str("[A]");
        assert_eq!(file.children.len(), 3);
        assert!(matches!(&file.children[0], SkeletonNode::Leaf(t) if t.kind.is_sym("[")));
        assert!(
            matches!(&file.children[1], SkeletonNode::Leaf(t) if matches!(&t.kind, TokenKind::Ident(s) if s == "A"))
        );
        assert!(matches!(&file.children[2], SkeletonNode::Leaf(t) if t.kind.is_sym("]")));
    }

    #[test]
    fn module_header_then_item_groups_pair() {
        // A small `module X; fn foo() -> . { () }` file: the
        // skeleton has a sequence of leaf tokens for the header
        // and the fn signature, then a Brace group for the body
        // containing a Paren group for `()`.
        let file = build_str("module x; fn foo() -> . { () }");
        // The brace group is the last child.
        let brace = file
            .children
            .iter()
            .filter_map(|n| match n {
                SkeletonNode::Group {
                    kind: GroupKind::Brace,
                    children,
                    ..
                } => Some(children),
                _ => None,
            })
            .next()
            .expect("expected a Brace group for the fn body");
        // Body has one Paren group for the unit value `()`.
        let paren = brace
            .iter()
            .find(|n| {
                matches!(
                    n,
                    SkeletonNode::Group {
                        kind: GroupKind::Paren,
                        ..
                    }
                )
            })
            .expect("expected a Paren group inside the fn body");
        let _ = paren;
    }

    fn cursor_kinds(src: &str) -> Vec<TokenKind> {
        let file = build_str(src);
        let mut cursor = SkeletonCursor::new(&file, src.len() as u32);
        let mut out = Vec::new();
        while let Some(t) = cursor.advance() {
            out.push(t.kind);
        }
        out
    }

    fn lex_kinds(src: &str) -> Vec<TokenKind> {
        lex(src).expect("lex").into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn cursor_flat_view_matches_token_stream_for_balanced_input() {
        // For balanced input, the cursor's flat advance() walk
        // yields the same kinds as the original token stream —
        // open / children / close in source order.
        let sources = [
            "",
            "module x;",
            "module x; fn foo() -> . { () }",
            "(a, b, c)",
            "{ a; b }",
            "module x; fn f(p: I32) -> I32 { let y = p; y }",
        ];
        for src in sources {
            assert_eq!(cursor_kinds(src), lex_kinds(src), "src = {src:?}");
        }
    }

    #[test]
    fn cursor_signals_recovery_when_entering_recovered_group() {
        // `(a` — open paren, then `a`, then EOF (recovered).
        // After advance() consumes the open paren, the recovery
        // flag is set.
        let file = build_str("(a");
        let mut cursor = SkeletonCursor::new(&file, 2);
        assert!(!cursor.just_entered_recovered());
        let open = cursor.advance().expect("open paren");
        assert!(matches!(open.kind, TokenKind::LParen));
        assert!(cursor.just_entered_recovered());
        // Next advance: the `a` leaf. Flag clears.
        let ident = cursor.advance().expect("ident");
        assert!(matches!(ident.kind, TokenKind::Ident(_)));
        assert!(!cursor.just_entered_recovered());
        // Then the synthesized close paren.
        let close = cursor.advance().expect("synth close");
        assert!(matches!(close.kind, TokenKind::RParen));
    }

    #[test]
    fn cursor_peek_kind_at_walks_through_groups() {
        // `f(a, b)` — peek_kind_at(0..=5) sees the tokens in
        // order, including group descent / ascent: `f`, `(`,
        // `a`, `,`, `b`, `)`.
        let file = build_str("f(a, b)");
        let cursor = SkeletonCursor::new(&file, 7);
        let kinds: Vec<TokenKind> = (0..6)
            .map(|i| cursor.peek_kind_at(i).expect("kind"))
            .collect();
        assert!(matches!(kinds[0], TokenKind::Ident(_)));
        assert!(matches!(kinds[1], TokenKind::LParen));
        assert!(matches!(kinds[2], TokenKind::Ident(_)));
        assert!(matches!(kinds[3], TokenKind::Comma));
        assert!(matches!(kinds[4], TokenKind::Ident(_)));
        assert!(matches!(kinds[5], TokenKind::RParen));
    }

    #[test]
    fn cursor_clone_preserves_state_for_backtracking() {
        // The parser saves a cursor before a speculative parse
        // and restores it on backtrack. Cloning then walking the
        // clone must not affect the original.
        let file = build_str("(a, b)");
        let mut cursor = SkeletonCursor::new(&file, 6);
        let saved = cursor.clone();
        // Walk past `(`, `a`, `,` — three advances.
        let _ = cursor.advance();
        let _ = cursor.advance();
        let _ = cursor.advance();
        // saved is still at the start.
        assert!(matches!(saved.peek_kind(), Some(TokenKind::LParen)));
        // cursor is now at `b`.
        assert!(matches!(cursor.peek_kind(), Some(TokenKind::Ident(_))));
    }

    // ---- split_current_sym ------------------------------------------------

    #[test]
    fn split_current_sym_peels_head_and_keeps_tail() {
        // `<=>` fuses into one greedy SymbolRun. Peel `<=` off
        // the front; the residual is `>` and the next advance
        // yields it with a span flush against the head.
        let src = "<=>";
        let file = build_str(src);
        let mut cursor = SkeletonCursor::new(&file, src.len() as u32);
        let head = cursor.split_current_sym(2);
        assert!(matches!(head.kind, TokenKind::SymbolRun(ref s) if s == "<="));
        assert_eq!(head.span, Span::new(0, 2));
        let tail = cursor.advance().expect("residual yields the tail");
        assert!(matches!(tail.kind, TokenKind::SymbolRun(ref s) if s == ">"));
        assert_eq!(tail.span, Span::new(2, 3));
        assert!(cursor.peek_token().is_none());
    }

    #[test]
    fn split_current_sym_residual_is_visible_via_peek() {
        // After a split, the cursor's `peek_token` reports the
        // tail residual until the next advance clears it.
        let src = ">:";
        let file = build_str(src);
        let mut cursor = SkeletonCursor::new(&file, src.len() as u32);
        let head = cursor.split_current_sym(1);
        assert!(matches!(head.kind, TokenKind::SymbolRun(ref s) if s == ">"));
        // peek should now report `:` (the residual).
        match cursor.peek_token() {
            Some(t) => assert!(t.kind.is_sym(":")),
            None => panic!("expected `:` residual"),
        }
        let _ = cursor.advance();
        assert!(cursor.peek_token().is_none());
    }

    #[test]
    fn split_current_sym_threads_into_clone_for_backtracking() {
        // A clone taken after a split sees the same residual —
        // backtracking can resume from either state.
        let src = ">: x";
        let file = build_str(src);
        let mut cursor = SkeletonCursor::new(&file, src.len() as u32);
        let _ = cursor.split_current_sym(1);
        let saved = cursor.clone();
        // Drain the original.
        let tail = cursor.advance().expect("tail");
        assert!(matches!(tail.kind, TokenKind::SymbolRun(ref s) if s == ":"));
        // The clone still has the residual pending.
        match saved.peek_token() {
            Some(t) => assert!(t.kind.is_sym(":")),
            None => panic!("expected `:` residual in clone"),
        }
    }

    #[test]
    fn split_current_sym_peek_kind_at_steps_through_residual() {
        // `peek_kind_at(0)` returns the residual; `peek_kind_at(1)`
        // returns the token after the residual is consumed.
        let src = ">: y";
        let file = build_str(src);
        let mut cursor = SkeletonCursor::new(&file, src.len() as u32);
        let _ = cursor.split_current_sym(1);
        let at0 = cursor.peek_kind_at(0).expect("residual");
        assert!(at0.is_sym(":"));
        let at1 = cursor.peek_kind_at(1).expect("after residual");
        assert!(matches!(at1, TokenKind::Ident(ref s) if s == "y"));
    }
}
