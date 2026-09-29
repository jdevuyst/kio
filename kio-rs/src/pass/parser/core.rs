//! Recursive-descent parser internals: the `Parser` state struct
//! and its ~64 helper methods.
//!
//! Each surface-level entry point (`parse`, `parse_build_block_body`,
//! `parse_package_file`) lives in the umbrella
//! [`super`] (`pass/parser`) and is a thin wrapper that constructs a
//! [`Parser`], drives the matching top-level production (`module`
//! / `package_file`), and calls
//! [`Parser::expect_eof`] before returning. Everything below — the
//! Parser state, helper types, private utilities — is shared
//! across the entry productions. The `build { ... }` block is parsed
//! inline by [`Parser::package_file`]; its body parser is also
//! exposed standalone for focused parser and formatter tests.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
use crate::ast::{
    BlockFieldValue, BridgeBlock, BridgeGlob, BridgeGlobSegment, BuildBlock, BuildBlockCache,
    CallArg, CallableSpec, DependencyFile, DocComment, ElaboratorCall, ElaboratorKind, Expr,
    FieldAccessLabel, FieldUpdateLabel, FnDef, GitManifestPath, GitSource, HostFn,
    HostFnParamGroup, HostFnValueParam, HostType, Import, ImportItem, ImportKind, Item, LabelEntry,
    LabelForward, LabelValueLabel, Labels, LabelsArm, LiteralAlias, LiteralAliasValue, LockFile,
    Meta, Module, ModulePath, Newtype, Op, OpBody, OpChainKind, OpPart, OperatorDispatchKey,
    PackageFile, Param, ParamPattern, ParamPatternElem, PathSegment, Purity, RecCallMode, RecGroup,
    RehostDecl, RetypeDecl, Role, RoleAnnotation, RowLetEntry, SigChangeSet, SigExportFn, SigItem,
    SigItemRef, SigModuleSection, SigRemoveModule, SigRemoveName, SigVersion, Signature,
    SignatureFile, SignatureGroup, SignatureParam, SourceBlock, SourceOrigin, TargetBlock,
    TargetEntry, Type, TypeAlias, TypeMember, TypeParam, TypeRecGroup, TypeRecMember, UfcsFlavor,
    UserElaboratorDef, VariadicOperator, Visibility,
};
use crate::error::{Error, Fix, FixEdit};
use crate::pass::lexer::{Token, TokenKind, Trivia};
use crate::pass::tree_skeleton::{SkeletonCursor, SkeletonFile, SkeletonNode};
use crate::span::Span;
#[cfg(feature = "parallel")]
use rayon::prelude::*;

type SigBlockEntries<T> = (Vec<T>, Vec<SigItemRef>, Vec<Trivia>);

/// Prepend `trivia` to the expression's `meta.leading_trivia` —
/// used by `let_statement` / `block_body_continuation` /
/// elaborator arm tuples to attach captured between-token trivia to the
/// inner expression that follows. The expression's own captured
/// trivia (if any) is kept after the injected run so source order is
/// preserved.
fn inject_leading_trivia(expr: &mut Expr, trivia: Vec<Trivia>) {
    inject_meta_trivia(expr.meta_mut(), trivia);
}

/// Lower-level version of [`inject_leading_trivia`] that operates
/// directly on a `Meta` — used at sites whose target node is a
/// `Type<Surface>` (whose `meta_mut` lives on `Type`) or a
/// label struct that exposes its `Meta` field directly.
fn inject_meta_trivia(meta: &mut crate::ast::Meta<crate::ast::Surface>, mut trivia: Vec<Trivia>) {
    if trivia.is_empty() {
        return;
    }
    trivia.append(&mut meta.leading_trivia);
    meta.leading_trivia = trivia;
}

fn comment_span(trivia: &Trivia) -> Option<Span> {
    match trivia {
        Trivia::LineComment { span, .. } | Trivia::DocCommentLine { span, .. } => Some(*span),
        Trivia::Newline => None,
    }
}

pub(super) fn trivia_cursor_suppression(
    trivia: &[Trivia],
    cursor: u32,
) -> Option<CursorSuppression> {
    trivia.iter().filter_map(comment_span).find_map(|span| {
        (span.start <= cursor && cursor <= span.end).then_some(CursorSuppression {
            span,
            kind: CursorSuppressionKind::Comment,
        })
    })
}

pub(super) fn token_cursor_suppression(token: &Token, cursor: u32) -> Option<CursorSuppression> {
    trivia_cursor_suppression(&token.leading_trivia, cursor).or_else(|| {
        (matches!(
            token.kind,
            TokenKind::StrLit(_)
                | TokenKind::IntLit { .. }
                | TokenKind::FloatLit { .. }
                | TokenKind::BoolLit(_)
        ) && token.span.start <= cursor
            && cursor <= token.span.end)
            .then_some(CursorSuppression {
                span: token.span,
                kind: CursorSuppressionKind::Literal,
            })
    })
}

fn remove_meta_comments(
    meta: &mut Meta<crate::ast::Surface>,
    moved: &std::collections::HashSet<Span>,
) {
    meta.leading_trivia
        .retain(|trivia| comment_span(trivia).is_none_or(|span| !moved.contains(&span)));
    meta.trailing_trivia
        .retain(|trivia| comment_span(trivia).is_none_or(|span| !moved.contains(&span)));
}

/// Remove comments moved out of a compact declaration's type from their old
/// structured trivia slots. The skeleton traversal supplies source order;
/// consuming the matching AST copies leaves each comment with one owner.
fn remove_type_comments(ty: &mut Type, moved: &std::collections::HashSet<Span>) {
    remove_meta_comments(ty.meta_mut(), moved);
    match ty {
        Type::Path { args, .. } => {
            for arg in args {
                remove_type_comments(arg, moved);
            }
        }
        Type::Function { param, ret, .. } => {
            remove_type_comments(param, moved);
            remove_type_comments(ret, moved);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            remove_type_comments(left, moved);
            remove_type_comments(right, moved);
        }
        Type::LabelSugar { labels, .. } => {
            for label in labels {
                remove_meta_comments(&mut label.meta, moved);
                if let Some(payload) = &mut label.payload {
                    remove_type_comments(payload, moved);
                }
            }
        }
        Type::Forall { body, .. } => remove_type_comments(body, moved),
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
        Type::Goal { ext, .. } => match *ext {},
    }
}

/// Collect comments after the first meaningful token in one top-level item
/// slice. `op`, `varop`, and `elab` canonicalize compact bodies whose fields do
/// not all have trivia channels, so their internal comments move together to
/// the compact body's trivia slot in source order. The first token's trivia is
/// excluded because
/// [`Parser::documented_declaration`] already preserves the run preceding the
/// declaration.
fn internal_item_comments(children: &[SkeletonNode]) -> Vec<Trivia> {
    fn collect_token(token: &Token, saw_first: &mut bool, comments: &mut Vec<Trivia>) {
        if std::mem::replace(saw_first, true) {
            comments.extend(
                token
                    .leading_trivia
                    .iter()
                    .filter(|trivia| comment_span(trivia).is_some())
                    .cloned(),
            );
        }
    }

    fn collect_node(node: &SkeletonNode, saw_first: &mut bool, comments: &mut Vec<Trivia>) {
        match node {
            SkeletonNode::Leaf(token) => collect_token(token, saw_first, comments),
            SkeletonNode::Group {
                open,
                children,
                close,
                ..
            } => {
                collect_token(open, saw_first, comments);
                for child in children {
                    collect_node(child, saw_first, comments);
                }
                if let Some(close) = close {
                    collect_token(close, saw_first, comments);
                }
            }
        }
    }

    let mut saw_first = false;
    let mut comments = Vec::new();
    for child in children {
        collect_node(child, &mut saw_first, &mut comments);
    }
    comments
}

fn retain_compact_declaration_comments(item: &mut Item, children: &[SkeletonNode]) {
    if matches!(
        item,
        Item::Op(_, _) | Item::VariadicOperator(_, _) | Item::Elaborator(_, _)
    ) {
        let declaration_end = item.meta().span.end;
        let comments = internal_item_comments(children)
            .into_iter()
            .filter(|trivia| comment_span(trivia).is_some_and(|span| span.end <= declaration_end))
            .collect::<Vec<_>>();
        let moved = comments.iter().filter_map(comment_span).collect();
        if let Item::Elaborator(elaborator, _) = item {
            remove_type_comments(&mut elaborator.call_ty, &moved);
        }
        match item {
            Item::Op(op, _) => op.body_trivia.extend(comments),
            Item::VariadicOperator(fold, _) => fold.body_trivia.extend(comments),
            Item::Elaborator(elaborator, _) => elaborator.body_trivia.extend(comments),
            _ => unreachable!("only compact declarations reach this branch"),
        }
    }
}

/// Stash `trivia` — the leading trivia of a closing delimiter (`}` /
/// `)`) or end-of-file token — onto `meta.trailing_trivia`, where the
/// lexer's leading-only trivia model would otherwise lose it. The
/// formatter reads this slot to keep a trailing or dangling comment in
/// place across a `kio fmt` round-trip. A no-op for an empty run so
/// non-comment closers leave the slot at its default.
fn set_trailing_trivia(meta: &mut crate::ast::Meta<crate::ast::Surface>, trivia: Vec<Trivia>) {
    if trivia.is_empty() {
        return;
    }
    meta.trailing_trivia = trivia;
}

fn empty_ufcs_arg_list_error(span: Span) -> Error {
    let error = Error::parse(
        span,
        "an explicitly empty UFCS argument list is ambiguous",
    )
    .with_help(
        "remove this argument list to write the bare UFCS form, or replace it with `(())` to pass Unit",
    );
    if span.end.checked_sub(span.start) == Some(2) {
        return error
            .with_fix(Fix::maybe_incorrect(
                "Pass Unit explicitly",
                vec![FixEdit::new(span, "(())")],
            ))
            .with_fix(Fix::maybe_incorrect(
                "Use the bare UFCS form",
                vec![FixEdit::new(span, "")],
            ));
    }

    let close = span
        .end
        .checked_sub(1)
        .expect("an empty call-argument-list span includes a closing parenthesis");
    error.with_fix(Fix::maybe_incorrect(
        "Pass Unit explicitly",
        vec![FixEdit::new(Span::new(close, close), "()")],
    ))
}

fn tuple_type_error(span: Span) -> Error {
    Error::parse(
        span,
        "commas form tuple values, not product types; write `A & B`",
    )
    .with_help("replace tuple separators with `&`; keep each component's grouping")
}

fn value_shaped_type_error(group: &SkeletonNode, original: &Error) -> Option<Error> {
    let SkeletonNode::Group {
        open,
        children,
        close: Some(close),
        recovered: false,
        ..
    } = group
    else {
        return None;
    };
    let span = Span::new(open.span.start, close.span.end);
    // Only replay the group that owns the rejection, not each enclosing
    // group through which a nested error propagates.
    let owns_error = (children.is_empty() && original.diagnostic().span == span)
        || children.iter().any(|node| {
            matches!(node, SkeletonNode::Leaf(Token { kind: TokenKind::Comma, span, .. })
                if *span == original.diagnostic().span)
        });
    if !owns_error {
        return None;
    }

    let mut parser = Parser::new_at(children, close.span.start as usize);
    let mut commas = Vec::new();
    let mut edits = Vec::new();
    let mut components = 0;
    while parser.peek().is_some() {
        if matches!(parser.peek(), Some(TokenKind::Comma)) {
            commas.push(parser.advance().span);
            continue;
        }
        let start = parser.peek_span().start;
        let remaining = parser.cursor.remaining_current_frame();
        let ty = parser.type_expr().ok()?;
        if !matches!(parser.peek(), None | Some(TokenKind::Comma)) {
            return None;
        }
        let consumed = remaining.len() - parser.cursor.remaining_current_frame().len();
        // A one-element or empty chain collapses to an atomic AST type,
        // but its written separators still need their own group.
        let written_chain = remaining[..consumed].iter().any(|node| {
            matches!(node, SkeletonNode::Leaf(token)
                if is_amp_chain_sep(&token.kind) || is_pipe_chain_sep(&token.kind))
        });
        components += 1;
        if written_chain
            || matches!(
                ty,
                Type::Function { .. }
                    | Type::Product { .. }
                    | Type::Sum { .. }
                    | Type::Forall { .. }
            )
        {
            // AST spans may omit written parentheses. Token boundaries retain
            // the complete component, including its trailing comment trivia.
            let end = parser.peek_span().start;
            edits.push(FixEdit::new(Span::new(start, start), "("));
            edits.push(FixEdit::new(Span::new(end, end), ")"));
        }
    }

    if components == 0 {
        let has_comments = children
            .iter()
            .filter_map(|node| match node {
                SkeletonNode::Leaf(token) => Some(token),
                SkeletonNode::Group { .. } => None,
            })
            .chain(std::iter::once(close))
            .flat_map(|token| &token.leading_trivia)
            .any(|trivia| {
                matches!(
                    trivia,
                    Trivia::LineComment { .. } | Trivia::DocCommentLine { .. }
                )
            });
        let edits = if has_comments {
            let mut edits = vec![FixEdit::new(open.span, " . ")];
            edits.extend(commas.into_iter().map(|comma| FixEdit::new(comma, "")));
            edits.push(FixEdit::new(close.span, ""));
            edits
        } else {
            vec![FixEdit::new(span, " . ")]
        };
        return Some(
            Error::parse(
                span,
                "`()` is unit value syntax; write `.` for the unit type",
            )
            .with_help("replace this unit value spelling with the unit type `.`")
            .with_fix(Fix::machine_applicable("Use `.` for the unit type", edits)),
        );
    }

    let first_comma = *commas.first()?;
    // The spaces keep `&` separate from adjacent operator tokens such as `!`.
    edits.extend(commas.into_iter().map(|comma| FixEdit::new(comma, " & ")));
    Some(
        tuple_type_error(first_comma).with_fix(Fix::machine_applicable(
            "Use `&` for the product type",
            edits,
        )),
    )
}

struct ParsedBlockFieldHead {
    key: String,
    key_span: Span,
    value: BlockFieldValue,
    value_span: Span,
    leading_trivia: Vec<Trivia>,
}

/// Prepend the trivia before an import keyword to comments hoisted from
/// its header tokens. Selection and container comments stay on their own
/// structural boundaries.
fn prepend_import_leading(u: &mut Import, mut leading: Vec<Trivia>) {
    if u.leading_trivia.is_empty() {
        u.leading_trivia = leading;
    } else {
        leading.append(&mut u.leading_trivia);
        u.leading_trivia = leading;
    }
}

/// Extract the trailing contiguous run of `DocCommentLine` entries
/// from `trivia`, returning them as a `DocComment` (or `None` if no
/// doc-comment lines are present in the trailing run).
///
/// "Contiguous" means the doc lines are separated only by single
/// `Newline` entries — a double-newline (blank line) or a
/// `LineComment` between `///` lines ends the run and the earlier
/// portion is not part of the attached doc-comment.
///
/// The extracted lines are *removed* from `trivia` (along with any
/// `Newline` separators between them). Trivia that precedes the
/// doc-comment run is left in place.
fn extract_doc_comment(trivia: &mut Vec<Trivia>) -> Option<DocComment> {
    // Walk backwards to find the trailing doc-comment run.
    // We scan from the end; skip a trailing Newline if present
    // (the newline between the last `///` line and the token itself).
    // Then collect consecutive [DocCommentLine, Newline?] pairs.

    let mut end_idx = trivia.len();
    // Skip the very last Newline (between last doc line and the keyword).
    if end_idx > 0 && matches!(trivia[end_idx - 1], Trivia::Newline) {
        end_idx -= 1;
    }

    // Scan backwards over DocCommentLine / Newline pairs.
    // The run ends when we hit a LineComment, a double-Newline, or the start.
    let mut scan = end_idx;
    loop {
        if scan == 0 {
            break;
        }
        match &trivia[scan - 1] {
            Trivia::DocCommentLine { .. } => {
                scan -= 1;
                // Optionally skip one preceding Newline (separator between lines).
                if scan > 0 && matches!(trivia[scan - 1], Trivia::Newline) {
                    scan -= 1;
                }
            }
            // LineComment or Newline here means we've left the doc block.
            _ => break,
        }
    }

    if scan == end_idx {
        // No DocCommentLine found at the trailing position.
        return None;
    }

    // Check that the element at `scan` actually starts with a DocCommentLine
    // (not a stray Newline from the separator skip).
    let run_start = if matches!(trivia[scan], Trivia::Newline) {
        scan + 1
    } else {
        scan
    };

    if run_start >= end_idx {
        return None;
    }

    // Collect the doc lines and compute the span.
    let mut lines: Vec<String> = Vec::new();
    let mut span_start: Option<u32> = None;
    let mut span_end: u32 = 0;

    for t in &trivia[run_start..end_idx] {
        if let Trivia::DocCommentLine { text, span } = t {
            if span_start.is_none() {
                span_start = Some(span.start);
            }
            span_end = span.end;
            lines.push(text.clone());
        }
        // Skip Newline separators.
    }

    if lines.is_empty() {
        return None;
    }

    // Remove the extracted range (run_start..=end_idx) from trivia.
    // Also remove the separator Newline at end_idx if it's still there.
    let remove_end = if end_idx < trivia.len() && matches!(trivia[end_idx], Trivia::Newline) {
        end_idx + 1
    } else {
        end_idx
    };
    trivia.drain(run_start..remove_end);

    Some(DocComment {
        lines,
        span: crate::span::Span::new(span_start.unwrap(), span_end),
    })
}

fn trivia_comment_span(trivia: &[Trivia]) -> Option<Span> {
    let mut comments = trivia.iter().filter_map(|trivia| match trivia {
        Trivia::LineComment { span, .. } | Trivia::DocCommentLine { span, .. } => Some(*span),
        Trivia::Newline => None,
    });
    let first = comments.next()?;
    let end = comments.fold(first.end, |_, span| span.end);
    Some(Span::new(first.start, end))
}

/// Set the `doc` field on a module-body item that supports it.
/// Every documentable kind carries a `doc: Option<DocComment>` field;
/// `Equiv` deliberately lacks one (a test-claim, not a documented
/// surface item), so its arm is an explicit no-op. The match is
/// **exhaustive with no wildcard**: adding a new [`Item`] variant
/// forces a deliberate documentable / non-documentable choice here.
fn set_item_doc(item: &mut Item, doc: Option<DocComment>, leading: &[Trivia]) {
    let doc_span = doc.as_ref().map(|doc| doc.span);
    match item {
        Item::FnDef(d) => d.doc = doc,
        Item::TypeAlias(a) => a.doc = doc,
        Item::LiteralAlias(l, _) => l.doc = doc,
        Item::Newtype(n) => n.doc = doc,
        Item::Labels(d, _) => d.doc = doc,
        Item::LabelForward(d, _) => d.doc = doc,
        Item::Op(b, _) => b.doc = doc,
        Item::VariadicOperator(b, _) => b.doc = doc,
        Item::Elaborator(s, _) => s.doc = doc,
        Item::HostType(h) => h.doc = doc,
        Item::HostFn(h) => h.doc = doc,
        Item::RecGroup(g, _) => {
            if g.members.len() == 1 {
                g.members[0].doc = doc;
            }
        }
        Item::TypeRecGroup(group) => group.doc = doc,
        // `equiv` is a test-claim, not a documented surface item, and
        // carries no `doc` field. If `Equiv` ever gains one, this arm
        // becomes a compile error here, forcing a deliberate decision.
        Item::Equiv(_, _) => {}
    }
    let ambiguous_leading_comment = trivia_comment_span(leading).is_some();
    if ambiguous_leading_comment {
        match item {
            Item::TypeAlias(alias) => alias.editable_span = None,
            Item::Newtype(newtype) => newtype.editable_span = None,
            Item::Labels(labels, _) => labels.editable_span = None,
            Item::LabelForward(forward, _) => forward.editable_span = None,
            _ => {}
        }
    } else if let Some(doc_span) = doc_span {
        match item {
            Item::TypeAlias(alias) => {
                alias.editable_span = Some(Span::new(doc_span.start, alias.meta.span.end));
            }
            Item::Newtype(newtype) => {
                let end = newtype
                    .editable_span
                    .map_or(newtype.meta.span.end, |span| span.end);
                newtype.editable_span = Some(Span::new(doc_span.start, end));
            }
            Item::Labels(labels, _) => {
                labels.editable_span = Some(Span::new(doc_span.start, labels.meta.span.end));
            }
            Item::LabelForward(forward, _) => {
                forward.editable_span = Some(Span::new(doc_span.start, forward.meta.span.end));
            }
            _ => {}
        }
    }
}

fn set_sig_item_doc(item: &mut SigItem, doc: Option<DocComment>) {
    match item {
        SigItem::HostType(h) => h.doc = doc,
        SigItem::HostFn(h) => h.doc = doc,
        SigItem::ExportFn(export) => export.function.doc = doc,
        SigItem::TypeAlias(a) => a.doc = doc,
        SigItem::Newtype(n) => n.doc = doc,
        SigItem::TypeRecGroup(_) => {}
    }
}

fn sig_item_meta_mut(item: &mut SigItem) -> &mut crate::ast::Meta<crate::ast::Surface> {
    match item {
        SigItem::HostType(h) => &mut h.meta,
        SigItem::HostFn(h) => &mut h.meta,
        SigItem::ExportFn(export) => &mut export.function.meta,
        SigItem::TypeAlias(a) => &mut a.meta,
        SigItem::Newtype(n) => &mut n.meta,
        SigItem::TypeRecGroup(group) => &mut group.meta,
    }
}

struct LabelPathParts {
    full: String,
    full_span: Span,
    last: String,
    last_span: Span,
}

/// Opaque consumer-declared operator grammar for parsing a standalone expression in a
/// module's exact scope. REPL tooling caches this alongside its loaded-module
/// snapshot rather than reconstructing operator patterns from completion
/// display strings.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(test, feature = "cli"))]
pub(crate) struct ExpressionParseContext {
    operator_scope: OpRegistry,
}

/// Parser-confirmed square-delimiter facts collected only for a requested
/// expression-fragment probe. Ordinary module parsing keeps this sink absent,
/// so recording tooling facts adds no allocation to the compiler hot path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StructuralForallFacts {
    pub(crate) bracket_offsets: Vec<u32>,
    pub(crate) unclosed_at_eof: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeywordRole {
    Declaration,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeywordFact {
    pub(crate) span: Span,
    pub(crate) role: KeywordRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceNameRole {
    Module,
    Function,
    FunctionReference,
    Type,
    Label,
    LabelReference,
    QualifiedLabelReference,
    Parameter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceNameFact {
    pub(crate) span: Span,
    pub(crate) role: SourceNameRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorSlot {
    Grammar,
    Value,
    Type,
    ModulePath,
    ImportProvider,
    Argument,
    RecursiveCallee,
    RecursiveAnnotation,
    BuildField,
    NewtypeMember,
    ImportSelection,
    TargetId,
    TargetField,
    OperatorContinuation,
    BlockLabel,
}

#[derive(Clone, Copy)]
enum DeclarationContext {
    Module,
    Signature { recursive: bool },
    TypeGroup { surface: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorAtom {
    /// The logical token after parser-owned symbol peeling, or an empty insertion.
    pub(crate) replacement: Span,
    pub(crate) prefix: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathSeparator {
    Slash,
    Dot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorPath {
    pub(crate) prefix: std::sync::Arc<[PathSegment]>,
    pub(crate) separator: Span,
    pub(crate) separator_kind: PathSeparator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorContext {
    /// The grammar interval which owns this cursor, not an edit range.
    pub(crate) span: Span,
    pub(crate) atom: CursorAtom,
    pub(crate) slot: CursorSlot,
    pub(crate) keywords: Vec<&'static str>,
    pub(crate) path: Option<CursorPath>,
    pub(crate) import: Option<ImportCursor>,
    pub(crate) target: Option<TargetCursor>,
    /// Other parsed target headers in the same build block, excluding this atom.
    pub(crate) target_ids: Vec<String>,
    pub(crate) operator_prefix: Option<Vec<String>>,
    pub(crate) call: Option<CallCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallCursor {
    pub(crate) callee: Expr,
    pub(crate) has_prior_argument: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlockCursorRegion {
    Prefix,
    Body(usize),
    Label(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockCursor {
    pub(crate) head: PathSegment,
    pub(crate) region: BlockCursorRegion,
    pub(crate) labels: Vec<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetCursor {
    pub(crate) id: String,
    pub(crate) fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportSelectionKind {
    Any,
    Name,
    Label,
    FixedOperator,
    VariadicOperator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportCursor {
    pub(crate) provider: ModulePath,
    pub(crate) kind: ImportSelectionKind,
    pub(crate) selected: Vec<ImportItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorSuppressionKind {
    Comment,
    Literal,
    Binder,
    Structural,
    #[cfg(any(test, feature = "cli"))]
    LexicalError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CursorSuppression {
    pub(crate) span: Span,
    pub(crate) kind: CursorSuppressionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopeRegion {
    pub(crate) kind: ScopeOwnerKind,
    pub(crate) open: Span,
    pub(crate) close: Span,
    /// Includes delimiter whitespace; a recovered end uses the lexical frontier.
    pub(crate) interior: Span,
    pub(crate) body: Option<Span>,
    pub(crate) recovered: bool,
    pub(crate) lambda: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScopeSyntax {
    Signature(Signature),
    TypeParameters(Vec<TypeParam>),
    RecursiveMembers {
        names: Vec<PathSegment>,
        polymorphic: bool,
    },
    RecursiveTypes(Vec<(PathSegment, Vec<TypeParam>)>),
    TypeDeclarations(Vec<PathSegment>),
    Binding {
        name: Option<(String, Span)>,
        pattern: Option<ParamPattern>,
        type_params: Vec<TypeParam>,
    },
    RowLet(Vec<crate::ast::RowLetEntry>),
    Placeholder {
        stem: PathSegment,
        slot_count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeOwnerKind {
    Body,
    Signature,
    Type,
    Declaration,
    RecursiveGroup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopePrefix {
    /// The consumed introduction token identifying the owning production.
    pub(crate) owner: Span,
    pub(crate) owner_kind: ScopeOwnerKind,
    pub(crate) interval: Span,
    pub(crate) syntax: ScopeSyntax,
}

#[derive(Debug, Default)]
pub(crate) struct ParserFacts {
    pub(crate) module_path: Option<ModulePath>,
    pub(crate) structural_forall: StructuralForallFacts,
    pub(crate) keywords: Vec<KeywordFact>,
    pub(crate) source_names: Vec<SourceNameFact>,
    pub(crate) cursor: Option<CursorContext>,
    pub(crate) block_cursor: Option<std::sync::Arc<BlockCursor>>,
    pub(crate) suppression: Option<CursorSuppression>,
    pub(crate) regions: Vec<ScopeRegion>,
    /// Type-parameter and local-binding carriers run outer-to-inner, in source order.
    pub(crate) scope_prefix: Vec<ScopePrefix>,
    pub(crate) prefix_imports: Vec<Import>,
    pub(crate) prefix_items: Vec<Item>,
    pub(crate) recovered_group: bool,
}

#[derive(Debug, Default)]
struct ParserTooling {
    facts: ParserFacts,
    cursor: Option<u32>,
    cursor_limit: u32,
    last_end: u32,
    last_span: Option<Span>,
    body_owner: Option<Span>,
    collect_roles: bool,
}

#[derive(Clone)]
struct ToolingCheckpoint {
    brackets: usize,
    unclosed: bool,
    keywords: usize,
    source_names: usize,
    cursor: Option<CursorContext>,
    block_cursor: Option<std::sync::Arc<BlockCursor>>,
    suppression: Option<CursorSuppression>,
    regions: usize,
    scope_prefix: usize,
    last_span: Option<Span>,
    body_owner: Option<Span>,
    last_end: u32,
    recovered_group: bool,
}

struct ToolingSuffix {
    brackets: Vec<u32>,
    unclosed: bool,
    keywords: Vec<KeywordFact>,
    source_names: Vec<SourceNameFact>,
    cursor: Option<CursorContext>,
    block_cursor: Option<std::sync::Arc<BlockCursor>>,
    suppression: Option<CursorSuppression>,
    regions: Vec<ScopeRegion>,
    scope_prefix: Vec<ScopePrefix>,
    last_span: Option<Span>,
    body_owner: Option<Span>,
    last_end: u32,
    recovered_group: bool,
}

#[cfg(any(test, feature = "lsp"))]
impl ExpressionParseContext {
    pub(super) fn from_module(module: &Module) -> Result<Self, Error> {
        Ok(Self {
            operator_scope: build_operator_scope(module, module.items.len())?,
        })
    }
}

#[derive(Clone, Copy)]
enum DeclarationEnd {
    Outer,
    BlockEntry,
}

pub(super) struct Parser<'a> {
    /// Cursor over the tree-skeleton CST. Replaces the raw
    /// `Vec<Token>` + `pos` pair: paren / brace boundaries come
    /// from the skeleton's balanced groups, so the parser never
    /// has to track open / close pairing itself. The cursor
    /// presents a flat token-stream view (open token, children,
    /// close token, in source order) — the parser's `peek` /
    /// `peek_at` / `advance` helpers delegate straight through.
    /// See [`crate::pass::tree_skeleton::SkeletonCursor`] for the
    /// cursor semantics.
    cursor: SkeletonCursor<'a>,
    /// Used to synthesize a span at the end of the source (for EOF errors).
    eof: u32,
    syntax_end: u32,
    /// Counter for [`crate::ast::NodeId`] issuance. Bumped on every
    /// elaboration-bearing variant (`Expr::BlockCall` / `Expr::Elaborator`)
    /// and on every `Expr::Ufcs` (whose `NodeId`
    /// carries through to the `Expr::Elaborator` the desugar pass
    /// produces for elaborator-UFCS) so the typer's `Elaborations`
    /// table has a clone-stable key. Per-parser-invocation; different
    /// `parse_*` calls don't share IDs, which is fine because each
    /// invocation produces an independent compilation unit with its
    /// own `Elaborations` table.
    next_node_id: u64,
    /// In-scope user-defined operators, keyed by operator-token
    /// content (e.g., `"+"`, `"==`"). Built incrementally as the
    /// parser walks top-level items: each `op` declaration
    /// registers its binding here, and subsequent expression
    /// parsing consults the table to recognize operator usages.
    operator_scope: OpRegistry,
    recover_import_errors: Option<Vec<Error>>,
    /// Depth of lenient `( … )` operand slots currently being
    /// parsed. When > 0, the chain-mixing check in
    /// `try_operator_continuation` bails (returns the chain so far)
    /// instead of erroring — the outer operator's continuation
    /// tokens, which may collide with another operator's leading
    /// run, are then matched by the enclosing pattern walk.
    /// Greedy `___` slots intentionally do *not* increment this
    /// depth: a `___` is always the last slot of its pattern, so
    /// there are no outer continuation tokens to leave behind.
    lenient_depth: usize,
    block_stop_depth: Option<usize>,
    /// Optional source-local tooling facts. The ordinary compiler path keeps
    /// this absent; expression probes request only structural forall facts.
    tooling: Option<ParserTooling>,
    /// Exact argument-list spans for empty direct path calls. Grouping erases
    /// its own AST node, so left-splice validation consults this parse-local
    /// provenance to distinguish `(f()).<x` from the bare `f.<x` form.
    empty_path_call_arg_lists: std::collections::HashMap<Span, Span>,
    /// Trivia after the last meaningful token, before end-of-file —
    /// taken from the skeleton (see
    /// [`crate::pass::tree_skeleton::build_with_trailing`]). The
    /// module / module-file builders stash it on the final region's
    /// trailing slot so the formatter keeps an end-of-file comment;
    /// empty for the per-item [`Parser::new_at`] fan-out, which never
    /// owns the file's end.
    file_trailing_trivia: Vec<crate::pass::lexer::Trivia>,
}

#[derive(Debug, Clone)]
pub struct LazyModule {
    module: Module,
    deferred: Vec<DeferredItem>,
}

impl LazyModule {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn force_all(&self) -> Result<Module, Error> {
        let mut module = self.module.clone();
        for deferred in &self.deferred {
            let mut item = deferred.force()?;
            let slot = module.items.get_mut(deferred.item_index).ok_or_else(|| {
                Error::parse(
                    module.meta.span,
                    "internal parser error: deferred body handle points past module item list",
                )
            })?;
            item.meta_mut().leading_trivia = slot.meta().leading_trivia.clone();
            *slot = item;
        }
        Ok(module)
    }
}

#[derive(Debug, Clone)]
pub struct ModuleFile {
    pub module: Module,
    /// The lazy parse handle for the file's module, present only when
    /// parsed through the lazy entry point. `None` for an eager parse.
    pub lazy: Option<LazyModule>,
}

impl ModuleFile {
    /// Materialize deferred bodies without reparsing the file's headers.
    pub fn force_all(self) -> Result<Self, Error> {
        if let Some(lazy) = self.lazy {
            Ok(Self {
                module: lazy.force_all()?,
                lazy: None,
            })
        } else {
            Ok(self)
        }
    }
}

#[derive(Debug, Clone)]
struct DeferredItem {
    item_index: usize,
    children: Vec<SkeletonNode>,
    operator_scope: OpRegistry,
    node_id_base: u64,
    eof: usize,
}

impl DeferredItem {
    fn force(&self) -> Result<Item, Error> {
        parse_item_from_children(
            &self.children,
            self.eof,
            self.operator_scope.clone(),
            self.node_id_base,
            ItemParseMode::Eager,
        )
        .map(|parsed| parsed.item)
    }
}

struct ParsedLazyItem {
    outer_trivia: Vec<Trivia>,
    item: Item,
    deferred: Option<DeferredItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemParseMode {
    Eager,
    Lazy,
}

const PURE_HOST_FN_DIAGNOSTIC: &str = "a host fn cannot carry `pure` because `pure` means that a function does not call host functions";
const PURE_REC_DIAGNOSTIC: &str = "`pure` cannot combine with `rec`; every recursive member's execution requires the group's declared `loop` function";

#[derive(Clone, Debug, PartialEq, Eq)]
struct ItemModifiers {
    vis: Visibility,
    vis_span: Option<Span>,
    purity: Purity,
    /// Set when the leading `host` contextual keyword introduced a
    /// `host type` / `host fn` declaration.
    host: bool,
}

struct TypeAliasHeader {
    start: u32,
    name: String,
    name_span: Span,
    type_params: Vec<TypeParam>,
}

struct FunctionHeader {
    owner: Span,
    name: String,
    name_span: Span,
    sig: Signature,
    ret: Type,
    ret_elided: bool,
    hoisted: Vec<Trivia>,
    body_open: Span,
}

struct NewtypeHeader {
    start: u32,
    name: String,
    name_span: Span,
    type_params: Vec<TypeParam>,
    existential_params: Vec<TypeParam>,
}

struct LabelsHeader {
    start: u32,
    type_alias_name: Option<String>,
    type_alias_span: Option<Span>,
    type_alias_params: Vec<TypeParam>,
}

struct LabelEntryHeader {
    name: String,
    name_span: Span,
    type_params: Vec<TypeParam>,
    existential_params: Vec<TypeParam>,
    payload_start: u32,
}

impl ItemModifiers {
    fn impure_private() -> Self {
        Self {
            vis: Visibility::Private,
            vis_span: None,
            purity: Purity::Impure,
            host: false,
        }
    }
}

/// Module-local operator scope. Two keyspaces (prefix vs. non-
/// prefix), each storing ops keyed by their op-token sequence.
///
/// **Prefix-forbidden conflict rule.** Within either keyspace, two
/// patterns may not stand in a strict prefix relationship in their
/// op-token sequences. So `_ && ++ _` and `_ && -- _` coexist
/// (sharing `&&` and branching at the third token); but `_ && _`
/// and `_ && ++ _` conflict (the first's sequence is a strict
/// prefix of the second's). At parse time, the trie walk reads
/// op-tokens forward token-by-token until it lands on a leaf
/// binding, with bounded lookahead = max declared pattern depth.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpRegistry {
    non_prefix: OpTrie,
    prefix: OpTrie,
    /// The first written shape determines parsing; semantic registry validation
    /// separately checks exact projections and ordinary introduction uniqueness.
    seen_keys: std::collections::HashSet<OperatorDispatchKey>,
    /// Names declared locally, kept separately so a prior import can defer an
    /// import/local collision without masking a second local declaration.
    local_keys: std::collections::HashSet<OperatorDispatchKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpTrie {
    /// op-token sequence → bound op. Each entry sits at a "leaf"
    /// of the conceptual trie; the prefix-forbidden rule guarantees
    /// no entry's key is a proper prefix of another's.
    leaves: std::collections::HashMap<Vec<String>, OperatorBinding>,
    /// Every *proper* prefix of every leaf key. Used both to reject
    /// inserts that would create a prefix relation and to drive the
    /// parse-time walk's "keep reading?" decision.
    prefixes: std::collections::HashSet<Vec<String>>,
}

impl OpRegistry {
    fn new() -> Self {
        Self {
            non_prefix: OpTrie::new(),
            prefix: OpTrie::new(),
            seen_keys: std::collections::HashSet::new(),
            local_keys: std::collections::HashSet::new(),
        }
    }

    /// Local declarations share a dispatch-key collision check across kinds.
    /// Diagnostics retain the incoming full grammar without storing a second
    /// copy beside the parser's key sets.
    fn begin_local_shape(
        &mut self,
        key: &OperatorDispatchKey,
        grammar: impl FnOnce() -> String,
    ) -> Result<bool, String> {
        if !self.local_keys.insert(key.clone()) {
            let grammar = grammar();
            return Err(format!(
                "operator `{grammar}` is already defined in this scope; \
                 the same expression position and leading token run may be bound only once"
            ));
        }
        Ok(self.seen_keys.insert(key.clone()))
    }

    /// Try to insert a op. Returns an error message if it
    /// conflicts with an existing binding — by name first,
    /// then by the keyspace prefix-forbidden rule.
    fn insert(&mut self, pattern: Vec<OpPart>) -> Result<(), String> {
        let name = OperatorDispatchKey::from_pattern(&pattern);
        if !self.begin_local_shape(&name, || {
            crate::ast::OperatorGrammar::fixed(&pattern).render()
        })? {
            return Ok(());
        }
        self.insert_pattern_shape(pattern)
    }

    fn insert_pattern_shape(&mut self, pattern: Vec<OpPart>) -> Result<(), String> {
        let is_prefix = matches!(pattern.first(), Some(OpPart::Token { .. }));
        let trie = if is_prefix {
            &mut self.prefix
        } else {
            &mut self.non_prefix
        };
        trie.insert(pattern)
    }

    /// Register a variadic operator keyed by its OPEN-token sequence.
    /// A duplicate dispatch key, or another variadic
    /// binding sharing the same OPEN sequence, is a name conflict.
    fn insert_variadic(
        &mut self,
        open: Vec<String>,
        binding: crate::ast::VariadicSpec,
    ) -> Result<(), String> {
        let name = OperatorDispatchKey {
            expr_start: true,
            leading_run: open.clone(),
        };
        if !self.begin_local_shape(&name, || {
            crate::ast::OperatorGrammar::variadic(&open, &binding).render()
        })? {
            return Ok(());
        }
        Ok(())
    }

    /// Walk forward from the parser's current position, reading
    /// op-tokens until a leaf matches in the given keyspace. Returns
    /// `Some((binding, n))` where `n` is the number of input tokens
    /// to consume after the LHS (or all input tokens consumed when
    /// `is_prefix` is true).
    fn find_at(&self, parser: &Parser<'_>, is_prefix: bool) -> Option<(OperatorBinding, usize)> {
        let trie = if is_prefix {
            &self.prefix
        } else {
            &self.non_prefix
        };
        trie.find_at(parser)
    }

    fn cursor_prefix(
        &self,
        parser: &Parser<'_>,
        is_prefix: bool,
        cursor: u32,
    ) -> Option<(Span, Vec<String>)> {
        let trie = if is_prefix {
            &self.prefix
        } else {
            &self.non_prefix
        };
        for key in trie.leaves.keys() {
            let mut written = Vec::new();
            let start = parser.peek_span().start;
            for (index, expected) in key.iter().enumerate() {
                let Some(token) = parser.peek_token_at(index) else {
                    if !written.is_empty() && cursor >= start {
                        return Some((Span::new(start, cursor), written));
                    }
                    break;
                };
                if token.span.start > cursor {
                    if !written.is_empty() {
                        return Some((Span::new(start, cursor), written));
                    }
                    break;
                }
                let Some(content) = token_kind_to_op_string(&token.kind) else {
                    break;
                };
                if cursor <= token.span.end {
                    let prefix = &content[..(cursor - token.span.start) as usize];
                    if expected.starts_with(prefix) {
                        written.push(prefix.to_owned());
                        return Some((Span::new(start, token.span.end), written));
                    }
                    break;
                }
                if &content != expected {
                    break;
                }
                written.push(content);
            }
        }
        None
    }

    /// Look up a non-prefix operator whose leading token run begins
    /// `offset` tokens after the parser's current position. Expression-prefix
    /// disambiguation uses this to distinguish `module/path.item` from a
    /// registered `/` operator applied to a dotted right operand without
    /// consuming either interpretation first.
    fn find_non_prefix_at_offset(
        &self,
        parser: &Parser<'_>,
        offset: usize,
    ) -> Option<(OperatorBinding, usize)> {
        self.non_prefix.find_at_offset(parser, offset)
    }

    /// Is the next non-prefix op at the parser's current position
    /// left-greedy (i.e. its first slot is `___`)? Drives the outer
    /// `expr()` loop that lets a left-greedy operator attach to
    /// any preceding expression, even one settled into a chain.
    fn next_is_left_greedy(&self, parser: &Parser<'_>) -> bool {
        match self.non_prefix.find_at(parser) {
            Some((binding, _)) => {
                matches!(binding.pattern.first(), Some(OpPart::SlotGreedy { .. }))
            }
            None => false,
        }
    }
}

fn insert_consumer_grammars(scope: &mut OpRegistry, import_clause: &Import) -> Result<(), Error> {
    let ImportKind::Selective { items, .. } = &import_clause.kind else {
        return Ok(());
    };
    for item in items {
        let ImportItem::OperatorPattern { grammar, span, .. } = item else {
            continue;
        };
        if !scope.seen_keys.insert(grammar.dispatch_key()) {
            continue;
        }
        let result = match grammar {
            crate::ast::OperatorGrammar::Fixed(pattern) => {
                scope.insert_pattern_shape(pattern.clone())
            }
            crate::ast::OperatorGrammar::Variadic { .. } => Ok(()),
        };
        result.map_err(|message| Error::parse(*span, message))?;
    }
    Ok(())
}

#[cfg(any(test, feature = "lsp"))]
fn build_operator_scope(module: &Module, local_end: usize) -> Result<OpRegistry, Error> {
    let mut scope = OpRegistry::new();
    for import_clause in &module.imports {
        insert_consumer_grammars(&mut scope, import_clause)?;
    }
    for item in module.items.iter().take(local_end) {
        match item {
            Item::Op(op, _) => {
                let OpBody::Normal { pattern, .. } = &op.body;
                scope
                    .insert(pattern.clone())
                    .map_err(|message| Error::parse(op.meta.span, message))?;
            }
            Item::VariadicOperator(fold, _) => {
                scope
                    .insert_variadic(fold.open.clone(), (*fold.spec).clone())
                    .map_err(|message| Error::parse(fold.meta.span, message))?;
            }
            _ => {}
        }
    }
    Ok(scope)
}

impl OpTrie {
    fn new() -> Self {
        Self {
            leaves: std::collections::HashMap::new(),
            prefixes: std::collections::HashSet::new(),
        }
    }

    fn insert(&mut self, pattern: Vec<OpPart>) -> Result<(), String> {
        let key = leading_op_run(&pattern);
        debug_assert!(
            !key.is_empty(),
            "validated patterns always include at least one leading op-token"
        );
        if self.leaves.contains_key(&key) {
            return Err(format!(
                "operator `{}` is already defined in this scope; \
                 each operator binds to exactly one function",
                crate::ast::OperatorGrammar::fixed(&pattern).render(),
            ));
        }
        // Existing entry is a proper prefix of the new key?
        for n in 1..key.len() {
            let prefix = &key[..n];
            if let Some(existing) = self.leaves.get(prefix) {
                return Err(format!(
                    "operator `{}` conflicts with the existing \
                     `{}` (op-token sequences must not have a \
                     prefix relationship)",
                    crate::ast::OperatorGrammar::fixed(&pattern).render(),
                    crate::ast::OperatorGrammar::fixed(&existing.pattern).render(),
                ));
            }
        }
        // New key is itself a proper prefix of some existing entry?
        if self.prefixes.contains(&key) {
            return Err(format!(
                "operator `{}` conflicts with an existing longer \
                 operator that starts with the same op-tokens \
                 (op-token sequences must not have a prefix relationship)",
                crate::ast::OperatorGrammar::fixed(&pattern).render(),
            ));
        }

        for n in 1..key.len() {
            self.prefixes.insert(key[..n].to_vec());
        }
        self.leaves.insert(key, OperatorBinding { pattern });
        Ok(())
    }

    fn find_at(&self, parser: &Parser<'_>) -> Option<(OperatorBinding, usize)> {
        self.find_at_offset(parser, 0)
    }

    fn find_at_offset(
        &self,
        parser: &Parser<'_>,
        offset: usize,
    ) -> Option<(OperatorBinding, usize)> {
        let mut seq: Vec<String> = Vec::new();
        let mut depth = 0usize;
        loop {
            let token_str = parser
                .peek_at(offset + depth)
                .as_ref()
                .and_then(token_kind_to_op_string)?;
            seq.push(token_str);
            depth += 1;
            if let Some(binding) = self.leaves.get(&seq) {
                return Some((binding.clone(), depth));
            }
            if !self.prefixes.contains(&seq) {
                return None;
            }
        }
    }
}

/// An in-scope user-defined operator. Records the function the
/// operator desugars to plus the full pattern (alternating slots
/// and operator tokens). The first token of the pattern is what
/// the parser keys lookup on:
///
/// - **Prefix** patterns (start with a token, e.g., `! __`) are
///   keyed for use at the start of a primary expression.
/// - **Non-prefix** patterns (start with a slot, e.g., `_ + _`,
///   `__ + _`, `_ ? _ : _`) are keyed for use after a left-hand-
///   side has been parsed.
///
/// A given operator-token string can appear in at most one
/// prefix binding and at most one non-prefix binding.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OperatorBinding {
    /// The op's exact declaration pattern. Imported bindings are installed
    /// from their provider before body parsing.
    pattern: Vec<OpPart>,
}

fn same_operator_binding(left: &OperatorBinding, right: &OperatorBinding) -> bool {
    OperatorDispatchKey::from_pattern(&left.pattern)
        == OperatorDispatchKey::from_pattern(&right.pattern)
}

// A literal operator token in user source is a `SymbolRun` matched
// against the next input token. Operator-character runs (`+`, `?`,
// `++`, `.+.`) and fixed-role symbols (`!`, `:`, `&`, `|`, `<`,
// `>`, `=`) are all the one token kind. The validator forbids a
// symbol run as the sole pattern-token for reserved standalone forms
// (`=`, `:`, `->`, `.>`, `.>>`, `.<`, `.<<`, and all dot-led
// one-dot runs); any of them is fine in a multi-token shape.
//
/// Discriminator for which type-chain we're inside — product
/// (`&`-runs) or sum (`|`-runs). Used by `type_expr` and
/// `type_paren` to thread the chain operator through helpers
/// without naming a specific token kind (since `&` / `&&` / `&&&`
/// / … all lex as one `SymbolRun`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainKind {
    Amp,
    Pipe,
}

/// Discriminator for the leading-operator inside a `(` … `)`
/// type expression: a product or sum chain op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeadingOp {
    Chain(ChainKind),
}

/// True iff `t` is an admissible product-chain separator: a
/// [`TokenKind::SymbolRun`] whose content is all `&` characters
/// (the run-collapse rule per `specs/grammar.md` — `&` / `&&` /
/// `&&&` / … in type position collapse to a single product
/// separator).
fn chain_sep_prefix_len(run: &str, sep: char) -> Option<usize> {
    let len = run.chars().take_while(|c| *c == sep).count();
    if len == 0 {
        return None;
    }
    if len == run.chars().count() || run.chars().nth(len) == Some('.') {
        Some(len)
    } else {
        None
    }
}

fn is_amp_chain_sep(t: &TokenKind) -> bool {
    matches!(t, TokenKind::SymbolRun(s) if chain_sep_prefix_len(s, '&').is_some())
}

/// True iff `t` is an admissible sum-chain separator. Mirrors
/// [`is_amp_chain_sep`] but for `|`.
fn is_pipe_chain_sep(t: &TokenKind) -> bool {
    matches!(t, TokenKind::SymbolRun(s) if chain_sep_prefix_len(s, '|').is_some())
}

/// The op-string a token contributes to an operator pattern. Every
/// [`TokenKind::SymbolRun`] (`+`, `?`, `++`, `#`, fixed-role
/// `< > [ ] : = ! ->`, dot-splice tokens, …) yields its spelling.
/// Everything else — identifiers, literals, structural punctuators
/// — contributes nothing.
fn token_kind_to_op_string(kind: &TokenKind) -> Option<String> {
    match kind {
        TokenKind::SymbolRun(s) => Some(s.clone()),
        _ => None,
    }
}

fn leading_dot_dot_count(s: &str) -> usize {
    if s.starts_with('.') {
        s.bytes().filter(|b| *b == b'.').count()
    } else {
        0
    }
}

/// True iff a run is held back for dot-led syntax instead of being
/// admissible as a user operator token. Runs that begin with `.` must
/// contain at least two dots (`..`, `.+.`); runs that contain `.` away
/// from the first byte (`+.` / `<.>`) are ordinary op-tokens.
fn is_reserved_leading_dot_op_token(s: &str) -> bool {
    s.starts_with('.') && leading_dot_dot_count(s) == 1
}

fn mirrored_varop_close(open: &str) -> Option<String> {
    if open.len() < 2
        || !open.contains('[')
        || open.contains(']')
        || is_reserved_leading_dot_op_token(open)
    {
        return None;
    }
    let close: String = open
        .chars()
        .rev()
        .map(|c| if c == '[' { ']' } else { c })
        .collect();
    (!is_reserved_leading_dot_op_token(&close)).then_some(close)
}

fn reserved_leading_dot_op_token_error(content: &str, span: Span) -> Error {
    Error::parse(
        span,
        format!(
            "`{content}` is reserved as an `op` operator-token — \
             operator tokens that start with `.` must contain at least two `.` characters. \
             Dot-led one-dot runs are held back for member access, UFCS, lambdas, \
             placeholder lambdas, and future dot-led syntax. Use a spelling such as \
             `..`, `.+.`, `<.>`, or `+.` instead."
        ),
    )
}

fn is_reserved_standalone_equals_op_token(s: &str) -> bool {
    s == "="
}

fn reserved_standalone_equals_op_token_error(span: Span) -> Error {
    Error::parse(
        span,
        "standalone `=` is reserved as an unquoted `op` operator-token; \
         use operator-token quotation `(=)` in a fixed-arity pattern"
            .to_owned(),
    )
}

impl<'a> Parser<'a> {
    pub(super) fn new(skeleton: &'a SkeletonFile, source_len: usize) -> Self {
        let eof = source_len as u32;
        Self {
            cursor: SkeletonCursor::new(skeleton, eof),
            eof,
            syntax_end: 0,
            next_node_id: 0,
            operator_scope: OpRegistry::new(),
            recover_import_errors: None,
            lenient_depth: 0,
            block_stop_depth: None,
            tooling: None,
            empty_path_call_arg_lists: std::collections::HashMap::new(),
            file_trailing_trivia: skeleton.trailing_trivia.clone(),
        }
    }

    pub(super) fn new_lazy(skeleton: &'a SkeletonFile, source_len: usize) -> Self {
        Self::new(skeleton, source_len)
    }

    pub(super) fn module_recover_imports(&mut self) -> Result<(Module, Vec<Error>), Error> {
        self.recover_import_errors = Some(Vec::new());
        let module = self.module()?;
        let errors = self.recover_import_errors.take().expect("recovery enabled");
        Ok((module, errors))
    }

    #[cfg(any(test, feature = "cli"))]
    pub(super) fn module_tooling(&mut self) -> Result<Module, Error> {
        self.recover_import_errors = Some(Vec::new());
        let parsed = self.module();
        let errors = self.recover_import_errors.take().expect("recovery enabled");
        if let Some(error) = errors.into_iter().next() {
            if let Ok(module) = parsed
                && let Some(tooling) = &mut self.tooling
            {
                tooling.facts.prefix_imports = module.imports;
                tooling.facts.prefix_items = module.items;
            }
            Err(error)
        } else {
            parsed
        }
    }

    #[cfg(any(test, feature = "cli"))]
    pub(super) fn new_with_expression_parse_context(
        skeleton: &'a SkeletonFile,
        source_len: usize,
        context: &ExpressionParseContext,
    ) -> Self {
        let mut parser = Self::new(skeleton, source_len);
        parser.operator_scope = context.operator_scope.clone();
        parser
    }

    /// Build a parser over a slice of skeleton children (rather
    /// than a whole file). Used by per-item rayon fan-out where
    /// each task parses one item's subtree independently.
    pub(super) fn new_at(
        children: &'a [crate::pass::tree_skeleton::SkeletonNode],
        source_len: usize,
    ) -> Self {
        let eof = source_len as u32;
        Self {
            cursor: SkeletonCursor::new_at(children, eof),
            eof,
            syntax_end: 0,
            next_node_id: 0,
            operator_scope: OpRegistry::new(),
            recover_import_errors: None,
            lenient_depth: 0,
            block_stop_depth: None,
            tooling: None,
            empty_path_call_arg_lists: std::collections::HashMap::new(),
            file_trailing_trivia: Vec::new(),
        }
    }

    /// Issue a fresh [`crate::ast::NodeId`] for an
    /// elaboration-bearing variant. See the field's docstring for
    /// why this lives on `Parser`.
    fn fresh_node_id(&mut self) -> crate::ast::NodeId {
        let id = self.next_node_id;
        self.next_node_id = self
            .next_node_id
            .checked_add(1)
            .expect("NodeId counter overflow — this would require >2^64 elaboration-bearing nodes in one parse, which can't happen in practice");
        crate::ast::NodeId(id)
    }

    // ---- low-level cursor ------------------------------------------------

    fn peek(&self) -> Option<&TokenKind> {
        self.cursor.peek_kind()
    }

    /// Lookahead: is the token `offset` positions ahead exactly `kind`?
    /// Used for grammar rules that decide on a 2-token prefix (e.g.,
    /// "is this a `name: T` value param, or a type-only param?").
    fn peek_kind_at(&self, offset: usize, kind: &TokenKind) -> bool {
        self.cursor
            .peek_kind_at(offset)
            .map(|k| &k == kind)
            .unwrap_or(false)
    }

    /// Lookahead returning the [`TokenKind`] at `offset`, or `None`
    /// past EOF. Used by the operator-scope trie walk, which reads
    /// multiple op-tokens before deciding which op matches.
    fn peek_at(&self, offset: usize) -> Option<TokenKind> {
        self.cursor.peek_kind_at(offset)
    }

    fn peek_token_at(&self, offset: usize) -> Option<Token> {
        let mut cursor = self.cursor.clone();
        for _ in 0..offset {
            cursor.advance()?;
        }
        cursor.peek_token().cloned()
    }

    fn peek_adjacent_bang_after_ident(&self) -> bool {
        let (Some(ident), Some(bang)) = (self.peek_token_at(0), self.peek_token_at(1)) else {
            return false;
        };
        Self::adjacent_bang_tokens(&ident, &bang)
    }

    fn adjacent_bang_tokens(ident: &Token, bang: &Token) -> bool {
        matches!((&ident.kind, &bang.kind), (TokenKind::Ident(_), TokenKind::SymbolRun(run))
            if run.starts_with('!') && bang.span.start == ident.span.end)
    }

    fn peek_span(&self) -> Span {
        self.cursor.peek_span()
    }

    /// Clone the leading trivia (newlines + line comments) of the
    /// next token. Used at boundaries where the parser wants to
    /// capture the comments preceding a top-level item or `import` so
    /// the formatter can preserve them through a `kio fmt`
    /// round-trip. At EOF, returns an empty vec — no token, no
    /// trivia.
    fn peek_leading_trivia(&self) -> Vec<crate::pass::lexer::Trivia> {
        self.cursor.peek_leading_trivia()
    }

    /// Consume and return the next token. It is exactly the token
    /// `peek()` last reported, so a caller that matched a `TokenKind`
    /// variant via `peek` and then re-destructures this token's `kind`
    /// with a let-else can reach the `else` only if `peek` / `advance`
    /// disagree — a bug in this file, hence the bare `unreachable!()`.
    fn advance(&mut self) -> Token {
        let token = self
            .cursor
            .advance()
            .expect("advance called past end of input");
        self.syntax_end = token.span.end;
        if let Some(tooling) = &mut self.tooling {
            tooling.last_end = token.span.end;
            tooling.last_span = Some(token.span);
            tooling.facts.recovered_group |= self.cursor.just_entered_recovered();
        }
        token
    }

    fn split_current_sym(&mut self, bytes: usize) -> Token {
        let token = self.cursor.split_current_sym(bytes);
        self.syntax_end = token.span.end;
        if let Some(tooling) = &mut self.tooling {
            tooling.last_end = token.span.end;
            tooling.last_span = Some(token.span);
            if let Some(cursor) = tooling.cursor
                && token.span.start <= cursor
                && cursor < token.span.end
            {
                tooling.facts.cursor = None;
                tooling.facts.suppression = Some(CursorSuppression {
                    span: token.span,
                    kind: CursorSuppressionKind::Structural,
                });
            }
            if let Some(context) = &mut tooling.facts.cursor
                && context.atom.replacement.start == token.span.start
                && token.span.end <= context.atom.prefix.end
            {
                context.atom.replacement.start = token.span.end;
                context.atom.prefix.start = token.span.end;
            }
        }
        token
    }

    fn at_end(&self) -> bool {
        self.cursor.at_end()
    }

    pub(super) fn expect_eof(&self) -> Result<(), Error> {
        if self.at_end() {
            Ok(())
        } else {
            Err(self.err_here("unexpected trailing tokens after module"))
        }
    }

    fn err(&self, span: Span, msg: impl Into<String>) -> Error {
        Error::parse(span, msg)
    }

    fn err_here(&self, msg: impl Into<String>) -> Error {
        self.err(self.peek_span(), msg)
    }

    /// Consume `kind` (a punctuation/keyword variant carrying no data); on
    /// mismatch, error with `expected <what>`.
    ///
    /// When `kind` is `LParen` or `LBrace` and the consumed token
    /// opens a *recovered* group (the skeleton builder didn't see
    /// a matching close before the surrounding context ended),
    /// returns an `Error::Parse` scoped to the whole group's
    /// source range. The parser's recursive descent unwinds out
    /// of the malformed subtree, leaving the cursor positioned
    /// inside the recovered group — `module()` re-syncs by
    /// catching the error at the top-level item boundary.
    fn expect_kind(&mut self, kind: TokenKind, what: &str) -> Result<Span, Error> {
        if matches!(kind, TokenKind::RBrace) {
            // Keep an unclaimed cursor with the closing grammar owner, including
            // trivia before a recovered closer, instead of a surrounding block.
            self.cursor_choices(CursorSlot::Grammar, &[]);
        }
        match self.peek() {
            Some(k) if *k == kind => {
                let span = self.peek_span();
                self.advance();
                // A live expression probe deliberately descends into a
                // recovered group so the ordinary grammar can report facts
                // from its incomplete contents (for example `f([A`). Normal
                // compilation has no fact sink and preserves the immediate
                // recovered-group diagnostic.
                if self.cursor.just_entered_recovered() && self.tooling.is_none() {
                    let end = self
                        .cursor
                        .current_group_close_span()
                        .map(|s| s.end)
                        .unwrap_or(span.end);
                    let group_span = Span::new(span.start, end);
                    let delim = match kind {
                        TokenKind::LParen => "`(`",
                        TokenKind::LBrace => "`{`",
                        _ => "open delimiter",
                    };
                    return Err(self.err(
                        group_span,
                        format!("unterminated {delim} — group is missing its matching closer"),
                    ));
                }
                Ok(span)
            }
            _ => Err(self.err_here(format!("expected {what}"))),
        }
    }

    /// Consume the [`TokenKind::SymbolRun`] spelled exactly `s`; on
    /// mismatch, error with `expected <what>`. The single-symbol
    /// counterpart of [`expect_kind`] — used everywhere the grammar
    /// pins one fixed-role symbol (`=`, `:`, `->`, `<`, `>`, `[`,
    /// `]`).
    fn expect_sym(&mut self, s: &str, what: &str) -> Result<Span, Error> {
        self.expect_kind(TokenKind::sym(s), what)
    }

    /// True iff the next token is the [`TokenKind::SymbolRun`]
    /// spelled exactly `s`.
    fn at_sym(&self, s: &str) -> bool {
        self.peek().is_some_and(|k| k.is_sym(s))
    }

    /// True iff the next token is the [`TokenKind::SymbolRun`]
    /// whose content starts with `s`. Used by the structural-
    /// recovery sites (function arrow `->`, forall brackets,
    /// existential closer `>`, bang-call dispatch `!`) to recognize their delimiter
    /// even when greedy lexer fusion pulled it into a longer run
    /// (`->!`, `>:`, `><`, `!.`). The `expect_*` peelers
    /// downstream do the actual split.
    fn at_sym_prefix(&self, s: &str) -> bool {
        self.peek().is_some_and(|k| match k {
            TokenKind::SymbolRun(run) => run.starts_with(s),
            _ => false,
        })
    }

    /// True iff the next logical token is a symbol run beginning
    /// with `.`. The leading-dot parser selects the structural form.
    fn at_leading_dot_prefix(&self) -> bool {
        matches!(self.peek(), Some(TokenKind::SymbolRun(run)) if run.starts_with('.'))
    }

    /// Consume the leading `.` of a leading-dot form. When greedy
    /// lexing fused the dot with the following symbol (`.<`,
    /// `.&`, ...), peel only the dot and leave the suffix as the
    /// current token for the existing parser path.
    fn expect_leading_dot(&mut self) -> Result<Span, Error> {
        match self.peek() {
            Some(TokenKind::SymbolRun(run)) if run == "." => Ok(self.advance().span),
            Some(TokenKind::SymbolRun(run)) if run.starts_with('.') && run.len() > 1 => {
                Ok(self.split_current_sym(1).span)
            }
            _ => Err(self.err_here("expected `.`")),
        }
    }

    /// Return the token immediately after the balanced paren group
    /// whose opening `(` sits `open_offset` tokens ahead.
    #[cfg(any(test, feature = "cli"))]
    fn peek_after_paren_group_at(&self, open_offset: usize) -> Option<TokenKind> {
        if !matches!(self.peek_at(open_offset), Some(TokenKind::LParen)) {
            return None;
        }
        let mut depth: i32 = 0;
        let mut offset = open_offset;
        loop {
            match self.peek_at(offset) {
                Some(TokenKind::LParen) => depth += 1,
                Some(TokenKind::RParen) => {
                    depth -= 1;
                    if depth == 0 {
                        return self.peek_at(offset + 1);
                    }
                }
                Some(_) => {}
                None => return None,
            }
            offset += 1;
        }
    }

    fn token_is_statement_boundary(tok: Option<TokenKind>) -> bool {
        matches!(
            &tok,
            None | Some(TokenKind::Comma)
                | Some(TokenKind::RParen)
                | Some(TokenKind::RBrace)
                | Some(TokenKind::Semicolon)
        ) || matches!(tok, Some(TokenKind::SymbolRun(run))
            if run.contains(']') && !run.contains('[')
                && mirrored_varop_close(&run.chars().rev().map(|c| if c == ']' { '[' } else { c }).collect::<String>()).is_some())
    }

    fn rec_call_starts_here(&mut self) -> bool {
        let mut lookahead = self.cursor.clone();
        let marker = lookahead.advance().expect("caller matched `rec`");
        let mut annotation_prefix = true;
        let end = if matches!(lookahead.peek_kind(), Some(TokenKind::LParen)) {
            let depth = lookahead.depth();
            lookahead.advance();
            let close = lookahead
                .current_group_close_span()
                .expect("lookahead entered a parenthesized group");
            if close.start == close.end {
                return true;
            }
            let mut modes = Vec::new();
            let mut expects_mode = true;
            while lookahead.depth() > depth {
                let token = lookahead.advance().expect("group has a closing token");
                if lookahead.depth() == depth {
                    break;
                }
                match token.kind {
                    TokenKind::Ident(name) if expects_mode => {
                        if let Some(mode) = RecCallMode::from_annotation(&name)
                            && mode != RecCallMode::Escape
                            && !modes.contains(&mode)
                        {
                            modes.push(mode);
                        } else {
                            annotation_prefix = false;
                        }
                        expects_mode = false;
                    }
                    TokenKind::Comma => expects_mode = true,
                    _ => annotation_prefix = false,
                }
            }
            annotation_prefix &= !modes.is_empty();
            close.end
        } else {
            marker.span.end
        };
        let after = lookahead.peek_kind().cloned();
        if let Some(TokenKind::Ident(name)) = &after {
            let span = lookahead.peek_span();
            if Self::validate_value_name(name, span).is_ok() {
                self.record_source_name(span, SourceNameRole::FunctionReference);
            }
            return true;
        }
        // A cursor in the trailing gap can still introduce a recursion callee;
        // a completed ordinary reference or call does not bind the marker.
        annotation_prefix
            && Self::token_is_statement_boundary(after)
            && self.tooling.as_ref().is_some_and(|tooling| {
                tooling.cursor.is_some_and(|cursor| {
                    end < cursor
                        && lookahead.peek_token().is_none_or(|token| {
                            cursor <= token.span.start || token.span.start == token.span.end
                        })
                })
            })
    }

    fn let_statement_starts_here(&self) -> bool {
        if !self.peek_ident_named("let") {
            return false;
        }
        match self.peek_at(1) {
            Some(TokenKind::Ident(_)) | Some(TokenKind::Slot1) => true,
            Some(TokenKind::SymbolRun(run)) if run == "." => {
                matches!(self.peek_at(2), Some(TokenKind::LParen))
            }
            _ => false,
        }
    }

    /// Consume `->` either as a standalone two-char SymbolRun or
    /// as the leading bytes of a fused run (`->!`, `->&`, `->|`,
    /// `->=`, …). At the second shape, peels `->` off the front
    /// via [`SkeletonCursor::split_current_sym`] and leaves the
    /// suffix as the new "current" for the next consumer. Used
    /// at the function-arrow recognition sites (`.foo -> T { … }`,
    /// `T -> R`, `fn name(p0: …) -> R;`).
    fn expect_fn_arrow(&mut self) -> Result<Span, Error> {
        match self.peek() {
            Some(TokenKind::SymbolRun(s)) if s == "->" => Ok(self.advance().span),
            Some(TokenKind::SymbolRun(s)) if s.starts_with("->") => {
                Ok(self.split_current_sym(2).span)
            }
            _ => Err(self.err_here("expected `->`")),
        }
    }

    /// Consume `>` either as a standalone one-char SymbolRun or
    /// as the leading byte of a fused run (`>:`, `><`, `>=`, …).
    /// At the second shape, peels `>` off the front via
    /// [`SkeletonCursor::split_current_sym`] and leaves the
    /// suffix as the new "current" for the next consumer. Used
    /// at the existential-binder closer (the `>` in `<U>`).
    fn expect_existential_close(&mut self) -> Result<Span, Error> {
        match self.peek() {
            Some(TokenKind::SymbolRun(s)) if s == ">" => Ok(self.advance().span),
            Some(TokenKind::SymbolRun(s)) if s.starts_with('>') => {
                Ok(self.split_current_sym(1).span)
            }
            _ => Err(self.err_here("expected `>`")),
        }
    }

    /// Consume `!` either as a standalone one-char SymbolRun or
    /// as the leading byte of a fused run (`!.`, `!=`, …). At
    /// the second shape, peels `!` off the front via
    /// [`SkeletonCursor::split_current_sym`] and leaves the
    /// suffix as the new "current" for the next consumer. Used
    /// at the bang-call recognition sites (`iso!` / `into!` /
    /// `onto!` / `align!` / `atom!`, and dot-splice elaborator suffixes
    /// like `r.>iso!` / `r.>>iso!`).
    fn expect_bang(&mut self) -> Result<Span, Error> {
        match self.peek() {
            Some(TokenKind::SymbolRun(s)) if s == "!" => Ok(self.advance().span),
            Some(TokenKind::SymbolRun(s)) if s.starts_with('!') => {
                Ok(self.split_current_sym(1).span)
            }
            _ => Err(self.err_here("expected `!`")),
        }
    }

    fn expect_ident(&mut self) -> Result<(String, Span), Error> {
        match self.peek() {
            Some(TokenKind::Ident(_)) => {
                let tok = self.advance();
                let TokenKind::Ident(name) = tok.kind else {
                    unreachable!()
                };
                Ok((name, tok.span))
            }
            // Pure-underscore slot tokens are admissible as the
            // wildcard-discard binder name (`_`). Slot2/Slot3
            // (`__`/`___`) are *not* admitted as identifiers — those
            // are slot-only tokens — so an attempt to bind them is
            // a parse error caught here. The grammar requires `_`
            // for the wildcard-discard role; `__` and `___` are
            // structurally reserved for op slot kinds.
            Some(TokenKind::Slot1) => {
                let tok = self.advance();
                Ok(("_".to_owned(), tok.span))
            }
            _ => Err(self.err_here("expected identifier")),
        }
    }

    fn expect_binder(&mut self) -> Result<(String, Span), Error> {
        self.suppress_current_cursor(CursorSuppressionKind::Binder);
        self.expect_ident()
    }

    /// Consume an `Ident` whose text is exactly `expected`. Used for the
    /// contextual keywords `pub`, `from`, `as`, `target`, `role`, etc.
    fn expect_ident_named(&mut self, expected: &str) -> Result<Span, Error> {
        self.expect_ident_role(expected, KeywordRole::Declaration)
    }

    fn expect_ident_role(&mut self, expected: &str, role: KeywordRole) -> Result<Span, Error> {
        match self.peek() {
            Some(TokenKind::Ident(s)) if s == expected => {
                let span = self.peek_span();
                self.advance();
                self.record_keyword(span, role);
                Ok(span)
            }
            _ => Err(self.err_here(format!("expected `{expected}`"))),
        }
    }

    fn parse_visibility_prefix(&mut self) -> Result<(Visibility, Option<Span>), Error> {
        if self.peek_ident_named("pub") {
            let span = self.peek_span();
            self.advance();
            self.record_keyword(span, KeywordRole::Declaration);
            let vis = if self.peek_kind_at(0, &TokenKind::LParen) {
                self.expect_kind(TokenKind::LParen, "`(`")?;
                let path = self.module_path()?;
                self.expect_kind(TokenKind::RParen, "`)`")?;
                Visibility::PublicIn(path)
            } else {
                Visibility::Public
            };
            Ok((vis, Some(span)))
        } else {
            Ok((Visibility::Private, None))
        }
    }

    fn peek_ident_named(&self, expected: &str) -> bool {
        matches!(self.peek(), Some(TokenKind::Ident(s)) if s == expected)
    }

    /// Lookahead variant of [`Self::peek_ident_named`]: the token at
    /// `offset` is the bare identifier `expected`.
    fn peek_ident_named_at(&self, offset: usize, expected: &str) -> bool {
        matches!(self.peek_at(offset), Some(TokenKind::Ident(s)) if s == expected)
    }

    /// Consume zero or more consecutive comma tokens. Returns `true` if
    /// at least one comma was advanced past. Comma-separated productions
    /// in Kio accept any number of commas in any position — leading,
    /// repeated between items, and trailing — all collapse to the same
    /// AST as the canonical one-comma-per-separator layout. The
    /// formatter relies on this to emit the leading-comma multi-line
    /// style (`( , a , b , c )`) without distinguishing it from the
    /// inline (`(a, b, c)`) shape at the AST level.
    fn skip_commas(&mut self) -> bool {
        let mut any = false;
        while matches!(self.peek(), Some(TokenKind::Comma)) {
            self.advance();
            any = true;
        }
        any
    }

    /// Append only the comment trivia from `run` to `into`, dropping
    /// newlines. Inter-token comments are hoisted to their owning
    /// construct; boundary comments keep a dedicated structural slot.
    fn collect_comments_into(run: Vec<Trivia>, into: &mut Vec<Trivia>) {
        for t in run {
            if matches!(
                t,
                Trivia::LineComment { .. } | Trivia::DocCommentLine { .. }
            ) {
                into.push(t);
            }
        }
    }

    /// Consume a run of `;` clause separators and return trivia that
    /// was attached to the skipped separator tokens. Callers prepend
    /// the returned trivia to the next real clause so comments stay
    /// with the clause they precede even when repeated separators are
    /// canonicalized away.
    fn skip_semicolon_separators(&mut self) -> Vec<Trivia> {
        let mut trivia = Vec::new();
        while matches!(self.peek(), Some(TokenKind::Semicolon)) {
            trivia.extend(self.peek_leading_trivia());
            self.advance();
        }
        trivia
    }

    fn declaration_end(&mut self, end: DeclarationEnd, expected: &str) -> Result<u32, Error> {
        match end {
            DeclarationEnd::Outer => Ok(self.expect_kind(TokenKind::Semicolon, expected)?.end),
            DeclarationEnd::BlockEntry => Ok(self.syntax_end),
        }
    }

    fn optional_outer_semicolon(&mut self) -> Vec<Trivia> {
        if matches!(self.peek(), Some(TokenKind::Semicolon)) {
            let trivia = self.peek_leading_trivia();
            self.advance();
            trivia
        } else {
            Vec::new()
        }
    }

    fn leading_block_separator(&mut self, repeated: bool) -> Result<Vec<Trivia>, Error> {
        if repeated {
            return Ok(self.skip_semicolon_separators());
        }
        let has_separator = matches!(self.peek(), Some(TokenKind::Semicolon));
        let trivia = self.optional_outer_semicolon();
        if has_separator && matches!(self.peek(), Some(TokenKind::RBrace | TokenKind::Semicolon)) {
            return Err(self.err_here("expected a block entry after the leading `;`"));
        }
        Ok(trivia)
    }

    /// The completed child supplies the insertion position; no unfinished
    /// expression or declaration is repaired by this boundary operation.
    fn block_separator(
        &mut self,
        repeated: bool,
        eof_closes: bool,
        heads: &[&str],
    ) -> Result<Vec<Trivia>, Error> {
        if matches!(self.peek(), Some(TokenKind::RBrace)) || (eof_closes && self.at_end()) {
            return Ok(Vec::new());
        }
        if !matches!(self.peek(), Some(TokenKind::Semicolon)) {
            let insertion = Span::new(self.syntax_end, self.syntax_end);
            let error = self.err_here("expected `;` between block entries");
            let starts_entry = if heads.is_empty() {
                matches!(self.peek(), Some(TokenKind::Ident(_)))
            } else {
                heads.iter().any(|head| self.peek_ident_named(head))
            };
            return Err(if starts_entry {
                error.with_fix(Fix::machine_applicable(
                    "Insert `;` between block entries",
                    vec![FixEdit::new(insertion, ";")],
                ))
            } else {
                error
            });
        }
        let mut trivia = self.optional_outer_semicolon();
        if repeated {
            trivia.extend(self.skip_semicolon_separators());
        }
        Ok(trivia)
    }

    /// Consume one StrLit token and any immediately-following adjacent
    /// StrLits, returning the concatenated value and the span covering
    /// the full run. Pre-condition: the next token must be a StrLit.
    /// Adjacent string literals fold into a single AST literal — the
    /// chunking present in the source is informational only, so the
    /// formatter can break long literals across source lines and re-
    /// chunk deterministically on emission with no runtime cost.
    fn read_str_literal(&mut self) -> (String, Span) {
        let tok = self.advance();
        let TokenKind::StrLit(mut value) = tok.kind else {
            unreachable!("read_str_literal called when next token is not StrLit");
        };
        let mut span = tok.span;
        while matches!(self.peek(), Some(TokenKind::StrLit(_))) {
            let next = self.advance();
            let TokenKind::StrLit(more) = next.kind else {
                unreachable!()
            };
            value.push_str(&more);
            span = Span::new(span.start, next.span.end);
        }
        (value, span)
    }

    /// Parse the value in a `source` / `build` block's directive-style
    /// field. The two blocks share one value grammar:
    ///
    /// - `()` — the unit literal (the no-value default fmt inserts for a
    ///   missing optional field);
    /// - a string literal;
    /// - a number literal (integer or float);
    /// - a boolean literal;
    /// - a bare word — a value-level identifier.
    ///
    /// Per [`grammar.md` § Package files](../../../specs/grammar.md#package-files).
    /// The caller validates which of these kinds the specific field
    /// accepts. The returned span covers the value tokens.
    fn parse_block_field_value(&mut self) -> Result<(crate::ast::BlockFieldValue, Span), Error> {
        match self.peek() {
            Some(TokenKind::StrLit(_)) => {
                let (value, span) = self.read_str_literal();
                Ok((BlockFieldValue::Str(value), span))
            }
            Some(TokenKind::LParen) => {
                let open = self.advance();
                if !matches!(self.peek(), Some(TokenKind::RParen)) {
                    return Err(self.err_here("expected `)` to close the `()` unit literal"));
                }
                let close = self.expect_kind(TokenKind::RParen, "`)`")?;
                Ok((BlockFieldValue::Unit, Span::new(open.span.start, close.end)))
            }
            Some(TokenKind::IntLit { .. } | TokenKind::FloatLit { .. }) => {
                let tok = self.advance();
                let digits = match tok.kind {
                    TokenKind::IntLit { digits } | TokenKind::FloatLit { digits } => digits,
                    _ => unreachable!("peeked IntLit/FloatLit"),
                };
                Ok((BlockFieldValue::Num(digits), tok.span))
            }
            Some(TokenKind::BoolLit(value)) => {
                let value = *value;
                let tok = self.advance();
                Ok((BlockFieldValue::Bool(value), tok.span))
            }
            Some(TokenKind::Ident(_)) => {
                let (word, span) = self.expect_ident()?;
                Self::validate_value_name(&word, span)?;
                Ok((BlockFieldValue::Word(word), span))
            }
            _ => Err(self.err_here(
                "expected a field value — the unit literal `()`, a string literal, \
                 a number, a boolean, or a bare word",
            )),
        }
    }

    fn parse_block_field_head(
        &mut self,
        mut leading_trivia: Vec<Trivia>,
        validate_key_name: bool,
    ) -> Result<ParsedBlockFieldHead, Error> {
        leading_trivia.extend(self.peek_leading_trivia());
        let (key, key_span) = self.expect_ident()?;
        if validate_key_name {
            Self::validate_value_name(&key, key_span)?;
        }
        let (value, value_span) = self.parse_block_field_value()?;
        Ok(ParsedBlockFieldHead {
            key,
            key_span,
            value,
            value_span,
            leading_trivia,
        })
    }

    fn string_block_field_value(
        &self,
        value: BlockFieldValue,
        value_span: Span,
        message: impl Into<String>,
    ) -> Result<String, Error> {
        match value {
            BlockFieldValue::Str(s) => Ok(s),
            _ => Err(self.err(value_span, message.into())),
        }
    }

    // ---- naming-convention validators -----------------------------------

    fn validate_type_name(name: &str, span: Span) -> Result<(), Error> {
        crate::naming::validate_source_name(name, crate::naming::NameRole::Type, span)
    }

    fn validate_type_path_name(name: &str, span: Span) -> Result<(), Error> {
        crate::naming::validate_reference_name(name, crate::naming::NameRole::Type, span)
    }

    /// Validate a name at a syntax boundary that admits either user namespace.
    /// The union is exact: prefix-only type classification is useful for
    /// choosing a diagnostic, but cannot turn a malformed type spelling into
    /// a value.
    fn validate_value_or_type_name(name: &str, span: Span) -> Result<(), Error> {
        if crate::naming::is_user_value_or_type_name(name) {
            return Ok(());
        }
        if crate::naming::starts_like_type_name(name) {
            Self::validate_type_name(name, span)
        } else {
            Self::validate_value_name(name, span)
        }
    }

    fn validate_path_segment_name(name: &str, span: Span) -> Result<(), Error> {
        if crate::naming::is_compiler_reserved_name(name) {
            Ok(())
        } else {
            Self::validate_value_or_type_name(name, span)
        }
    }

    /// Validate every segment of a path-shaped raw call argument against the
    /// exact spelling union admitted before its namespace/type/member role is
    /// established by resolution. This remains permissive about the role of
    /// each valid segment, preserving qualified type arguments (`m._Foo`) and
    /// value-member calls (`Formtype.member(...)`), but no declaration can make
    /// a spelling outside both user namespaces legal.
    fn validate_call_arg_path_names(&self) -> Result<(), Error> {
        let Some(Token {
            kind: TokenKind::Ident(head),
            span: head_span,
            ..
        }) = self.peek_token_at(0)
        else {
            return Ok(());
        };
        Self::validate_path_segment_name(&head, head_span)?;

        let mut offset = 0usize;
        while self
            .peek_at(offset + 1)
            .is_some_and(|kind| kind.is_sym("."))
        {
            let Some(Token {
                kind: TokenKind::Ident(name),
                span,
                ..
            }) = self.peek_token_at(offset + 2)
            else {
                break;
            };
            Self::validate_path_segment_name(&name, span)?;
            offset += 2;
        }
        Ok(())
    }

    fn validate_value_path_leaf(segments: &[PathSegment]) -> Result<(), Error> {
        let Some(leaf) = segments.last() else {
            unreachable!("a parsed value path always has at least one segment");
        };
        Self::validate_value_name(&leaf.name, leaf.span)
    }

    /// An elaborator may capture either a bare type or a value path. A dotted
    /// capture denotes a value member, while a single segment retains the
    /// type-or-value distinction that the typer resolves.
    fn validate_elaborator_capture_path(segments: &[PathSegment]) -> Result<(), Error> {
        if segments.len() == 1 {
            let segment = &segments[0];
            Self::validate_value_or_type_name(&segment.name, segment.span)
        } else {
            Self::validate_value_path_leaf(segments)
        }
    }

    fn validate_reference_value_name(name: &str, span: Span) -> Result<(), Error> {
        crate::naming::validate_reference_name(name, crate::naming::NameRole::Value, span)
    }

    fn validate_reference_value_path_leaf(segments: &[PathSegment]) -> Result<(), Error> {
        let Some(leaf) = segments.last() else {
            unreachable!("a parsed reference path always has at least one segment");
        };
        Self::validate_reference_value_name(&leaf.name, leaf.span)
    }

    fn validate_value_name(name: &str, span: Span) -> Result<(), Error> {
        crate::naming::validate_source_name(name, crate::naming::NameRole::Value, span)
    }

    /// Validate a label name: like `validate_value_name` but reject
    /// the leading underscore. A label's surface spelling is
    /// capitalized to produce the generated newtype name
    /// (`foo` → `Foo`), which is well-defined only when the first
    /// character is a letter. See `specs/language.md` § Naming
    /// conventions.
    fn validate_label_name(name: &str, span: Span) -> Result<(), Error> {
        crate::naming::validate_source_name(name, crate::naming::NameRole::Label, span)
    }

    // =====================================================================
    // Top level
    // =====================================================================

    #[cfg(any(test, feature = "cli"))]
    pub(super) fn tooling_source_root(&self) -> (Option<crate::ast::KioFileKind>, bool) {
        if let Some(TokenKind::Ident(name)) = self.peek()
            && matches!(self.peek_at(1), Some(TokenKind::Ident(_)))
            && let Some(kind) = crate::ast::KioFileKind::from_variant_name(name)
        {
            return (Some(kind), false);
        }
        let declarations = if self.peek_ident_named("import") {
            matches!(self.peek_at(1), Some(TokenKind::Ident(_)))
        } else if self.peek_ident_named("rec") && matches!(self.peek_at(1), Some(TokenKind::LParen))
        {
            matches!(self.peek_after_paren_group_at(1), Some(TokenKind::LBrace))
                || matches!(self.peek_after_paren_group_at(1), Some(TokenKind::Ident(name)) if name == "fn" || name == "pub")
        } else if (self.peek_ident_named("op") || self.peek_ident_named("varop"))
            && matches!(self.peek_at(1), Some(TokenKind::LParen))
        {
            matches!(self.peek_at(2), Some(TokenKind::SymbolRun(_)))
                && matches!(self.peek_at(3), Some(TokenKind::RParen))
                && !Self::token_is_statement_boundary(self.peek_at(4))
        } else {
            peek_item_keyword(self.cursor.remaining_current_frame(), 0).is_some()
        };
        (None, declarations)
    }

    #[cfg(any(test, feature = "cli"))]
    pub(super) fn tooling_declarations(&mut self) -> Result<(Vec<Import>, Vec<Item>), Error> {
        self.recover_import_errors = Some(Vec::new());
        let parsed = (|| {
            let imports = self.module_imports()?;
            let slices = partition_top_level_items(self.cursor.remaining_current_frame());
            match self.parse_items_with_mode(&slices, ItemParseMode::Eager) {
                Ok(items) => Ok((imports, items.into_iter().map(|item| item.item).collect())),
                Err(error) => {
                    if let Some(tooling) = &mut self.tooling {
                        tooling.facts.prefix_imports = imports;
                    }
                    Err(error)
                }
            }
        })();
        let errors = self.recover_import_errors.take().expect("recovery enabled");
        if let Some(error) = errors.into_iter().next() {
            if let Ok((imports, items)) = parsed
                && let Some(tooling) = &mut self.tooling
            {
                tooling.facts.prefix_imports = imports;
                tooling.facts.prefix_items = items;
            }
            Err(error)
        } else {
            parsed
        }
    }

    pub(super) fn module(&mut self) -> Result<Module, Error> {
        Ok(self.module_with_mode(ItemParseMode::Eager)?.module)
    }

    pub(super) fn module_lazy(&mut self) -> Result<LazyModule, Error> {
        self.module_with_mode(ItemParseMode::Lazy)
    }

    pub(super) fn module_file(&mut self) -> Result<ModuleFile, Error> {
        self.module_file_with_mode(ItemParseMode::Eager)
    }

    pub(super) fn module_file_lazy(&mut self) -> Result<ModuleFile, Error> {
        self.module_file_with_mode(ItemParseMode::Lazy)
    }

    fn module_file_with_mode(&mut self, mode: ItemParseMode) -> Result<ModuleFile, Error> {
        let lazy = self.module_with_mode(mode)?;
        let module = lazy.module.clone();
        let lazy = matches!(mode, ItemParseMode::Lazy).then_some(lazy);
        Ok(ModuleFile { module, lazy })
    }

    /// Parse a run of module-body `import` clauses, capturing leading
    /// trivia and registering each explicitly written operator grammar.
    fn module_imports(&mut self) -> Result<Vec<Import>, Error> {
        let mut imports = Vec::new();
        while self.peek_ident_named("import") {
            // Capture the trivia (comments + newlines) before this
            // `import` statement so the formatter can preserve any
            // comments above it. Doc-comments above `import` are a
            // parse error — `import` clauses are not documented.
            let leading = self.peek_leading_trivia();
            if leading
                .iter()
                .any(|t| matches!(t, Trivia::DocCommentLine { .. }))
            {
                return Err(self.err_here(
                    "doc-comment (`///`) attached to nothing — `import` clauses \
                     are not documented; move the doc-comment to the following \
                     definition",
                ));
            }
            let mut u = match self.import_stmt() {
                Ok(import) => import,
                Err(error) if self.recover_import_errors.is_some() => {
                    self.recover_import_errors
                        .as_mut()
                        .expect("recovery enabled")
                        .push(error);
                    while self.peek().is_some()
                        && !self.peek_ident_named("import")
                        && peek_item_keyword(self.cursor.remaining_current_frame(), 0).is_none()
                    {
                        if matches!(self.peek(), Some(TokenKind::Semicolon)) {
                            self.advance();
                            break;
                        }
                        self.advance();
                    }
                    continue;
                }
                Err(error) => {
                    if let Some(tooling) = &mut self.tooling
                        && tooling.cursor.is_some()
                    {
                        tooling.facts.prefix_imports = imports;
                    }
                    return Err(error);
                }
            };
            prepend_import_leading(&mut u, leading);
            self.register_import_operators(&u)?;
            imports.push(u);
        }
        Ok(imports)
    }

    fn module_with_mode(&mut self, mode: ItemParseMode) -> Result<LazyModule, Error> {
        self.cursor_choices(CursorSlot::Grammar, &["module"]);
        // Capture the trivia before the `module` keyword — this is
        // where a module-level doc-comment lives (the only place a
        // `///` can precede the `module` line).
        let mut module_leading = self.peek_leading_trivia();
        let module_doc = extract_doc_comment(&mut module_leading);

        let start = self.peek_span().start;

        if let Some(error) = self.foreign_file_header_error(false) {
            return Err(error);
        }
        let module_kw_span = self.expect_ident_named("module")?;
        let path = self.module_path_in(true)?;
        if matches!(self.peek(), Some(TokenKind::Ident(_))) {
            let mut last_name_end = None;
            for node in self.cursor.remaining_current_frame() {
                match node {
                    SkeletonNode::Leaf(Token {
                        kind: TokenKind::Ident(_),
                        span,
                        ..
                    }) => {
                        last_name_end = Some(span.end);
                    }
                    SkeletonNode::Leaf(Token {
                        kind: TokenKind::SymbolRun(run),
                        ..
                    }) if run == "/" => {}
                    SkeletonNode::Leaf(Token {
                        kind: TokenKind::Semicolon,
                        ..
                    }) => {
                        if let Some(end) = last_name_end {
                            let mut error = self.err(
                                Span::new(path.span.start, end),
                                "`module` must be followed by one module path, not adjacent names",
                            ).with_help("write `module <path>;` with `/`-separated segments relative to the package root, removing `.kio`, with no package-name prefix");
                            if path.segments[0].as_str() == "package" {
                                error = error
                                    .with_note("package declarations belong in `*.pkg.kio` files");
                            }
                            return Err(error);
                        }
                        break;
                    }
                    _ => break,
                }
            }
        }
        let semi_end = self
            .expect_kind(TokenKind::Semicolon, "`;` after the module header")?
            .end;
        if let Some(tooling) = &mut self.tooling {
            tooling.facts.module_path = Some(path.clone());
        }
        let _ = module_kw_span;

        self.declaration_keywords(
            DeclarationContext::Module,
            &ItemModifiers::impure_private(),
            true,
        );
        let imports = self.module_imports()?;
        self.declaration_keywords(
            DeclarationContext::Module,
            &ItemModifiers::impure_private(),
            true,
        );

        // Per-item parsing. Items live at the top-level skeleton
        // frame as a sequence of subtrees terminated by `;` (most
        // kinds) or by a top-level Brace group (fn — its body
        // is the last child). The cursor's
        // [`remaining_current_frame`] view exposes the still-to-
        // process slice; the partition function below splits it
        // into per-item subslices that can be parsed
        // independently.
        let item_slices = partition_top_level_items(self.cursor.remaining_current_frame());
        let parsed_items = match self.parse_items_with_mode(&item_slices, mode) {
            Ok(items) => items,
            Err(error) => {
                if let Some(tooling) = &mut self.tooling
                    && tooling.cursor.is_some()
                {
                    tooling.facts.prefix_imports = imports;
                }
                return Err(error);
            }
        };
        self.declaration_keywords(
            DeclarationContext::Module,
            &ItemModifiers::impure_private(),
            false,
        );
        let mut items = Vec::with_capacity(parsed_items.len());
        let mut deferred = Vec::new();
        let mut outer_trivia = Vec::new();
        for mut parsed in parsed_items {
            inject_meta_trivia(parsed.item.meta_mut(), std::mem::take(&mut outer_trivia));
            outer_trivia = parsed.outer_trivia;
            let item_index = items.len();
            items.push(parsed.item);
            if let Some(mut handle) = parsed.deferred {
                handle.item_index = item_index;
                deferred.push(handle);
            }
        }

        let end = items
            .last()
            .map(|i| item_span(i).end)
            .or_else(|| imports.last().map(|u| u.span.end))
            .unwrap_or(semi_end);

        // `extract_doc_comment` pulled the trailing `///` run into
        // `module_doc`; whatever remains in `module_leading` is a plain
        // `//` file-header comment above the `module` line. Attach it to
        // the module's meta so `kio fmt` preserves it (without this the
        // header is silently dropped).
        let mut meta: Meta<crate::ast::Surface> = Meta::new(Span::new(start, end));
        meta.leading_trivia = module_leading;
        // Comments after the last item, before end-of-file, have no
        // token to attach to; the lexer stashed them on the skeleton's
        // trailing run. Carry them on the module's trailing slot so a
        // trailing-after-item or end-of-file comment survives the
        // round-trip.
        meta.trailing_trivia = outer_trivia;
        meta.trailing_trivia
            .extend(std::mem::take(&mut self.file_trailing_trivia));
        let module = Module {
            path,
            imports,
            items,
            meta,
            doc: module_doc,
        };
        Ok(LazyModule { module, deferred })
    }

    /// Per-item parallel parse. Each `slice` is a contiguous run
    /// of skeleton children that forms one top-level item. The
    /// algorithm:
    ///
    /// 1. **Sequential scope pass.** Each fixed and variadic operator slice is parsed in
    ///    source order through its own sub-parser, capturing the operator
    ///    scope available before each other item.
    /// 2. **Parallel non-op pass.** Other items don't change
    ///    operator scope, so they parse in parallel via rayon
    ///    with their source-position scope snapshot. Each task gets a unique
    ///    `next_node_id` offset so
    ///    [`crate::ast::NodeId`]s stay globally distinct.
    /// 3. **Stitch.** Results merge back in source order.
    ///
    /// Every sub-parser calls [`Parser::expect_eof`] after its
    /// single `item()` so trailing garbage lumped into a slice
    /// by [`partition_top_level_items`] (e.g. a stray token
    /// after the last item) surfaces as a parse error instead of
    /// being silently dropped.
    ///
    fn parse_items_with_mode(
        &mut self,
        slices: &[ItemSlice<'a>],
        mode: ItemParseMode,
    ) -> Result<Vec<ParsedLazyItem>, Error> {
        if self
            .tooling
            .as_ref()
            .is_some_and(|tooling| tooling.collect_roles)
        {
            return self.parse_items_with_tooling(slices, mode);
        }
        let eof = self.eof;

        // (1) Sequential operator pass, in source order. Each operator is
        // parsed by its own sub-parser over its skeleton slice; the resulting
        // scope is merged back into the main parser so subsequent items
        // observe it. Operators don't issue NodeIds, so
        // no offset bookkeeping is needed here.
        let mut op_results: Vec<(usize, ParsedLazyItem)> = Vec::new();
        let mut non_ops = Vec::new();
        for (i, slice) in slices.iter().enumerate() {
            if !slice.is_op {
                non_ops.push((i, slice, self.operator_scope.clone()));
                continue;
            }
            let initial_scope = self.operator_scope.clone();
            let mut sub = Parser::new_at(slice.children, eof as usize);
            sub.operator_scope = initial_scope.clone();
            let mut parsed = sub.documented_declaration(
                |parser| parser.item_with_mode(mode),
                |parsed, doc, leading| {
                    set_item_doc(&mut parsed.item, doc, &leading);
                    inject_meta_trivia(parsed.item.meta_mut(), leading);
                },
            )?;
            retain_compact_declaration_comments(&mut parsed.item, slice.children);
            sub.expect_eof()?;
            self.operator_scope = sub.operator_scope;
            op_results.push((i, parsed));
        }

        // (2) Parallel non-op pass. Each task carries its own `Parser` over the
        // item's skeleton slice and its earlier operator-scope snapshot.
        // The `next_node_id` offsets keep NodeIds
        // disjoint across tasks.
        let parallel_results: Vec<(usize, Result<ParsedLazyItem, Error>)> =
            crate::maybe_into_par_iter!(non_ops)
                .map(|(idx, slice, scope)| {
                    let node_id_base = NODE_ID_TASK_STRIDE.saturating_mul(idx as u64 + 1);
                    let item_result = parse_item_from_children(
                        slice.children,
                        eof as usize,
                        scope,
                        node_id_base,
                        mode,
                    );
                    (idx, item_result)
                })
                .collect();

        // (3) Drain the main parser's cursor to EOF. Items were
        // parsed by sub-parsers over their own slices, so the
        // main cursor never advanced past the module header /
        // import block. The outer `parse()` calls `expect_eof`
        // after `module()`, so the main cursor must be
        // exhausted; any trailing garbage was already lumped
        // into the last slice and caught by that sub-parser's
        // `expect_eof`.
        while self.cursor.advance().is_some() {}

        // (4) Stitch op + parallel results in source order.
        let mut items: Vec<Option<ParsedLazyItem>> = (0..slices.len()).map(|_| None).collect();
        for (i, it) in op_results {
            items[i] = Some(it);
        }
        for (i, r) in parallel_results {
            items[i] = Some(r?);
        }
        Ok(items
            .into_iter()
            .map(|o| o.expect("every slot filled"))
            .collect())
    }

    fn parse_items_with_tooling(
        &mut self,
        slices: &[ItemSlice<'a>],
        mode: ItemParseMode,
    ) -> Result<Vec<ParsedLazyItem>, Error> {
        let mut items = Vec::new();
        let mut first_error = None;
        let requested_cursor = self.tooling.as_ref().and_then(|tooling| tooling.cursor);
        let mut pending: std::collections::VecDeque<_> = slices
            .iter()
            .map(|slice| ItemSlice {
                children: slice.children,
                is_op: slice.is_op,
            })
            .collect();
        let mut index = 0u64;
        while let Some(slice) = pending.pop_front() {
            let mut sub = Parser::new_at(slice.children, self.eof as usize);
            sub.operator_scope = self.operator_scope.clone();
            index += 1;
            sub.next_node_id = NODE_ID_TASK_STRIDE.saturating_mul(index);
            sub.tooling = self.tooling.take();
            sub.file_trailing_trivia = std::mem::take(&mut self.file_trailing_trivia);
            if let Some(tooling) = &mut sub.tooling
                && let Some(first) = slice.children.first()
            {
                tooling.last_end = tooling.last_end.max(skeleton_node_start(first));
                let next_start =
                    pending
                        .front()
                        .and_then(|next| next.children.first())
                        .map(|node| {
                            let token = match node {
                                SkeletonNode::Leaf(token)
                                | SkeletonNode::Group { open: token, .. } => token,
                            };
                            token
                                .leading_trivia
                                .iter()
                                .find_map(comment_span)
                                .map_or(token.span.start, |span| span.start)
                        });
                // Diagnostics keep the file EOF, while a cursor belongs only
                // to the item's source interval before the next sibling.
                tooling.cursor = requested_cursor
                    .filter(|cursor| next_start.is_none_or(|start| *cursor < start));
                tooling.cursor_limit = next_start.unwrap_or(self.eof);
            }
            let result = sub
                .documented_declaration(
                    |parser| parser.item_with_mode(mode),
                    |parsed, doc, leading| {
                        set_item_doc(&mut parsed.item, doc, &leading);
                        inject_meta_trivia(parsed.item.meta_mut(), leading);
                    },
                )
                .and_then(|mut parsed| {
                    retain_compact_declaration_comments(&mut parsed.item, slice.children);
                    sub.expect_eof()?;
                    Ok(parsed)
                });
            if result.is_err()
                && sub
                    .cursor
                    .current_group_close_span()
                    .is_some_and(|span| span.start == span.end)
                && peek_item_keyword(sub.cursor.remaining_current_frame(), 0).is_some()
            {
                // A missing closer may hide later declarations inside this
                // failed item's group. Reuse the ordinary item grammar while
                // keeping the first error and ending the failed scope here.
                let remaining = sub.cursor.remaining_current_frame();
                let boundary = skeleton_node_start(&remaining[0]);
                let item_start = skeleton_node_start(&slice.children[0]);
                if let Some(tooling) = &mut sub.tooling {
                    for region in &mut tooling.facts.regions {
                        if region.open.start >= item_start
                            && region.recovered
                            && region.interior.end > boundary
                        {
                            region.close = Span::new(boundary, boundary);
                            region.interior.end = boundary;
                        }
                    }
                    for prefix in &mut tooling.facts.scope_prefix {
                        if prefix.owner.start >= item_start && prefix.interval.end > boundary {
                            prefix.interval.end = boundary;
                        }
                    }
                }
                for recovered in partition_top_level_items(remaining).into_iter().rev() {
                    pending.push_front(recovered);
                }
            }
            self.tooling = sub.tooling;
            self.file_trailing_trivia = sub.file_trailing_trivia;
            match result {
                Ok(item) => {
                    if slice.is_op {
                        self.operator_scope = sub.operator_scope;
                    }
                    items.push(item);
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if let Some(tooling) = &mut self.tooling {
            tooling.cursor = requested_cursor;
            tooling.cursor_limit = self.eof;
        }
        while self.cursor.advance().is_some() {}
        if first_error.is_some()
            && let Some(tooling) = &mut self.tooling
        {
            tooling.facts.prefix_items = items
                .into_iter()
                .filter(|item| {
                    requested_cursor.is_none_or(|cursor| item.item.span().start <= cursor)
                })
                .map(|item| item.item)
                .collect();
            return Err(first_error.expect("checked above"));
        }
        first_error.map_or(Ok(items), Err)
    }

    fn item_with_mode(&mut self, mode: ItemParseMode) -> Result<ParsedLazyItem, Error> {
        let mut parsed = self.item_with_mode_inner(mode)?;
        if matches!(
            parsed.item,
            Item::FnDef(_)
                | Item::RecGroup(_, _)
                | Item::TypeRecGroup(_)
                | Item::Newtype(_)
                | Item::Equiv(_, _)
                | Item::Elaborator(_, _)
                | Item::Op(_, _)
                | Item::VariadicOperator(_, _)
        ) {
            let separator =
                matches!(self.peek(), Some(TokenKind::Semicolon)).then(|| self.peek_span());
            parsed.outer_trivia = self.optional_outer_semicolon();
            if let (Item::Newtype(newtype), Some(separator)) = (&mut parsed.item, separator)
                && let Some(editable) = &mut newtype.editable_span
            {
                editable.end = separator.end;
            }
            if let (Item::TypeRecGroup(group), Some(separator)) = (&mut parsed.item, separator)
                && let Some(layout) = &mut group.source_layout
            {
                layout.separator_spans.push(separator);
            }
        }
        Ok(parsed)
    }

    fn item_with_mode_inner(&mut self, mode: ItemParseMode) -> Result<ParsedLazyItem, Error> {
        let item_start = self.peek_span().start;
        let modifiers = self.item_modifiers()?;
        if modifiers.purity.is_pure() && modifiers.host && self.peek_ident_named("fn") {
            return Err(self.err_here(PURE_HOST_FN_DIAGNOSTIC));
        }
        if modifiers.purity.is_pure() && self.peek_ident_named("rec") {
            return Err(self.err_here(PURE_REC_DIAGNOSTIC));
        }
        if modifiers.purity.is_pure() && (modifiers.host || !self.peek_ident_named("fn")) {
            return Err(self.err_here("`pure` is valid only on ordinary `fn` declarations"));
        }
        if mode == ItemParseMode::Lazy && !modifiers.host && self.peek_ident_named("fn") {
            let d = self.fn_item_deferred(modifiers, item_start)?;
            Ok(ParsedLazyItem {
                outer_trivia: Vec::new(),
                item: Item::FnDef(d),
                deferred: None,
            })
        } else if mode == ItemParseMode::Lazy && self.peek_ident_named("rec") {
            Ok(ParsedLazyItem {
                outer_trivia: Vec::new(),
                item: self.rec_group_item_deferred(
                    modifiers.vis,
                    modifiers.vis_span,
                    item_start,
                )?,
                deferred: None,
            })
        } else if mode == ItemParseMode::Lazy && self.peek_ident_named("equiv") {
            if modifiers.vis.is_pub() {
                return Err(self.err_here(
                    "`equiv` declarations cannot be `pub` — they are not part of any public API",
                ));
            }
            let e = self.equiv_deferred(item_start)?;
            Ok(ParsedLazyItem {
                outer_trivia: Vec::new(),
                item: Item::Equiv(e, ()),
                deferred: None,
            })
        } else if mode == ItemParseMode::Lazy && self.peek_ident_named("elab") {
            let elaborator = self.elaborator_item_deferred(modifiers, item_start)?;
            Ok(ParsedLazyItem {
                outer_trivia: Vec::new(),
                item: Item::Elaborator(elaborator, ()),
                deferred: None,
            })
        } else {
            Ok(ParsedLazyItem {
                outer_trivia: Vec::new(),
                item: self.item_after_modifiers(modifiers)?,
                deferred: None,
            })
        }
    }

    fn documented_declaration<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, Error>,
        attach: impl FnOnce(&mut T, Option<DocComment>, Vec<Trivia>),
    ) -> Result<T, Error> {
        let mut leading = self.peek_leading_trivia();
        let doc = extract_doc_comment(&mut leading);
        let mut item = parse(self)?;
        attach(&mut item, doc, leading);
        Ok(item)
    }

    fn fn_item_deferred(&mut self, modifiers: ItemModifiers, start: u32) -> Result<FnDef, Error> {
        self.expect_ident_named("fn")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        let sig = self.signature()?;
        let (ret, ret_elided) = if self.at_sym_prefix("->") {
            self.expect_fn_arrow()?;
            (self.type_expr()?, false)
        } else {
            let unit_span = self.peek_span();
            (
                Type::Unit {
                    meta: Meta::new(Span::new(unit_span.start, unit_span.start)),
                },
                true,
            )
        };
        let body_span = self.skip_lazy_body_group(
            "`{` to open the function body, after an optional `->` return type",
        )?;
        Ok(FnDef {
            vis: modifiers.vis.clone(),
            purity: modifiers.purity,
            name,
            sig,
            ret,
            ret_elided,
            body: Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(body_span),
            },
            meta: Meta::new(Span::new(start, body_span.end)),
            doc: None,
        })
    }

    fn equiv_deferred(&mut self, start: u32) -> Result<crate::ast::Equiv, Error> {
        self.expect_ident_named("equiv")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        let sig = if self.peek_starts_signature_group() {
            self.signature()?
        } else {
            Signature::new(Vec::new())
        };
        let body_span = self.skip_lazy_body_group("`{` after `equiv` head")?;
        Ok(crate::ast::Equiv {
            name,
            name_span,
            sig,
            terms: Vec::new(),
            meta: Meta::new(Span::new(start, body_span.end)),
        })
    }

    fn elaborator_item_deferred(
        &mut self,
        modifiers: ItemModifiers,
        start: u32,
    ) -> Result<UserElaboratorDef, Error> {
        self.expect_ident_named("elab")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        self.expect_kind(TokenKind::sym(":"), "`:` after elab name")?;
        let call_ty = self.type_expr()?;
        let (captures, schedule, implementation, trailing_blocks) = self.parse_elaborator_body()?;
        let semi_end = self.syntax_end;
        Ok(UserElaboratorDef {
            vis: modifiers.vis,
            name,
            name_span,
            trailing_blocks,
            captures,
            call_ty,
            schedule,
            implementation,
            body_trivia: Vec::new(),
            meta: Meta::new(Span::new(start, semi_end)),
            doc: None,
        })
    }

    fn skip_lazy_body_group(&mut self, what: &str) -> Result<Span, Error> {
        let Some(TokenKind::LBrace) = self.peek() else {
            return Err(self.err_here(format!("expected {what}")));
        };
        let open = self.peek_span();
        self.advance();
        self.skip_open_body_group(open)
    }

    fn skip_open_body_group(&mut self, open: Span) -> Result<Span, Error> {
        let body_depth = self.cursor.depth();
        while !self.at_end() {
            let tok = self.advance();
            if matches!(tok.kind, TokenKind::RBrace) && self.cursor.depth() < body_depth {
                return Ok(Span::new(open.start, tok.span.end));
            }
        }
        Err(self.err(
            Span::new(open.start, self.eof),
            "internal parser error: lazy body group had no closing token",
        ))
    }

    // =====================================================================
    // Build block (`build { ... }` inside `<name>.pkg.kio`)
    // =====================================================================

    /// Parse a `build { ... }` block, consuming its keyword and braces.
    fn build_block(&mut self) -> Result<BuildBlock, Error> {
        let leading_trivia = self.peek_leading_trivia();
        let kw_span = self.expect_ident_named("build")?;
        self.expect_kind(TokenKind::LBrace, "`{` to open the `build` block")?;
        let mut block = self.build_block_body()?;
        let close = self.expect_kind(TokenKind::RBrace, "`}` to close the `build` block")?;
        block.span = Span::new(kw_span.start, close.end);
        block.leading_trivia = leading_trivia;
        Ok(block)
    }

    /// Collect named build entries through the closing brace or standalone
    /// EOF. An omitted cache field defaults to disabled after collection.
    pub(super) fn build_block_body(&mut self) -> Result<BuildBlock, Error> {
        let start = self.peek_span().start;
        let mut end = start;
        let mut cache = None;
        let mut docs = None;
        let mut targets = Vec::new();
        let mut target_headers = Vec::new();
        let mut pending_separator_trivia = Vec::new();
        let mut first_error = None;
        let depth = self.cursor.depth();
        let recovering = self
            .tooling
            .as_ref()
            .is_some_and(|tooling| tooling.cursor.is_some());
        loop {
            let mut field_trivia = std::mem::take(&mut pending_separator_trivia);
            field_trivia.extend(self.skip_semicolon_separators());
            let before = self.cursor.consumed();
            let parsed: Result<Option<u32>, Error> = (|| {
                let Some(field) = self.next_named_entry(
                    CursorSlot::BuildField,
                    &[
                        (
                            "cache",
                            cache.is_none(),
                            "duplicate `cache` field in `build` block",
                        ),
                        (
                            "docs",
                            docs.is_none(),
                            "duplicate `docs` block in `build` block",
                        ),
                        ("target", true, ""),
                    ],
                    "expected `cache`, `docs`, or `target` in `build` block",
                )?
                else {
                    pending_separator_trivia = field_trivia;
                    return Ok(None);
                };
                let end = match field {
                    "cache" => {
                        let parsed = self.cache_decl(field_trivia)?;
                        let (BuildBlockCache::Path { span, .. }
                        | BuildBlockCache::Disabled { span, .. }) = &parsed;
                        let end = span.end;
                        cache = Some(parsed);
                        end
                    }
                    "docs" => {
                        let parsed = self.docs_decl(field_trivia)?;
                        let end = parsed.span.end;
                        docs = Some(parsed);
                        end
                    }
                    "target" => {
                        let parsed = self.target_block(field_trivia, &mut target_headers)?;
                        let end = parsed.span.end;
                        targets.push(parsed);
                        end
                    }
                    _ => unreachable!("named-entry dispatch selected a build field"),
                };
                pending_separator_trivia =
                    self.block_separator(true, true, &["cache", "docs", "target"])?;
                Ok(Some(end))
            })();
            match parsed {
                Ok(Some(field_end)) => end = field_end,
                Ok(None) => break,
                Err(error) if recovering => {
                    first_error.get_or_insert(error);
                    while !self.at_end() {
                        if self.cursor.depth() == depth
                            && (matches!(self.peek(), Some(TokenKind::RBrace))
                                || (self.cursor.consumed() > before
                                    && ["cache", "docs", "target"]
                                        .into_iter()
                                        .any(|name| self.peek_ident_named(name))))
                        {
                            break;
                        }
                        self.advance();
                    }
                }
                Err(error) => return Err(error),
            }
            pending_separator_trivia.extend(self.skip_semicolon_separators());
        }
        if let Some(context) = self
            .tooling
            .as_mut()
            .and_then(|tooling| tooling.facts.cursor.as_mut())
            && context.slot == CursorSlot::TargetId
        {
            context.target_ids = target_headers
                .into_iter()
                .filter(|(_, span)| span.start != context.atom.replacement.start)
                .map(|(id, _)| id)
                .collect();
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        let cache = cache.unwrap_or(BuildBlockCache::Disabled {
            span: Span::new(start, start),
            leading_trivia: Vec::new(),
        });
        Ok(BuildBlock {
            leading_trivia: Vec::new(),
            trailing_trivia: {
                pending_separator_trivia.extend(self.peek_leading_trivia());
                pending_separator_trivia
            },
            cache,
            docs,
            targets,
            span: Span::new(start, end),
        })
    }

    /// Parse the optional
    /// `docs { md "…"; support "…"; md_out "…"; html "…"; };`
    /// block. `md` is mandatory inside the block; `support` is
    /// repeatable; `md_out` and `html` are optional. Per
    /// [`specs/package.md` § Build target files](../../specs/package.md#build-target-files).
    fn docs_decl(
        &mut self,
        mut leading_trivia: Vec<Trivia>,
    ) -> Result<crate::ast::BuildBlockDocs, Error> {
        leading_trivia.extend(self.peek_leading_trivia());
        let kw_span = self.expect_ident_named("docs")?;
        self.expect_kind(TokenKind::LBrace, "`{` to open the `docs` block")?;
        let mut md: Option<String> = None;
        let mut md_leading_trivia: Vec<Trivia> = Vec::new();
        let mut support: Vec<String> = Vec::new();
        let mut support_leading_trivia: Vec<Vec<Trivia>> = Vec::new();
        let mut md_out: Option<String> = None;
        let mut md_out_leading_trivia: Vec<Trivia> = Vec::new();
        let mut html: Option<String> = None;
        let mut html_leading_trivia: Vec<Trivia> = Vec::new();
        let mut pending_separator_trivia: Vec<Trivia> = Vec::new();
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let mut key_trivia = std::mem::take(&mut pending_separator_trivia);
            key_trivia.extend(self.skip_semicolon_separators());
            self.docs_field_keywords(md.is_some(), md_out.is_some(), html.is_some());
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                pending_separator_trivia = key_trivia;
                break;
            }
            let ParsedBlockFieldHead {
                key,
                key_span,
                value,
                value_span,
                leading_trivia,
                ..
            } = self.parse_block_field_head(key_trivia, false)?;
            let value = self.string_block_field_value(
                value,
                value_span,
                "`docs` block keys require a string-literal path",
            )?;
            pending_separator_trivia =
                self.block_separator(true, false, &["md", "support", "md_out", "html"])?;
            match key.as_str() {
                "md" => {
                    if md.is_some() {
                        return Err(self.err(key_span, "duplicate `md` key in `docs` block"));
                    }
                    md = Some(value);
                    md_leading_trivia = leading_trivia;
                }
                "support" => {
                    support.push(value);
                    support_leading_trivia.push(leading_trivia);
                }
                "md_out" => {
                    if md_out.is_some() {
                        return Err(self.err(key_span, "duplicate `md_out` key in `docs` block"));
                    }
                    md_out = Some(value);
                    md_out_leading_trivia = leading_trivia;
                }
                "html" => {
                    if html.is_some() {
                        return Err(self.err(key_span, "duplicate `html` key in `docs` block"));
                    }
                    html = Some(value);
                    html_leading_trivia = leading_trivia;
                }
                other => {
                    return Err(self.err(
                        key_span,
                        format!(
                            "unknown key `{other}` in `docs` block — expected `md`, \
                             `support`, `md_out`, or `html`"
                        ),
                    ));
                }
            }
        }
        self.docs_field_keywords(md.is_some(), md_out.is_some(), html.is_some());
        pending_separator_trivia.extend(self.peek_leading_trivia());
        let close = self.expect_kind(TokenKind::RBrace, "`}`")?;
        let Some(md) = md else {
            return Err(self.err(
                Span::new(kw_span.start, close.end),
                "the `docs` block requires an `md` key naming the markdown source tree",
            ));
        };
        Ok(crate::ast::BuildBlockDocs {
            trailing_trivia: pending_separator_trivia,
            leading_trivia,
            md,
            md_leading_trivia,
            support,
            support_leading_trivia,
            md_out,
            md_out_leading_trivia,
            html,
            html_leading_trivia,
            span: Span::new(kw_span.start, close.end),
        })
    }

    fn docs_field_keywords(&mut self, md: bool, md_out: bool, html: bool) {
        self.field_keywords(&[
            ("md", !md),
            ("support", true),
            ("md_out", !md_out),
            ("html", !html),
        ]);
    }

    /// Parse a cache path or explicit disabled-cache unit value.
    fn cache_decl(&mut self, mut leading_trivia: Vec<Trivia>) -> Result<BuildBlockCache, Error> {
        leading_trivia.extend(self.peek_leading_trivia());
        let kw_span = self.expect_ident_named("cache")?;
        let (value, value_span) = self.parse_block_field_value()?;
        let mut cache = match value {
            BlockFieldValue::Str(path) => BuildBlockCache::Path {
                path,
                span: Span::new(kw_span.start, value_span.end),
                leading_trivia,
            },
            BlockFieldValue::Unit => BuildBlockCache::Disabled {
                span: Span::new(kw_span.start, value_span.end),
                leading_trivia,
            },
            _ => {
                return Err(self.err(
                    value_span,
                    "`cache` requires a string-literal path or the unit literal `()`",
                ));
            }
        };
        let end = self.syntax_end;
        let (BuildBlockCache::Path { span, .. } | BuildBlockCache::Disabled { span, .. }) =
            &mut cache;
        span.end = end;
        Ok(cache)
    }

    fn target_block(
        &mut self,
        mut leading_trivia: Vec<Trivia>,
        headers: &mut Vec<(String, Span)>,
    ) -> Result<TargetBlock, Error> {
        leading_trivia.extend(self.peek_leading_trivia());
        let start = self.peek_span().start;
        let _kw_span = self.expect_ident_named("target")?;
        // Target id: a bare (kebab-case) identifier naming the backend,
        // e.g. `rust`, `js`, `kio-prime`. The old quoted form
        // (`target "rust"`) is rejected with a migration message.
        if matches!(self.peek(), Some(TokenKind::StrLit(_))) {
            return Err(self.err_here(
                "target id must be a bare identifier — write `target rust { … }`, \
                 not `target \"rust\" { … }` (see specs/package.md § Build target files)",
            ));
        }
        let (id, id_span) = self.target_id()?;
        if self
            .tooling
            .as_ref()
            .is_some_and(|tooling| tooling.cursor.is_some())
        {
            headers.push((id.clone(), id_span));
        }
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut entries = Vec::new();
        let mut pending_separator_trivia: Vec<Trivia> = Vec::new();
        self.target_field_cursor(&id, &entries);
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let mut entry_trivia = std::mem::take(&mut pending_separator_trivia);
            entry_trivia.extend(self.skip_semicolon_separators());
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                pending_separator_trivia = entry_trivia;
                break;
            }
            entries.push(self.target_entry(entry_trivia)?);
            pending_separator_trivia = self.block_separator(true, false, &[])?;
            self.target_field_cursor(&id, &entries);
        }
        pending_separator_trivia.extend(self.peek_leading_trivia());
        let end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        Ok(TargetBlock {
            trailing_trivia: pending_separator_trivia,
            id,
            entries,
            span: Span::new(start, end),
            leading_trivia,
        })
    }

    /// Parse a target id — a bare kebab-case identifier (`IDENT ('-'
    /// IDENT)*`) naming a backend. The hyphen-folding lets backend
    /// ids like `kio-prime` spell as a single bare id; the lexer
    /// splits `kio-prime` into `kio`, `-`, `prime`, and this peeler
    /// reassembles the run as long as each `-` and the following
    /// `IDENT` are flush (no intervening whitespace), so `kio - prime`
    /// is not a single id.
    fn target_id(&mut self) -> Result<(String, Span), Error> {
        if self
            .tooling
            .as_ref()
            .is_some_and(|tooling| tooling.cursor.is_some())
            && matches!(self.peek(), Some(TokenKind::Ident(_) | TokenKind::Slot1))
        {
            let mut atom = self.peek_span();
            let mut offset = 1;
            while let Some(dash) = self.peek_token_at(offset) {
                if !dash.kind.is_sym("-") || dash.span.start != atom.end {
                    break;
                }
                atom.end = dash.span.end;
                let Some(name) = self.peek_token_at(offset + 1) else {
                    break;
                };
                if !matches!(name.kind, TokenKind::Ident(_)) || name.span.start != atom.end {
                    break;
                }
                atom.end = name.span.end;
                offset += 2;
            }
            self.cursor_owned_atom(CursorSlot::TargetId, atom);
        } else {
            self.cursor_choices(CursorSlot::TargetId, &[]);
        }
        let (mut id, first_span) = self.expect_ident()?;
        let mut end = first_span.end;
        // Fold `-IDENT` continuations while flush against the run.
        while self.at_sym("-")
            && self.peek_span().start == end
            && matches!(self.peek_at(1), Some(TokenKind::Ident(_)))
        {
            let dash = self.advance(); // the `-`
            if self.peek_span().start != dash.span.end {
                // A space between `-` and the next ident; not a
                // continuation. (The `-` is already consumed; this is
                // a malformed id, so report it.)
                return Err(self.err_here(
                    "target id must be a single bare identifier — no spaces around `-`",
                ));
            }
            let (next, next_span) = self.expect_ident()?;
            id.push('-');
            id.push_str(&next);
            end = next_span.end;
        }
        Ok((id, Span::new(first_span.start, end)))
    }

    fn target_field_cursor(&mut self, id: &str, entries: &[TargetEntry]) {
        self.cursor_choices(CursorSlot::TargetField, &[]);
        if let Some(context) = self
            .tooling
            .as_mut()
            .and_then(|tooling| tooling.facts.cursor.as_mut())
            && context.slot == CursorSlot::TargetField
            && context.target.is_none()
        {
            context.target = Some(TargetCursor {
                id: id.to_owned(),
                fields: entries.iter().map(|entry| entry.key.clone()).collect(),
            });
        }
    }

    fn target_entry(&mut self, leading_trivia: Vec<Trivia>) -> Result<TargetEntry, Error> {
        // `key "value";`
        let field = self.parse_block_field_head(leading_trivia, true)?;
        let value = self.string_block_field_value(
            field.value,
            field.value_span,
            "expected a string-literal value (the build block is not typed)",
        )?;
        let semi_end = self.syntax_end;
        Ok(TargetEntry {
            key: field.key,
            value,
            span: Span::new(field.key_span.start, semi_end),
            leading_trivia: field.leading_trivia,
        })
    }

    // =====================================================================
    // Package files (`<name>.pkg.kio`)
    // =====================================================================

    pub(super) fn package_file(&mut self, stem: Option<&str>) -> Result<PackageFile, Error> {
        self.cursor_choices(CursorSlot::Grammar, &["package"]);
        let start = self.peek_span().start;
        let header_leading = self.peek_leading_trivia();
        if !self.peek_ident_named("package") {
            return Err(self.err_here(
                "a `.pkg.kio` file must begin with a `package <name>;` directive whose `<name>` is the filename stem",
            ));
        }
        self.expect_ident_named("package")?;
        let (name, name_span) = self.expect_binder()?;
        crate::naming::validate_source_name(&name, crate::naming::NameRole::Package, name_span)?;
        if let Some(expected) = stem
            && expected != name
        {
            return Err(self.err(
                name_span,
                format!("package-file name `{name}` does not match filename stem `{expected}`"),
            ));
        }
        let header_end = self
            .expect_kind(TokenKind::Semicolon, "`;` after the package header")?
            .end;

        let mut build = None;
        let mut bridge = None;
        let mut last_end = header_end;
        let mut pending = Vec::new();
        loop {
            let expected = if self.peek_ident_named("env") || self.peek_ident_named("export") {
                "`env` / `export` blocks were replaced by `host` declarations in modules and a single package `bridge { … }` glob list"
            } else {
                "a package file contains a `package <name>;` header, an optional `build` block, and an optional `bridge` block"
            };
            let Some(field) = self.next_named_entry(
                CursorSlot::Grammar,
                &[
                    (
                        "build",
                        build.is_none(),
                        "a package file may declare only one `build` block",
                    ),
                    (
                        "bridge",
                        bridge.is_none(),
                        "a package file may declare only one `bridge` block",
                    ),
                ],
                expected,
            )?
            else {
                if !self.at_end() {
                    return Err(self.err_here(expected));
                }
                break;
            };
            match field {
                "build" => {
                    let mut block = self.build_block()?;
                    pending.append(&mut block.leading_trivia);
                    block.leading_trivia = std::mem::take(&mut pending);
                    last_end = block.span.end;
                    build = Some(block);
                }
                "bridge" => {
                    let mut block = self.bridge_block()?;
                    pending.append(&mut block.leading_trivia);
                    block.leading_trivia = std::mem::take(&mut pending);
                    last_end = block.span.end;
                    bridge = Some(block);
                }
                _ => unreachable!("named-entry dispatch selected a package section"),
            }
            pending = self.optional_outer_semicolon();
        }
        let mut meta: Meta<crate::ast::Surface> = Meta::new(Span::new(start, last_end));
        meta.leading_trivia = header_leading;
        meta.trailing_trivia = pending;
        meta.trailing_trivia
            .extend(self.file_trailing_trivia.clone());
        Ok(PackageFile {
            name,
            build,
            bridge,
            meta,
        })
    }

    // =====================================================================
    // Signature file (`<name>.sig.kio`)
    // =====================================================================

    /// Parse a `<name>.sig.kio` package-signature changelog:
    ///
    ///   `signature <pkg> v(<N>);` header, then a body of oldest-first
    ///   per-version blocks `v(<M>) { breaking { … } nonbreaking { … } }`.
    ///
    /// `signature` is a file-shape keyword, sibling to `package` /
    /// `module`. `stem` is the filename stem (`<pkg>` of
    /// `<pkg>.sig.kio`); when present, the header `<pkg>` must match it,
    /// the same coherence check `package <name>;` runs.
    pub(super) fn signature_file(&mut self, stem: Option<&str>) -> Result<SignatureFile, Error> {
        self.cursor_choices(CursorSlot::Grammar, &["signature"]);
        let start = self.peek_span().start;
        let header_leading = self.peek_leading_trivia();
        if !self.peek_ident_named("signature") {
            return Err(self.err_here(
                "a `.sig.kio` file must begin with a `signature <pkg> v(<N>);` header whose `<pkg>` is the filename stem",
            ));
        }
        self.expect_ident_named("signature")?;
        let (pkg, pkg_span) = self.expect_binder()?;
        crate::naming::validate_source_name(&pkg, crate::naming::NameRole::Package, pkg_span)?;
        if let Some(expected) = stem
            && expected != pkg
        {
            return Err(self.err(
                pkg_span,
                format!("signature-file name `{pkg}` does not match filename stem `{expected}`"),
            ));
        }
        self.cursor_choices(CursorSlot::Grammar, &["v"]);
        if !self.peek_ident_named("v") {
            return Err(self.err_here(
                "expected the contract generation `v(<N>)` in the `signature <pkg> v(<N>);` header",
            ));
        }
        let (version, header_version_span) = self.sig_version_paren()?;
        let header_end = self
            .expect_kind(TokenKind::Semicolon, "`;` after the signature header")?
            .end;

        let mut versions = Vec::new();
        let mut last_end = header_end;
        let mut pending = Vec::new();
        self.cursor_choices(CursorSlot::Grammar, &["v"]);
        while self.peek_ident_named("v") {
            let mut block = self.sig_version_block()?;
            pending.append(&mut block.leading_trivia);
            block.leading_trivia = std::mem::take(&mut pending);
            pending = self.optional_outer_semicolon();
            last_end = block.span.end;
            versions.push(block);
            self.cursor_choices(CursorSlot::Grammar, &["v"]);
        }

        if !self.at_end() {
            return Err(
                self.err_here("expected a `v(<N>) { … }` version block or end of signature file")
            );
        }

        // Changelog coherence (an input-error gate, not a panic): the
        // header generation must be `>= 1`; every block version must be
        // unique and `<= header`. Ordinary history is contiguous from
        // `v(1)`. A history carrying the self-validating compact shape
        // may instead begin at a later, correctly partitioned add-only
        // boundary, after which its surviving suffix is contiguous. A
        // nonempty history ends at the header generation, or one before
        // it when the open draft block is absent. A duplicate or
        // incoherent block would make replay and the draft recompute
        // disagree about the same file.
        if version < 1 {
            return Err(self.err(
                header_version_span,
                "signature header generation must be `v(N)` with `N >= 1`".to_owned(),
            ));
        }
        let mut seen: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for block in &versions {
            if !seen.insert(block.version) {
                return Err(self.err(
                    block.span,
                    format!(
                        "duplicate `v({})` version block; each contract generation appears at most once",
                        block.version
                    ),
                ));
            }
            if block.version < 1 || block.version > version {
                return Err(self.err(
                    block.span,
                    format!(
                        "version block `v({})` is out of range for header generation `v({version})`; blocks must be `v(1)`..=`v({version})`",
                        block.version
                    ),
                ));
            }
        }
        // Contiguity: present block versions ordinarily run `1..=k`.
        // `kio sig compact` deliberately removes the earlier prefix and
        // emits one synthesized boundary at its last generation, so a
        // first block `v(start)` with `start > 1` is admitted only when
        // it has that narrow generated shape. Blocks after the boundary
        // still have no holes.
        if let (Some(&first), Some(&max)) = (seen.iter().next(), seen.iter().next_back()) {
            let later_compact_boundary = first > 1
                && versions
                    .iter()
                    .find(|block| block.version == first)
                    .is_some_and(Self::is_sig_compact_boundary);
            let start = if later_compact_boundary { first } else { 1 };
            for n in start..=max {
                if !seen.contains(&n) {
                    return Err(self.err(
                        header_version_span,
                        format!(
                            "signature changelog is non-contiguous: `v({n})` is missing but a later `v({max})` block is present; version blocks must be contiguous, and a later first block is admitted only as a correctly partitioned add-only compact boundary"
                        ),
                    ));
                }
            }
            let prior_generation = version - 1;
            if max != version && max != prior_generation {
                return Err(self.err(
                    header_version_span,
                    format!(
                        "signature changelog omits trailing generations: its latest block is `v({max})` under header `v({version})`; nonempty history must end at `v({version})` or at `v({prior_generation})` when the open draft block is absent"
                    ),
                ));
            }
        }

        let mut meta: Meta<crate::ast::Surface> = Meta::new(Span::new(start, last_end));
        meta.leading_trivia = header_leading;
        meta.trailing_trivia = pending;
        meta.trailing_trivia
            .extend(self.file_trailing_trivia.clone());
        Ok(SignatureFile {
            pkg,
            version,
            versions,
            meta,
        })
    }

    /// Whether a later-starting first version block has exactly the
    /// representation `kio sig compact` synthesizes. There is no general
    /// permission to omit old generations: only an add-only snapshot
    /// whose host requirements are under `breaking` and whose exports are
    /// under `nonbreaking` can stand in for the removed prefix. An
    /// optional version message is inert replay metadata, so it does not
    /// weaken this self-validating boundary shape.
    fn is_sig_compact_boundary(block: &SigVersion) -> bool {
        use std::collections::{BTreeMap, BTreeSet};

        // Exact operation references name declarations in the leading
        // `with` block. Flatten that context while retaining recursive-group
        // membership, so the boundary remains self-validating without
        // treating the group container as a contract item.
        // Values are `(environment side, contract-visible, recursive)`.
        let mut context = BTreeMap::<(String, String), (bool, bool, bool)>::new();
        let mut groups = Vec::<Vec<(String, String)>>::new();
        for section in &block.with {
            let module = section
                .path
                .segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            for item in &section.items {
                let declarations: Vec<(String, bool, bool, bool)> = match item {
                    SigItem::HostType(item) => vec![(item.name.clone(), true, true, false)],
                    SigItem::HostFn(item) => vec![(item.name.clone(), true, true, false)],
                    SigItem::TypeAlias(item) => {
                        vec![(item.name.clone(), false, true, false)]
                    }
                    SigItem::Newtype(item) => {
                        // As in every signature module section, the outer
                        // newtype declaration is an implied contract export;
                        // member visibility still controls its public shape.
                        vec![(item.name.clone(), false, true, item.rec_span.is_some())]
                    }
                    SigItem::ExportFn(item) => {
                        vec![(item.function.name.clone(), false, true, false)]
                    }
                    SigItem::TypeRecGroup(group) => {
                        let mut members = Vec::new();
                        let mut declarations = Vec::new();
                        for member in &group.members {
                            let (name, visible) = match member {
                                TypeRecMember::TypeAlias(alias) => (alias.name.clone(), true),
                                TypeRecMember::Newtype(newtype) => (newtype.name.clone(), true),
                                TypeRecMember::Labels(_, _) => return false,
                            };
                            members.push((module.clone(), name.clone()));
                            declarations.push((name, false, visible, true));
                        }
                        groups.push(members);
                        declarations
                    }
                };
                for (name, env_side, visible, recursive) in declarations {
                    if context
                        .insert((module.clone(), name), (env_side, visible, recursive))
                        .is_some()
                    {
                        return false;
                    }
                }
            }
        }

        let mut names = std::collections::BTreeSet::new();
        let mut referenced = BTreeSet::new();
        let valid_set = |set: &SigChangeSet,
                         env_side: bool,
                         names: &mut BTreeSet<(String, String)>,
                         referenced: &mut BTreeSet<(String, String)>| {
            if (set.add.is_empty() && set.add_refs.is_empty())
                || !set.modify.is_empty()
                || !set.modify_refs.is_empty()
                || !set.remove.is_empty()
                || !set.remove_refs.is_empty()
            {
                return false;
            }
            for section in &set.add {
                if section.items.is_empty() {
                    return false;
                }
                let module = section
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.name.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                for item in &section.items {
                    let (name, item_is_env) = match item {
                        SigItem::HostType(item) => (item.name.as_str(), true),
                        SigItem::HostFn(item) => (item.name.as_str(), true),
                        SigItem::TypeAlias(item) => (item.name.as_str(), false),
                        SigItem::Newtype(item) => (item.name.as_str(), false),
                        SigItem::ExportFn(item) => (item.function.name.as_str(), false),
                        SigItem::TypeRecGroup(_) => return false,
                    };
                    if item_is_env != env_side || !names.insert((module.clone(), name.to_owned())) {
                        return false;
                    }
                }
            }
            for reference in &set.add_refs {
                let module = reference
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.name.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                let key = (module, reference.name.clone());
                let Some((item_is_env, visible, recursive)) = context.get(&key).copied() else {
                    return false;
                };
                if !visible
                    || !recursive
                    || item_is_env != env_side
                    || !names.insert(key.clone())
                    || !referenced.insert(key)
                {
                    return false;
                }
            }
            true
        };

        let partitions_valid = block
            .breaking
            .as_ref()
            .is_none_or(|set| valid_set(set, true, &mut names, &mut referenced))
            && block
                .nonbreaking
                .as_ref()
                .is_none_or(|set| valid_set(set, false, &mut names, &mut referenced));
        if !partitions_valid {
            return false;
        }

        // A compact boundary starts without earlier state. Consequently every
        // public context declaration must be introduced by exactly one add
        // reference, while private peers may exist solely to close a recursive
        // group. No unrelated recursive group is admitted.
        if !context.is_empty() && referenced.is_empty() {
            return false;
        }
        context
            .iter()
            .all(|(name, (_, visible, _))| !visible || referenced.contains(name))
            && groups
                .iter()
                .all(|members| members.iter().any(|member| referenced.contains(member)))
    }

    // =====================================================================
    // Dependency file (`<local>.dep.kio`)
    // =====================================================================

    /// Parse a `<local>.dep.kio` dependency declaration:
    ///
    ///   `dependency <local>;` header, then exactly one
    ///   `source { path "<rel>/<name>.pkg.kio"; }` block.
    ///
    /// `dependency` is a file-shape keyword, sibling to `package` /
    /// `module` / `signature`. `stem` is the filename stem (`<local>` of
    /// `<local>.dep.kio`); when present, the header `<local>` must match
    /// it, the same coherence check `package <name>;` runs.
    pub(super) fn dependency_file(&mut self, stem: Option<&str>) -> Result<DependencyFile, Error> {
        self.cursor_choices(CursorSlot::Grammar, &["dependency"]);
        let start = self.peek_span().start;
        let header_leading = self.peek_leading_trivia();
        if !self.peek_ident_named("dependency") {
            return Err(self.err_here(
                "a `.dep.kio` file must begin with a `dependency <local>;` header whose `<local>` is the filename stem",
            ));
        }
        self.expect_ident_named("dependency")?;
        let (name, name_span) = self.expect_binder()?;
        crate::naming::validate_source_name(&name, crate::naming::NameRole::Dependency, name_span)?;
        if let Some(expected) = stem
            && expected != name
        {
            return Err(self.err(
                name_span,
                format!("dependency-file name `{name}` does not match filename stem `{expected}`"),
            ));
        }
        let header_end = self
            .expect_kind(TokenKind::Semicolon, "`;` after the dependency header")?
            .end;
        let mut source = None;
        let mut rehost = Vec::new();
        let mut retype = Vec::new();
        let mut last_end = header_end;
        let mut pending = Vec::new();
        while let Some(field) = self.next_named_entry(
            CursorSlot::Grammar,
            &[
                ("source", source.is_none(), "a dependency file may declare only one `source` block"),
                ("rehost", true, ""),
                ("retype", true, ""),
            ],
            "expected a `source` block, a `rehost` or `retype` statement, or end of dependency file",
        )? {
            match field {
                "source" => {
                    let mut block = self.source_block()?;
                    pending.append(&mut block.leading_trivia);
                    block.leading_trivia = std::mem::take(&mut pending);
                    pending = self.optional_outer_semicolon();
                    last_end = block.span.end;
                    source = Some(block);
                }
                "rehost" => {
                    let mut decl = self.rehost_statement()?;
                    pending.append(&mut decl.leading_trivia);
                    decl.leading_trivia = std::mem::take(&mut pending);
                    last_end = decl.span.end;
                    rehost.push(decl);
                }
                "retype" => {
                    let mut decl = self.retype_statement()?;
                    pending.append(&mut decl.leading_trivia);
                    decl.leading_trivia = std::mem::take(&mut pending);
                    last_end = decl.span.end;
                    retype.push(decl);
                }
                _ => unreachable!("named-entry dispatch selected a dependency entry"),
            }
        }
        if !self.at_end() {
            return Err(self.err_here("expected end of dependency file"));
        }
        let Some(source) = source else {
            return Err(self.err_here(
                "a `.dep.kio` file must declare a `source { path \"<rel>/<name>.pkg.kio\"; }` block after the header",
            ));
        };
        let mut meta: Meta<crate::ast::Surface> = Meta::new(Span::new(start, last_end));
        meta.leading_trivia = header_leading;
        meta.trailing_trivia = pending;
        meta.trailing_trivia
            .extend(self.file_trailing_trivia.clone());
        Ok(DependencyFile {
            name,
            source,
            rehost,
            retype,
            meta,
        })
    }

    /// Parse one `rehost <dep>/<mod> to <local>/<mod>;` statement: the
    /// `from` path (the dependency module, prefixed with the dependency's
    /// local name) and the `to` path (the consumer's providing module).
    fn rehost_statement(&mut self) -> Result<RehostDecl, Error> {
        let leading = self.peek_leading_trivia();
        let start = self.peek_span().start;
        self.expect_ident_named("rehost")?;
        let from = self.module_path()?;
        self.cursor_choices(CursorSlot::Grammar, &["to"]);
        if !self.peek_ident_named("to") {
            return Err(self.err_here(
                "a `rehost` statement needs `to <local>/<mod>` naming the local module that provides the host items",
            ));
        }
        self.expect_ident_named("to")?;
        let to = self.module_path()?;
        let semi_span = self.peek_span();
        self.expect_kind(TokenKind::Semicolon, "`;` after the rehost directive")?;
        Ok(RehostDecl {
            from,
            to,
            span: Span::new(start, semi_span.end),
            leading_trivia: leading,
        })
    }

    /// Parse one `retype <dep>/<mod>[.T] to <local>/<mod>[.T];` statement:
    /// the `from` path (the dependency module, prefixed with the
    /// dependency's local name) and the `to` path (the consumer's module
    /// holding the same-named counterparts). An optional trailing `.T`
    /// names a single newtype (the per-type form); when present on one
    /// side it must be present on the other with the **same** name
    /// (same-last-segment-name). Omitting it remaps every newtype under
    /// `from`.
    fn retype_statement(&mut self) -> Result<RetypeDecl, Error> {
        let leading = self.peek_leading_trivia();
        let start = self.peek_span().start;
        self.expect_ident_named("retype")?;
        let from = self.module_path()?;
        let from_type = self.optional_retype_type()?;
        self.cursor_choices(CursorSlot::Grammar, &["to"]);
        if !self.peek_ident_named("to") {
            return Err(self.err_here(
                "a `retype` statement needs `to <local>/<mod>` naming the local module that holds the same-named newtypes",
            ));
        }
        self.expect_ident_named("to")?;
        let to = self.module_path()?;
        let to_type = self.optional_retype_type()?;
        let semi_span = self.peek_span();
        self.expect_kind(TokenKind::Semicolon, "`;` after the retype directive")?;
        let type_name = match (from_type, to_type) {
            (None, None) => None,
            (Some((name, _)), Some((to_name, to_span))) => {
                if name != to_name {
                    return Err(self.err(
                        to_span,
                        format!(
                            "a per-type `retype` names the same newtype on both sides: \
                             `{name}` here is `{to_name}`. The remap keeps the type's name; \
                             write `.{name}` on both sides"
                        ),
                    ));
                }
                Some(name)
            }
            (Some((_, span)), None) | (None, Some((_, span))) => {
                return Err(self.err(
                    span,
                    "a per-type `retype` writes the newtype on *both* sides (`<from>.T to <to>.T`); \
                     omit it on both sides to remap every newtype under the module"
                        .to_string(),
                ));
            }
        };
        Ok(RetypeDecl {
            from,
            to,
            type_name,
            span: Span::new(start, semi_span.end),
            leading_trivia: leading,
        })
    }

    /// Parse an optional `.IDENT` trailing a `retype` path: the per-type
    /// newtype selector. Returns the name and the `IDENT`'s span when a
    /// `.` follows the module path, `None` otherwise.
    fn optional_retype_type(&mut self) -> Result<Option<(String, Span)>, Error> {
        if !self.at_sym(".") {
            return Ok(None);
        }
        self.advance();
        let (name, name_span) = self.expect_ident()?;
        Self::validate_type_name(&name, name_span)?;
        Ok(Some((name, name_span)))
    }

    // =====================================================================
    // Dependency lock file (`<local>.lock.kio`)
    // =====================================================================

    /// Parse a `<local>.lock.kio` lock file:
    ///
    ///   `lock <local>;` header, then exactly one
    ///   `resolved { git "<url>"; ref "<rev>"; commit "<sha>"; sig "<d>"; }`
    ///   block.
    ///
    /// `lock` is a file-shape keyword, sibling to `dependency`. `stem` is
    /// the filename stem (`<local>` of `<local>.lock.kio`); when present,
    /// the header `<local>` must match it, the same coherence check
    /// `dependency <local>;` runs. The `sig` key records the dependency's
    /// contract-surface digest at the pinned commit (see [`LockFile`]).
    pub(super) fn lock_file(&mut self, stem: Option<&str>) -> Result<LockFile, Error> {
        self.cursor_choices(CursorSlot::Grammar, &["lock"]);
        let start = self.peek_span().start;
        let header_leading = self.peek_leading_trivia();
        if !self.peek_ident_named("lock") {
            return Err(self.err_here(
                "a `.lock.kio` file must begin with a `lock <local>;` header whose `<local>` is the filename stem",
            ));
        }
        self.expect_ident_named("lock")?;
        let (name, name_span) = self.expect_binder()?;
        crate::naming::validate_source_name(&name, crate::naming::NameRole::Dependency, name_span)?;
        if let Some(expected) = stem
            && expected != name
        {
            return Err(self.err(
                name_span,
                format!("lock-file name `{name}` does not match filename stem `{expected}`"),
            ));
        }
        self.expect_kind(TokenKind::Semicolon, "`;` after the lock header")?;

        self.cursor_choices(CursorSlot::Grammar, &["resolved"]);
        if !self.peek_ident_named("resolved") {
            return Err(self.err_here(
                "a `.lock.kio` file must declare a `resolved { git \"<url>\"; ref \"<rev>\"; commit \"<sha>\"; sig \"<digest>\"; }` block after the header",
            ));
        }
        let resolved_span_start = self.peek_span().start;
        let resolved_leading_trivia = self.peek_leading_trivia();
        self.expect_ident_named("resolved")?;
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut url: Option<(String, Span)> = None;
        let mut git_ref: Option<(String, Span)> = None;
        let mut manifest_path: Option<(String, Span)> = None;
        let mut commit: Option<(String, Span)> = None;
        let mut sig: Option<(String, Span)> = None;
        let mut field_leading_trivia = std::collections::BTreeMap::new();
        let mut pending = Vec::new();
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            pending.extend(self.skip_semicolon_separators());
            self.resolved_field_keywords(
                url.is_some(),
                git_ref.is_some(),
                manifest_path.is_some(),
                commit.is_some(),
                sig.is_some(),
            );
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                break;
            }
            let ParsedBlockFieldHead {
                key,
                key_span,
                value,
                value_span,
                leading_trivia,
                ..
            } = self.parse_block_field_head(std::mem::take(&mut pending), true)?;
            field_leading_trivia.insert(key.clone(), leading_trivia);
            pending =
                self.block_separator(true, false, &["git", "ref", "path", "commit", "sig"])?;
            let slot = match key.as_str() {
                "git" => &mut url,
                "ref" => &mut git_ref,
                "path" => &mut manifest_path,
                "commit" => &mut commit,
                "sig" => &mut sig,
                other => {
                    return Err(self.err(
                        key_span,
                        format!(
                            "unknown key `{other}` in `resolved` block; expected `git`, `ref`, `path`, `commit`, or `sig`"
                        ),
                    ));
                }
            };
            if slot.is_some() {
                return Err(self.err(key_span, format!("duplicate `{key}` in `resolved` block")));
            }
            let v = self.string_block_field_value(
                value,
                value_span,
                format!("`resolved` block key `{key}` requires a string-literal value"),
            )?;
            *slot = Some((v, value_span));
        }
        self.resolved_field_keywords(
            url.is_some(),
            git_ref.is_some(),
            manifest_path.is_some(),
            commit.is_some(),
            sig.is_some(),
        );
        pending.extend(self.peek_leading_trivia());
        let close_end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        let resolved_span = Span::new(resolved_span_start, close_end);
        let outer_trivia = self.optional_outer_semicolon();

        let (url, _) = url
            .ok_or_else(|| self.err(resolved_span, "`resolved` block must declare a `git` URL"))?;
        let (git_ref, _) = git_ref
            .ok_or_else(|| self.err(resolved_span, "`resolved` block must declare a `ref`"))?;
        let (commit, _) = commit.ok_or_else(|| {
            self.err(
                resolved_span,
                "`resolved` block must declare a `commit` SHA",
            )
        })?;
        let (sig, _) = sig.ok_or_else(|| {
            self.err(
                resolved_span,
                "`resolved` block must declare a `sig` contract digest",
            )
        })?;
        if let Some((path, span)) = &manifest_path {
            self.validate_git_manifest_path(path, *span)?;
        }

        if !self.at_end() {
            return Err(self.err_here("expected end of lock file after the `resolved` block"));
        }

        let mut meta: Meta<crate::ast::Surface> = Meta::new(Span::new(start, close_end));
        meta.leading_trivia = header_leading;
        meta.trailing_trivia = outer_trivia;
        meta.trailing_trivia
            .extend(self.file_trailing_trivia.clone());
        Ok(LockFile {
            field_leading_trivia,
            resolved_leading_trivia,
            trailing_trivia: pending,
            name,
            url,
            git_ref,
            manifest_path: manifest_path.map(|(path, _)| path),
            commit,
            sig,
            meta,
        })
    }

    fn resolved_field_keywords(
        &mut self,
        git: bool,
        git_ref: bool,
        path: bool,
        commit: bool,
        sig: bool,
    ) {
        self.field_keywords(&[
            ("git", !git),
            ("ref", !git_ref),
            ("path", !path),
            ("commit", !commit),
            ("sig", !sig),
        ]);
    }

    /// Parse a `source { … }` block. Two admitted forms:
    ///
    /// - a local `path "<rel>/<name>.pkg.kio";` naming the dependency's
    ///   package file (relative, `/`-separated — an absolute path or a
    ///   `\` separator is a parse error); or
    /// - a remote `git "<url>"; ref "<rev|tag|branch>";` pair, with an
    ///   optional `path` naming a manifest relative to its checkout.
    ///
    /// Fields are unordered and unique. `git` and `ref` require each other;
    /// a standalone `path` keeps its consumer-relative meaning.
    fn source_block(&mut self) -> Result<SourceBlock, Error> {
        let leading_trivia = self.peek_leading_trivia();
        let kw_span = self.expect_ident_named("source")?;
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut path: Option<(String, Span, Vec<Trivia>)> = None;
        let mut git: Option<(String, Span, Vec<Trivia>)> = None;
        let mut git_ref: Option<(String, Span, Vec<Trivia>)> = None;
        let mut pending_separator_trivia: Vec<Trivia> = Vec::new();
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let mut key_trivia = std::mem::take(&mut pending_separator_trivia);
            key_trivia.extend(self.skip_semicolon_separators());
            self.source_field_keywords(path.is_some(), git.is_some(), git_ref.is_some());
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                pending_separator_trivia = key_trivia;
                break;
            }
            let ParsedBlockFieldHead {
                key,
                key_span,
                value,
                value_span,
                leading_trivia,
                ..
            } = self.parse_block_field_head(key_trivia, true)?;
            pending_separator_trivia =
                self.block_separator(true, false, &["path", "git", "ref"])?;
            match key.as_str() {
                "path" => {
                    if path.is_some() {
                        return Err(self.err(key_span, "duplicate `path` in `source` block"));
                    }
                    let path_val = self.string_block_field_value(
                        value,
                        value_span,
                        "`source` block key `path` requires a string-literal value",
                    )?;
                    path = Some((path_val, value_span, leading_trivia));
                }
                "git" => {
                    if git.is_some() {
                        return Err(self.err(key_span, "duplicate `git` in `source` block"));
                    }
                    let url = self.string_block_field_value(
                        value,
                        value_span,
                        "`source` block key `git` requires a string-literal value (the clone URL)",
                    )?;
                    git = Some((url, value_span, leading_trivia));
                }
                "ref" => {
                    if git_ref.is_some() {
                        return Err(self.err(key_span, "duplicate `ref` in `source` block"));
                    }
                    let r = self.string_block_field_value(
                        value,
                        value_span,
                        "`source` block key `ref` requires a string-literal value (a branch, tag, or commit SHA)",
                    )?;
                    git_ref = Some((r, value_span, leading_trivia));
                }
                other => {
                    return Err(self.err(
                        key_span,
                        format!(
                            "unknown key `{other}` in `source` block; expected `path`, or `git` and `ref`"
                        ),
                    ));
                }
            }
        }
        self.source_field_keywords(path.is_some(), git.is_some(), git_ref.is_some());
        pending_separator_trivia.extend(self.peek_leading_trivia());
        let close_end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        let block_span = Span::new(kw_span.start, close_end);

        if let Some((url, url_span, url_leading_trivia)) = git {
            let (git_ref, ref_span, ref_leading_trivia) = git_ref.ok_or_else(|| {
                self.err(
                    block_span,
                    "a `git` source requires a `ref \"<rev|tag|branch>\";` line pinning the revision",
                )
            })?;
            if url.trim().is_empty() {
                return Err(self.err(url_span, "`source` `git` URL must not be empty"));
            }
            if git_ref.trim().is_empty() {
                return Err(self.err(ref_span, "`source` `ref` must not be empty"));
            }
            let manifest_path = path.map(|(path, span, leading_trivia)| GitManifestPath {
                path,
                span,
                leading_trivia,
            });
            if let Some(selector) = &manifest_path {
                self.validate_git_manifest_path(&selector.path, selector.span)?;
            }
            return Ok(SourceBlock {
                trailing_trivia: pending_separator_trivia,
                origin: SourceOrigin::Git(GitSource {
                    url,
                    url_span,
                    url_leading_trivia,
                    git_ref,
                    ref_span,
                    ref_leading_trivia,
                    manifest_path,
                }),
                span: block_span,
                leading_trivia,
            });
        }

        if git_ref.is_some() {
            return Err(self.err(
                block_span,
                "`source` block declares a `ref` without a `git`; `ref` pins a remote `git` dependency's revision",
            ));
        }
        if let Some((path_val, path_span, path_leading_trivia)) = path {
            if path_val.starts_with('/') {
                return Err(self.err(path_span, "`source` `path` must be relative"));
            }
            if path_val.contains('\\') {
                return Err(self.err(path_span, "`source` `path` must use `/` as the separator"));
            }
            return Ok(SourceBlock {
                trailing_trivia: pending_separator_trivia,
                origin: SourceOrigin::Path {
                    path: path_val,
                    path_span,
                    path_leading_trivia,
                },
                span: block_span,
                leading_trivia,
            });
        }

        Err(self.err(
            block_span,
            "`source` block must declare a source — a local `path \"<rel>/<name>.pkg.kio\";` or a remote `git \"<url>\"; ref \"<rev>\";`",
        ))
    }

    fn source_field_keywords(&mut self, path: bool, git: bool, git_ref: bool) {
        self.field_keywords(&[("path", !path), ("git", !git), ("ref", !git_ref)]);
    }

    fn validate_git_manifest_path(&self, path: &str, span: Span) -> Result<(), Error> {
        let bytes = path.as_bytes();
        let drive_qualified =
            bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        if path.starts_with('/') || drive_qualified {
            return Err(self.err(span, "git manifest `path` must be relative to the checkout"));
        }
        if path.contains('\\') {
            return Err(self.err(span, "git manifest `path` must use `/` as the separator"));
        }
        Ok(())
    }

    /// `v ( <N> )` — the version-number form used in the header and at
    /// the head of each version block. Returns the parsed number and the
    /// span of the whole `v(<N>)` run. The `v` keyword is consumed here.
    fn sig_version_paren(&mut self) -> Result<(u32, Span), Error> {
        let start = self.expect_ident_named("v")?.start;
        self.expect_kind(TokenKind::LParen, "`(` after `v`")?;
        let (digits, digits_span) = match self.peek() {
            Some(TokenKind::IntLit { digits }) => {
                let digits = digits.clone();
                let span = self.peek_span();
                self.advance();
                (digits, span)
            }
            _ => return Err(self.err_here("expected a version number in `v(<N>)`")),
        };
        let version: u32 = digits.parse().map_err(|_| {
            self.err(
                digits_span,
                format!("version number `{digits}` in `v(<N>)` is out of range"),
            )
        })?;
        let end = self
            .expect_kind(TokenKind::RParen, "`)` closing `v(<N>)`")?
            .end;
        Ok((version, Span::new(start, end)))
    }

    /// One `v(<N>) { breaking { … } nonbreaking { … } }` block, with an
    /// optional leading `///` doc-comment (the version's changelog
    /// message). The `breaking` section comes first and each section is
    /// omitted when empty; the presence of `breaking { … }` is the
    /// version's breaking-ness.
    fn sig_version_block(&mut self) -> Result<SigVersion, Error> {
        let mut leading = self.peek_leading_trivia();
        let doc = extract_doc_comment(&mut leading);
        let (version, head_span) = self.sig_version_paren()?;
        self.expect_kind(TokenKind::LBrace, "`{` after `v(<N>)`")?;
        let mut with = None;
        let mut breaking = None;
        let mut nonbreaking = None;
        let mut with_leading_trivia = Vec::new();
        let mut with_trailing_trivia = Vec::new();
        let mut pending = self.leading_block_separator(false)?;
        while let Some(field) = self.next_named_entry(
            CursorSlot::Grammar,
            &[
                (
                    "with",
                    with.is_none(),
                    "duplicate `with` block in a version block",
                ),
                (
                    "breaking",
                    breaking.is_none(),
                    "duplicate `breaking` section in a version block",
                ),
                (
                    "nonbreaking",
                    nonbreaking.is_none(),
                    "duplicate `nonbreaking` section in a version block",
                ),
            ],
            "expected `with`, `breaking`, or `nonbreaking` in a version block",
        )? {
            match field {
                "with" => {
                    with_leading_trivia = std::mem::take(&mut pending);
                    with_leading_trivia.extend(self.peek_leading_trivia());
                    let (sections, trailing) = self.sig_with_block()?;
                    with = Some(sections);
                    with_trailing_trivia = trailing;
                }
                "breaking" | "nonbreaking" => {
                    let mut section = self.sig_change_set(field)?;
                    pending.append(&mut section.leading_trivia);
                    section.leading_trivia = std::mem::take(&mut pending);
                    if field == "breaking" {
                        breaking = Some(section);
                    } else {
                        nonbreaking = Some(section);
                    }
                }
                _ => unreachable!("named-entry dispatch selected a version section"),
            }
            pending = self.block_separator(false, false, &["with", "breaking", "nonbreaking"])?;
        }
        if breaking.is_none() && nonbreaking.is_none() {
            return Err(self.err_here(
                "a `v(<N>) { … }` block needs at least one of `breaking { … }` / `nonbreaking { … }` (empty sections are omitted, not empty blocks)",
            ));
        }
        pending.extend(self.peek_leading_trivia());
        let end = self
            .expect_kind(TokenKind::RBrace, "`}` closing the `v(<N>)` block")?
            .end;
        Ok(SigVersion {
            leading_trivia: leading,
            trailing_trivia: pending,
            with_leading_trivia,
            with_trailing_trivia,
            doc,
            version,
            with: with.unwrap_or_default(),
            breaking,
            nonbreaking,
            span: Span::new(head_span.start, end),
        })
    }

    fn sig_change_set(&mut self, kw: &str) -> Result<SigChangeSet, Error> {
        let leading_trivia = self.peek_leading_trivia();
        let start = self.expect_ident_named(kw)?.start;
        self.expect_kind(TokenKind::LBrace, "`{` after the section keyword")?;
        let mut add = None;
        let mut modify = None;
        let mut remove = None;
        let mut add_leading_trivia = Vec::new();
        let mut modify_leading_trivia = Vec::new();
        let mut remove_leading_trivia = Vec::new();
        let mut pending = self.leading_block_separator(false)?;
        while let Some(field) = self.next_named_entry(
            CursorSlot::Grammar,
            &[
                (
                    "add",
                    add.is_none(),
                    "duplicate `add` block in a change section",
                ),
                (
                    "modify",
                    modify.is_none(),
                    "duplicate `modify` block in a change section",
                ),
                (
                    "remove",
                    remove.is_none(),
                    "duplicate `remove` block in a change section",
                ),
            ],
            "expected `add`, `modify`, or `remove` in a change section",
        )? {
            match field {
                "add" => {
                    add_leading_trivia = std::mem::take(&mut pending);
                    add_leading_trivia.extend(self.peek_leading_trivia());
                    add = Some(self.sig_operation_block("add")?);
                }
                "modify" => {
                    modify_leading_trivia = std::mem::take(&mut pending);
                    modify_leading_trivia.extend(self.peek_leading_trivia());
                    modify = Some(self.sig_operation_block("modify")?);
                }
                "remove" => {
                    remove_leading_trivia = std::mem::take(&mut pending);
                    remove_leading_trivia.extend(self.peek_leading_trivia());
                    remove = Some(self.sig_remove_block()?);
                }
                _ => unreachable!("named-entry dispatch selected a change bucket"),
            }
            pending = self.block_separator(false, false, &["add", "modify", "remove"])?;
        }
        pending.extend(self.peek_leading_trivia());
        let end = self
            .expect_kind(TokenKind::RBrace, "`}` closing the change section")?
            .end;
        let (add, add_refs, add_trailing_trivia) = add.unwrap_or_default();
        let (modify, modify_refs, modify_trailing_trivia) = modify.unwrap_or_default();
        let (remove, remove_refs, remove_trailing_trivia) = remove.unwrap_or_default();
        if add.is_empty()
            && add_refs.is_empty()
            && modify.is_empty()
            && modify_refs.is_empty()
            && remove.is_empty()
            && remove_refs.is_empty()
        {
            return Err(self.err(
                Span::new(start, end),
                format!("`{kw} {{ … }}` needs at least one `add` / `modify` / `remove` block"),
            ));
        }
        Ok(SigChangeSet {
            leading_trivia,
            trailing_trivia: pending,
            add_leading_trivia,
            add_trailing_trivia,
            modify_leading_trivia,
            modify_trailing_trivia,
            remove_leading_trivia,
            remove_trailing_trivia,
            add,
            add_refs,
            modify,
            modify_refs,
            remove,
            remove_refs,
            span: Span::new(start, end),
        })
    }

    /// A `with` block carries complete Kio' declarations, including any
    /// recursive type group referenced by the version's operations.
    fn sig_with_block(&mut self) -> Result<(Vec<SigModuleSection>, Vec<Trivia>), Error> {
        self.expect_ident_named("with")?;
        self.expect_kind(TokenKind::LBrace, "`{` after `with`")?;
        self.cursor_choices(CursorSlot::Grammar, &["module"]);
        let mut sections = Vec::new();
        let mut pending = self.leading_block_separator(false)?;
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let mut section = self.sig_module_section(true)?;
            pending.append(&mut section.leading_trivia);
            section.leading_trivia = std::mem::take(&mut pending);
            sections.push(section);
            pending = self.block_separator(false, false, &["module"])?;
            self.cursor_choices(CursorSlot::Grammar, &["module"]);
        }
        pending.extend(self.peek_leading_trivia());
        self.expect_kind(TokenKind::RBrace, "`}` closing `with`")?;
        if sections.is_empty() {
            return Err(self.err_here("`with { … }` needs at least one `module` section"));
        }
        Ok((sections, pending))
    }

    /// `add` / `modify` accepts the established inline module sections and
    /// exact FQN references to declarations supplied by the version's
    /// `with` block.
    fn sig_operation_block(
        &mut self,
        kw: &str,
    ) -> Result<SigBlockEntries<SigModuleSection>, Error> {
        self.expect_ident_named(kw)?;
        self.expect_kind(TokenKind::LBrace, "`{` after the block keyword")?;
        let mut sections = Vec::new();
        let mut refs = Vec::new();
        let mut pending = self.leading_block_separator(false)?;
        self.cursor_choices(CursorSlot::ModulePath, &["module"]);
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            if self.peek_ident_named("module")
                && matches!(self.peek_at(1), Some(TokenKind::Ident(_)))
            {
                let mut section = self.sig_module_section(false)?;
                pending.append(&mut section.leading_trivia);
                section.leading_trivia = std::mem::take(&mut pending);
                sections.push(section);
            } else {
                let mut reference = self.sig_item_ref()?;
                pending.append(&mut reference.leading_trivia);
                reference.leading_trivia = std::mem::take(&mut pending);
                refs.push(reference);
            }
            pending = self.block_separator(false, false, &[])?;
            self.cursor_choices(CursorSlot::ModulePath, &["module"]);
        }
        pending.extend(self.peek_leading_trivia());
        self.expect_kind(TokenKind::RBrace, "`}` closing the block")?;
        if sections.is_empty() && refs.is_empty() {
            return Err(self.err_here(format!(
                "`{kw} {{ … }}` needs at least one declaration or exact declaration reference"
            )));
        }
        Ok((sections, refs, pending))
    }

    /// One `module <path> { <import clauses> <signature-only items> }`
    /// section inside an `add` / `modify` block.
    fn sig_module_section(
        &mut self,
        allow_type_rec_groups: bool,
    ) -> Result<SigModuleSection, Error> {
        let leading_trivia = self.peek_leading_trivia();
        let start = self.expect_ident_named("module")?.start;
        let path = self.module_path()?;
        self.expect_kind(TokenKind::LBrace, "`{` after the module path")?;
        let context = DeclarationContext::Signature {
            recursive: allow_type_rec_groups,
        };
        self.declaration_keywords(context, &ItemModifiers::impure_private(), true);
        let mut imports = Vec::new();
        let mut items = Vec::new();
        let mut pending = self.leading_block_separator(false)?;
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            if items.is_empty() && self.peek_ident_named("import") {
                if self
                    .peek_leading_trivia()
                    .iter()
                    .chain(pending.iter())
                    .any(|trivia| matches!(trivia, Trivia::DocCommentLine { .. }))
                {
                    return Err(self.err_here("doc-comment (`///`) attached to nothing — `import` clauses are not documented"));
                }
                let mut import = self.import_stmt_in(DeclarationEnd::BlockEntry)?;
                prepend_import_leading(&mut import, std::mem::take(&mut pending));
                self.register_import_operators(&import)?;
                imports.push(import);
            } else {
                let mut item = self.documented_declaration(
                    |parser| parser.sig_item(allow_type_rec_groups),
                    |item, doc, leading| {
                        set_sig_item_doc(item, doc);
                        inject_meta_trivia(sig_item_meta_mut(item), leading);
                    },
                )?;
                inject_meta_trivia(sig_item_meta_mut(&mut item), std::mem::take(&mut pending));
                items.push(item);
            }
            let heads: &[&str] = if items.is_empty() {
                &["import", "pub", "pure", "host", "type", "newtype", "rec"]
            } else {
                &["pub", "pure", "host", "type", "newtype", "rec"]
            };
            pending = self.block_separator(false, false, heads)?;
            self.declaration_keywords(context, &ItemModifiers::impure_private(), items.is_empty());
        }
        pending.extend(self.peek_leading_trivia());
        let end = self
            .expect_kind(TokenKind::RBrace, "`}` closing the module section")?
            .end;
        Ok(SigModuleSection {
            leading_trivia,
            trailing_trivia: pending,
            path,
            imports,
            items,
            span: Span::new(start, end),
        })
    }

    /// One signature-only declaration: `host type` / `host fn`, `type`,
    /// `newtype`, or the body-less `pub fn f(p0: T) -> R;` export
    /// production. A version-leading `with` section additionally admits
    /// the shared-Kio′ `rec newtype` and complete `rec { ... }` forms;
    /// operation sections name those declarations by exact references.
    /// Surface-only forms (`elab` / `op` / `labels` / `equiv` / `varop` /
    /// `literal`) are rejected with a sig-context diagnostic.
    fn sig_item(&mut self, allow_type_rec_groups: bool) -> Result<SigItem, Error> {
        let modifiers = self.item_modifiers_in(DeclarationContext::Signature {
            recursive: allow_type_rec_groups,
        })?;
        if self.peek_ident_named("rec") {
            if !allow_type_rec_groups {
                return Err(self.err_here(
                    "recursive declarations live in the version-leading `with` block and are named by exact references in operation blocks",
                ));
            }
            if modifiers.purity.is_pure() {
                return Err(self.err_here("`pure` cannot modify a recursive type declaration"));
            }
            let rec_span = self.expect_ident_named("rec")?;
            if self.peek_ident_named("newtype") {
                let mut newtype =
                    self.with_recursive_type_scope(rec_span, |parser| parser.newtype(modifiers))?;
                newtype.rec_span = Some(rec_span);
                return Ok(SigItem::Newtype(newtype));
            }
            if !matches!(self.peek(), Some(TokenKind::LBrace)) {
                return Err(self.err_here(
                    "a signature-file recursive declaration must be `rec newtype` or a complete `rec { ... }` group",
                ));
            }
            let item = self.type_rec_group_after_rec(
                rec_span.start,
                modifiers.vis.clone(),
                modifiers.vis_span,
                false,
            )?;
            let Item::TypeRecGroup(group) = item else {
                unreachable!("type_rec_group_after_rec returns a type group")
            };
            if group.members.iter().any(|member| match member {
                TypeRecMember::Labels(_, _) => true,
                TypeRecMember::Newtype(newtype) => newtype.rec_span.is_some(),
                TypeRecMember::TypeAlias(_) => false,
            }) {
                return Err(self.err(
                    group.meta.span,
                    "a signature-file `rec { ... }` group contains only ordinary Kio' `type` and `newtype` members",
                ));
            }
            return Ok(SigItem::TypeRecGroup(group));
        }
        if modifiers.host {
            if modifiers.purity.is_pure() {
                let message = if self.peek_ident_named("fn") {
                    PURE_HOST_FN_DIAGNOSTIC
                } else {
                    "`pure` is valid only on ordinary `fn` declarations"
                };
                return Err(self.err_here(message));
            }
            let start = self.peek_span().start;
            return if self.peek_ident_named("type") {
                self.expect_ident_named("type")?;
                let host_type = self.host_type_rest_in(start, DeclarationEnd::BlockEntry)?;
                if host_type.owned {
                    return Err(self.err(
                        host_type.meta.span,
                        "`{ owned }` is a source-only compatibility annotation — a `.sig.kio` records backend-independent signatures, not source-only annotations",
                    ));
                }
                Ok(SigItem::HostType(host_type))
            } else if self.peek_ident_named("fn") {
                self.expect_ident_named("fn")?;
                self.host_fn_rest_in(start, DeclarationEnd::BlockEntry)
                    .map(SigItem::HostFn)
            } else {
                Err(self.err_here("expected `type` or `fn` after `host`"))
            };
        }
        if self.peek_ident_named("fn") {
            self.sig_export_fn(modifiers.vis.is_pub(), modifiers.purity)
                .map(SigItem::ExportFn)
        } else if self.peek_ident_named("type") {
            if modifiers.purity.is_pure() {
                return Err(self.err_here("`pure` is valid only on ordinary `fn` declarations"));
            }
            self.type_alias_in(modifiers, DeclarationEnd::BlockEntry)
                .map(SigItem::TypeAlias)
        } else if self.peek_ident_named("newtype") {
            if modifiers.purity.is_pure() {
                return Err(self.err_here("`pure` is valid only on ordinary `fn` declarations"));
            }
            self.newtype(modifiers).map(SigItem::Newtype)
        } else {
            if modifiers.purity.is_pure() {
                return Err(self.err_here("`pure` is valid only on ordinary `fn` declarations"));
            }
            Err(self.err_here(
                "a signature-file declaration must be `host type` / `host fn`, `type`, `newtype`, or a body-less `pub fn` export signature",
            ))
        }
    }

    /// `pub fn f(p0: T) -> R;` — a body-less export-fn signature. Reuses
    /// the host-fn signature shape (`host_fn_rest` minus the `host`
    /// keyword); a stray `{ … }` body where the `;` is expected gets a
    /// sig-context diagnostic.
    fn sig_export_fn(&mut self, vis_pub: bool, purity: Purity) -> Result<SigExportFn, Error> {
        if !vis_pub {
            return Err(self.err_here(
                "a signature-file export `fn` must be `pub` — a `.sig.kio` records only the public interface",
            ));
        }
        let owner = self.peek_span();
        let start = owner.start;
        self.expect_ident_named("fn")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        self.record_source_name(name_span, SourceNameRole::Function);
        let groups = self.host_fn_param_groups()?;
        self.expect_fn_arrow()?;
        let ret = self.with_host_type_parameters(owner, &groups, Self::type_expr)?;
        // A `{` here is a function body — illegal in a signature file,
        // which records signatures only. Give the sig-context
        // diagnostic rather than the bare "expected `;`".
        if matches!(self.peek(), Some(TokenKind::LBrace)) {
            return Err(self.err_here(
                "a signature-file `fn` records a body-less signature — write `pub fn f(p) -> R;`, not a `{ … }` body",
            ));
        }
        let semi_end = self.syntax_end;
        Ok(SigExportFn {
            purity,
            function: crate::ast::host_fn_from_groups(
                name,
                groups,
                ret,
                Meta::new(Span::new(start, semi_end)),
                None,
            ),
        })
    }

    /// `remove { module <path> { <name>; … } … }` — a braced run of
    /// name-only module sections.
    fn sig_remove_block(&mut self) -> Result<SigBlockEntries<SigRemoveModule>, Error> {
        self.expect_ident_named("remove")?;
        self.expect_kind(TokenKind::LBrace, "`{` after `remove`")?;
        let mut modules = Vec::new();
        let mut refs = Vec::new();
        let mut pending = self.leading_block_separator(false)?;
        self.cursor_choices(CursorSlot::ModulePath, &["module"]);
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            if self.peek_ident_named("module")
                && matches!(self.peek_at(1), Some(TokenKind::Ident(_)))
            {
                let mut section = self.sig_remove_module()?;
                pending.append(&mut section.leading_trivia);
                section.leading_trivia = std::mem::take(&mut pending);
                modules.push(section);
            } else {
                let mut reference = self.sig_item_ref()?;
                pending.append(&mut reference.leading_trivia);
                reference.leading_trivia = std::mem::take(&mut pending);
                refs.push(reference);
            }
            pending = self.block_separator(false, false, &[])?;
            self.cursor_choices(CursorSlot::ModulePath, &["module"]);
        }
        pending.extend(self.peek_leading_trivia());
        self.expect_kind(TokenKind::RBrace, "`}` closing `remove`")?;
        if modules.is_empty() && refs.is_empty() {
            return Err(
                self.err_here("`remove { … }` needs at least one exact declaration reference")
            );
        }
        Ok((modules, refs, pending))
    }

    /// An exact, non-expression declaration reference: `foo/bar.Item;`.
    /// This deliberately reuses the ordinary module-path and declared-name
    /// categories while requiring the separating dot, so a local/bare name
    /// cannot acquire context-dependent meaning inside a changelog.
    fn sig_item_ref(&mut self) -> Result<SigItemRef, Error> {
        let leading_trivia = self.peek_leading_trivia();
        let start = self.peek_span().start;
        let path = self.module_path()?;
        self.expect_sym(".", "`.` between the module path and declaration name")?;
        let (name, name_span) = self.expect_ident()?;
        Self::validate_removed_name(&name, name_span)?;
        let end = self.syntax_end;
        Ok(SigItemRef {
            leading_trivia,
            path,
            name,
            span: Span::new(start, end),
        })
    }

    /// One `module <path> { <name>; … }` entry inside a `remove` block.
    /// Each `<name>` is a bare value-name or type-name terminated by `;`;
    /// the item's side (host vs export) and signature are recovered by
    /// replay, not parsed here.
    fn sig_remove_module(&mut self) -> Result<SigRemoveModule, Error> {
        let leading_trivia = self.peek_leading_trivia();
        let start = self.expect_ident_named("module")?.start;
        let path = self.module_path()?;
        self.expect_kind(TokenKind::LBrace, "`{` after the module path")?;
        let mut names = Vec::new();
        let mut pending = self.leading_block_separator(false)?;
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            pending.extend(self.peek_leading_trivia());
            let (name, name_span) = self.expect_ident()?;
            Self::validate_removed_name(&name, name_span)?;
            names.push(SigRemoveName {
                leading_trivia: std::mem::take(&mut pending),
                name,
                span: name_span,
            });
            pending = self.block_separator(false, false, &[])?;
        }
        pending.extend(self.peek_leading_trivia());
        let end = self
            .expect_kind(TokenKind::RBrace, "`}` closing the module section")?
            .end;
        if names.is_empty() {
            return Err(self.err(
                Span::new(start, end),
                "a `remove` module section needs at least one item name",
            ));
        }
        Ok(SigRemoveModule {
            leading_trivia,
            trailing_trivia: pending,
            path,
            names,
            span: Span::new(start, end),
        })
    }

    /// A removed item name is side-ambiguous (a value name names a host
    /// fn or an export fn; a type name names a `host type` or an exported
    /// `newtype` / `type`), so admit either the value-name or the
    /// type-name shape. Reject only the reserved `__`-prefix and the bare
    /// wildcard.
    fn validate_removed_name(name: &str, span: Span) -> Result<(), Error> {
        if Self::validate_value_name(name, span).is_ok() && name != "_" {
            return Ok(());
        }
        if Self::validate_type_name(name, span).is_ok() {
            return Ok(());
        }
        Err(Error::parse(
            span,
            format!("`{name}` is not a valid removed item name (a value name or a type name)"),
        ))
    }

    /// `bridge { <glob>; … }` — a `;`-separated list of module-path
    /// globs. Each glob is a `/`-separated run of literal-IDENT / `*` /
    /// `**` segments.
    fn bridge_block(&mut self) -> Result<BridgeBlock, Error> {
        let leading_trivia = self.peek_leading_trivia();
        let start = self.expect_ident_named("bridge")?.start;
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut globs = Vec::new();
        let mut pending = self.leading_block_separator(false)?;
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let mut glob = self.bridge_glob()?;
            pending.append(&mut glob.leading_trivia);
            glob.leading_trivia = std::mem::take(&mut pending);
            globs.push(glob);
            pending = self.block_separator(false, false, &[])?;
        }
        pending.extend(self.peek_leading_trivia());
        let trailing = pending;
        let end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        let mut trailing_trivia = Vec::new();
        Self::collect_comments_into(trailing, &mut trailing_trivia);
        Ok(BridgeBlock {
            globs,
            span: Span::new(start, end),
            leading_trivia,
            trailing_trivia,
        })
    }

    /// One bridge glob; the enclosing list owns semicolon separators.
    fn bridge_glob(&mut self) -> Result<BridgeGlob, Error> {
        let leading_trivia = self.peek_leading_trivia();
        let mut segments = Vec::new();
        let start = self.peek_span().start;
        loop {
            match self.peek() {
                Some(TokenKind::Ident(name)) => {
                    let name = name.clone();
                    let span = self.peek_span();
                    Self::validate_value_name(&name, span)?;
                    self.advance();
                    segments.push(BridgeGlobSegment::Literal(name));
                }
                Some(TokenKind::SymbolRun(run)) if run == "*" => {
                    self.advance();
                    segments.push(BridgeGlobSegment::Star);
                }
                Some(TokenKind::SymbolRun(run)) if run == "**" => {
                    self.advance();
                    segments.push(BridgeGlobSegment::DoubleStar);
                }
                _ => {
                    return Err(self.err_here(
                        "expected a module-name segment, `*`, or `**` in a bridge glob",
                    ));
                }
            }
            // The path separator `/` may stand alone or have fused
            // greedily with the following `*` / `**` into one SymbolRun
            // (`app/*` lexes `app`, then `/*`). Peel a leading `/` off
            // the run and continue so the `*` / `**` parses as the next
            // segment.
            if self.at_sym("/") {
                self.advance();
                continue;
            }
            if self.at_sym_prefix("/") {
                self.split_current_sym(1);
                continue;
            }
            break;
        }
        let span_end = self.syntax_end;
        Ok(BridgeGlob {
            segments,
            span: Span::new(start, span_end),
            leading_trivia,
        })
    }

    /// `type Name TypeParamList? RoleAnnotation? OwnedBlock? ;` after the
    /// `host type` keywords. The parser preserves the grammar's components;
    /// shared semantic validation restricts host type parameters to kind `*`
    /// and rejects a declaration that combines any parameters with a role. The
    /// optional `{ owned }` block is preserved for source compatibility
    /// (see [`HostType::owned`]) but selects no alternate facade. Rust renders
    /// every `role(str)` occurrence by value, and other backends likewise give
    /// the block no runtime meaning.
    fn host_type_rest(&mut self, start: u32) -> Result<HostType, Error> {
        self.host_type_rest_in(start, DeclarationEnd::Outer)
    }

    fn host_type_rest_in(&mut self, start: u32, end: DeclarationEnd) -> Result<HostType, Error> {
        let (name, name_span) = self.expect_binder()?;
        Self::validate_type_name(&name, name_span)?;
        self.record_source_name(name_span, SourceNameRole::Type);
        let type_params = if self.peek_starts_type_param_list() {
            self.type_param_list()?
        } else {
            Vec::new()
        };
        self.cursor_choices(
            CursorSlot::Grammar,
            if type_params.is_empty() {
                &["role"]
            } else {
                &[]
            },
        );
        let role = if self.peek_ident_named("role") {
            Some(self.role_annotation()?)
        } else {
            None
        };
        let owned = if matches!(self.peek(), Some(TokenKind::LBrace)) {
            self.expect_kind(TokenKind::LBrace, "`{`")?;
            self.cursor_choices(CursorSlot::Grammar, &["owned"]);
            self.expect_ident_named("owned")?;
            self.expect_kind(TokenKind::RBrace, "`}`")?;
            true
        } else {
            false
        };
        let semi_end = self.declaration_end(end, "`;` after the host type declaration")?;
        Ok(HostType {
            name,
            type_params,
            role,
            owned,
            meta: Meta::new(Span::new(start, semi_end)),
            doc: None,
        })
    }

    fn role_annotation(&mut self) -> Result<RoleAnnotation, Error> {
        let start = self.peek_span().start;
        self.expect_ident_named("role")?;
        self.expect_kind(TokenKind::LParen, "`(`")?;
        self.cursor_choices_from(
            CursorSlot::Grammar,
            [
                Role::I8,
                Role::I16,
                Role::I32,
                Role::I64,
                Role::I128,
                Role::U8,
                Role::U16,
                Role::U32,
                Role::U64,
                Role::U128,
                Role::F32,
                Role::F64,
                Role::Bool,
                Role::Str,
            ]
            .into_iter()
            .map(Role::as_str),
        );
        let (kind_text, kind_text_span) = self.expect_ident()?;
        let kind = Role::from_str(&kind_text).ok_or_else(|| {
            self.err(
                kind_text_span,
                format!(
                    "unknown `role` kind `{kind_text}`; expected one of \
                     i8/i16/i32/i64/i128, u8/u16/u32/u64/u128, f32/f64, bool, str"
                ),
            )
        })?;
        self.record_keyword(kind_text_span, KeywordRole::Declaration);
        let end = self.expect_kind(TokenKind::RParen, "`)`")?.end;
        Ok(RoleAnnotation {
            role: kind,
            span: Span::new(start, end),
        })
    }

    /// `fn name[A](x: A) -> R;` after the `host fn` keywords. Host
    /// declarations use the same named value-parameter shape as ordinary
    /// function signatures.
    fn host_fn_rest(&mut self, start: u32) -> Result<HostFn, Error> {
        self.host_fn_rest_in(start, DeclarationEnd::Outer)
    }

    fn host_fn_rest_in(&mut self, start: u32, end: DeclarationEnd) -> Result<HostFn, Error> {
        let owner = self.peek_span();
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        self.record_source_name(name_span, SourceNameRole::Function);
        let groups = self.host_fn_param_groups()?;
        self.expect_fn_arrow()?;
        let ret = self.with_host_type_parameters(owner, &groups, Self::type_expr)?;
        if matches!(self.peek(), Some(TokenKind::LBrace)) {
            return Err(self
                .err_here("a `host fn` declares a function supplied by the host, not a Kio function body")
                .with_help("end the host function signature with `;`; use an ordinary `fn` to define a Kio body"));
        }
        let semi_end = self.declaration_end(end, "`;` after the host function declaration")?;
        Ok(crate::ast::host_fn_from_groups(
            name,
            groups,
            ret,
            Meta::new(Span::new(start, semi_end)),
            None,
        ))
    }

    fn host_fn_param_groups(&mut self) -> Result<Vec<HostFnParamGroup>, Error> {
        let owner = self.peek_span();
        let mut groups = Vec::new();
        while self.peek_starts_signature_group() {
            if self.peek_starts_type_param() {
                groups.push(HostFnParamGroup::Type(self.type_param_group()?));
            } else {
                let params =
                    self.with_host_type_parameters(owner, &groups, Self::host_fn_value_group)?;
                groups.push(HostFnParamGroup::Value(params));
            }
        }
        if groups.is_empty() {
            return Err(self.err_here("expected a host function parameter group"));
        }
        if !groups
            .iter()
            .any(|group| matches!(group, HostFnParamGroup::Value(_)))
        {
            return Err(
                self.err_here("host function signatures require an explicit value group `()`")
            );
        }
        Ok(groups)
    }

    fn with_host_type_parameters<T>(
        &mut self,
        owner: Span,
        groups: &[HostFnParamGroup],
        parse: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.with_type_parameters(
            owner,
            ScopeOwnerKind::Signature,
            groups.iter().flat_map(|group| match group {
                HostFnParamGroup::Type(params) => params.as_slice(),
                HostFnParamGroup::Value(_) => &[],
            }),
            parse,
        )
    }

    fn host_fn_value_group(&mut self) -> Result<Vec<HostFnValueParam>, Error> {
        self.expect_kind(TokenKind::LParen, "`(`")?;
        self.skip_commas();
        let mut params = Vec::new();
        while !matches!(self.peek(), Some(TokenKind::RParen)) {
            params.push(self.host_fn_value_param()?);
            let saw_comma = self.skip_commas();
            if matches!(self.peek(), Some(TokenKind::RParen)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `)`"));
            }
        }
        self.expect_kind(TokenKind::RParen, "`)`")?;
        Ok(params)
    }

    fn host_fn_value_param(&mut self) -> Result<HostFnValueParam, Error> {
        self.suppress_current_cursor(CursorSuppressionKind::Binder);
        match self.peek() {
            // `name: T` — lookahead for `:` after an ident decides this.
            Some(TokenKind::Ident(_)) if self.peek_kind_at(1, &TokenKind::sym(":")) => {
                let start = self.peek_span().start;
                let (name, name_span) = self.expect_ident()?;
                Self::validate_value_name(&name, name_span)?;
                self.record_source_name(name_span, SourceNameRole::Parameter);
                self.expect_sym(":", "`:`")?;
                let ty = self.type_expr()?;
                let ty_end = ty.span().end;
                Ok(HostFnValueParam {
                    name: Some(name),
                    ty,
                    meta: Meta::new(Span::new(start, ty_end)),
                })
            }
            _ => Err(self.err_here("expected named host function parameter `name: Type`")),
        }
    }

    // =====================================================================
    // Module paths
    // =====================================================================

    fn foreign_file_header_error(&self, in_module_body: bool) -> Option<Error> {
        let Some(TokenKind::Ident(keyword)) = self.peek() else {
            return None;
        };
        let extension = match keyword.as_str() {
            "package" => "pkg",
            "dependency" => "dep",
            "lock" => "lock",
            "signature" => "sig",
            _ => return None,
        };
        if !matches!(self.peek_at(1), Some(TokenKind::Ident(_))) {
            return None;
        }
        let complete_header = if keyword == "signature" {
            matches!(self.peek_at(2), Some(TokenKind::Ident(name)) if name == "v")
                && matches!(self.peek_at(3), Some(TokenKind::LParen))
                && matches!(self.peek_at(4), Some(TokenKind::IntLit { .. }))
                && matches!(self.peek_at(5), Some(TokenKind::RParen))
                && matches!(self.peek_at(6), Some(TokenKind::Semicolon))
        } else {
            matches!(self.peek_at(2), Some(TokenKind::Semicolon))
        };
        if !complete_header {
            return None;
        }
        let message = if in_module_body {
            format!("a {keyword} declaration is not a module-body declaration")
        } else {
            format!("a {keyword} declaration is not a module header")
        };
        Some(self.err_here(message).with_help(format!(
            "put {keyword} declarations in `*.{extension}.kio` files; a regular `.kio` file starts with `module <path>;`"
        )))
    }

    fn module_path(&mut self) -> Result<ModulePath, Error> {
        self.module_path_in(false)
    }

    fn module_path_in(&mut self, binding: bool) -> Result<ModulePath, Error> {
        self.module_path_in_slot(binding, CursorSlot::ModulePath)
    }

    fn module_path_in_slot(
        &mut self,
        binding: bool,
        slot: CursorSlot,
    ) -> Result<ModulePath, Error> {
        let (first, first_span) = if binding {
            self.expect_binder()?
        } else {
            self.cursor_choices(slot, &[]);
            self.expect_ident()?
        };
        crate::naming::validate_source_name(&first, crate::naming::NameRole::Module, first_span)?;
        self.record_source_name(first_span, SourceNameRole::Module);
        let mut segments = vec![PathSegment::new(first, first_span)];
        let mut span_end = first_span.end;
        while self.at_sym("/") {
            let separator = self.advance().span;
            let (next, next_span) = if binding {
                self.expect_binder()?
            } else {
                self.cursor_path_choices(slot, &segments, separator, PathSeparator::Slash, 0);
                self.expect_ident()?
            };
            crate::naming::validate_source_name(&next, crate::naming::NameRole::Module, next_span)?;
            self.record_source_name(next_span, SourceNameRole::Module);
            segments.push(PathSegment::new(next, next_span));
            span_end = next_span.end;
        }
        Ok(ModulePath {
            segments,
            span: Span::new(first_span.start, span_end),
        })
    }

    /// Parse a type path: a bare type name or a slash-separated module path
    /// followed by one dotted type name (`a/b/c.Type`).
    fn type_path_segments(&mut self) -> Result<Vec<PathSegment>, Error> {
        self.cursor_choices(CursorSlot::Type, &[]);
        let (first, first_span) = self.expect_ident()?;
        let mut segments = vec![PathSegment::new(first, first_span)];
        let mut saw_module_separator = false;
        while self.at_sym("/") && matches!(self.peek_at(1), Some(TokenKind::Ident(_))) {
            saw_module_separator = true;
            let separator = self.advance().span;
            self.cursor_path_choices(
                CursorSlot::ModulePath,
                &segments,
                separator,
                PathSeparator::Slash,
                0,
            );
            let (next, next_span) = self.expect_ident()?;
            segments.push(PathSegment::new(next, next_span));
        }
        if self.at_sym("/") {
            self.cursor_path_choices(
                CursorSlot::ModulePath,
                &segments,
                self.peek_span(),
                PathSeparator::Slash,
                1,
            );
        }
        if self.at_sym(".") {
            let separator = self.advance().span;
            self.cursor_path_choices(
                CursorSlot::Type,
                &segments,
                separator,
                PathSeparator::Dot,
                0,
            );
            let (item, item_span) = self.expect_ident()?;
            segments.push(PathSegment::new(item, item_span));
        } else if saw_module_separator {
            let end = segments
                .last()
                .map_or(first_span.end, |segment| segment.span.end);
            return Err(self.err(
                Span::new(first_span.start, end),
                "qualified paths require `.` before the final item or type name",
            ));
        }
        Ok(segments)
    }

    // =====================================================================
    // Imports
    // =====================================================================

    fn collect_import_comments(cursor: &mut SkeletonCursor<'_>, end: u32, into: &mut Vec<Trivia>) {
        while let Some(token) = cursor.peek_token() {
            if token.span.start >= end {
                break;
            }
            Self::collect_comments_into(token.leading_trivia.clone(), into);
            cursor.advance();
        }
    }

    fn import_stmt(&mut self) -> Result<Import, Error> {
        self.import_stmt_in(DeclarationEnd::Outer)
    }

    fn import_stmt_in(&mut self, termination: DeclarationEnd) -> Result<Import, Error> {
        let start = self.peek_span().start;
        let mut comments = self.cursor.clone();
        comments.advance();
        let mut leading_trivia = Vec::new();
        let mut trailing_trivia = Vec::new();
        self.expect_ident_named("import")?;
        self.cursor_choices(CursorSlot::ImportProvider, &[]);
        let kind = if self.peek_ident_named("__intrinsics__") {
            self.advance();
            ImportKind::Intrinsics
        } else if self.peek_ident_named("__comptime__") {
            self.advance();
            ImportKind::Comptime
        } else {
            let from = self.module_path_in_slot(false, CursorSlot::ImportProvider)?;
            self.cursor_choices(CursorSlot::Grammar, &["as"]);
            if self.peek_ident_named("as") {
                self.expect_ident_named("as")?;
                let (alias, span) = self.expect_binder()?;
                Self::validate_value_name(&alias, span)?;
                ImportKind::Qualified { path: from, alias }
            } else {
                self.expect_kind(TokenKind::LParen, "`(` after the import provider")?;
                Self::collect_import_comments(
                    &mut comments,
                    self.peek_span().start,
                    &mut leading_trivia,
                );
                let mut pending_item_trivia = Vec::new();
                self.skip_commas();
                Self::collect_import_comments(
                    &mut comments,
                    self.peek_span().start,
                    &mut pending_item_trivia,
                );
                let mut items = Vec::new();
                while !matches!(self.peek(), Some(TokenKind::RParen)) {
                    self.cursor_choices(CursorSlot::ImportSelection, &[]);
                    let selection_start = self.peek_span().start;
                    let selection_depth = self.cursor.depth();
                    let selection_kind = self.import_selection_kind();
                    let tag_end = (self.peek_ident_named("op") || self.peek_ident_named("varop"))
                        .then(|| self.peek_span().end);
                    let parsed = self.parse_import_item();
                    self.import_cursor(&from, &items, selection_kind, selection_start, tag_end);
                    let mut item = match parsed {
                        Ok(item) => Some(item),
                        Err(error) if self.recover_import_errors.is_some() => {
                            self.recover_import_errors
                                .as_mut()
                                .expect("recovery enabled")
                                .push(error);
                            while self.peek().is_some()
                                && (self.cursor.depth() > selection_depth
                                    || (!matches!(
                                        self.peek(),
                                        Some(
                                            TokenKind::Comma
                                                | TokenKind::RParen
                                                | TokenKind::Semicolon
                                        )
                                    ) && peek_item_keyword(
                                        self.cursor.remaining_current_frame(),
                                        0,
                                    )
                                    .is_none()))
                            {
                                self.advance();
                            }
                            None
                        }
                        Err(error) => return Err(error),
                    };
                    let trivia = item
                        .as_mut()
                        .map(ImportItem::leading_trivia_mut)
                        .unwrap_or(&mut trailing_trivia);
                    trivia.append(&mut pending_item_trivia);
                    Self::collect_import_comments(&mut comments, self.peek_span().start, trivia);
                    if matches!(self.peek(), Some(TokenKind::RParen)) {
                        items.extend(item);
                        break;
                    }
                    if !self.skip_commas() {
                        return Err(self.err_here("expected `,` or `)` in the import list"));
                    }
                    Self::collect_import_comments(
                        &mut comments,
                        self.peek_span().start,
                        &mut pending_item_trivia,
                    );
                    items.extend(item);
                }
                self.cursor_choices(CursorSlot::ImportSelection, &[]);
                self.import_cursor(
                    &from,
                    &items,
                    ImportSelectionKind::Any,
                    self.peek_span().start,
                    None,
                );
                if items.is_empty() && self.recover_import_errors.is_none() {
                    return Err(self.err_here("expected at least one imported item"));
                }
                trailing_trivia.append(&mut pending_item_trivia);
                self.expect_kind(TokenKind::RParen, "`)` closing the import list")?;
                Self::collect_import_comments(
                    &mut comments,
                    self.peek_span().start,
                    &mut trailing_trivia,
                );
                ImportKind::Selective { items, from }
            }
        };
        let end = self.declaration_end(termination, "`;` after the import")?;
        Self::collect_import_comments(&mut comments, end, &mut leading_trivia);
        Ok(Import {
            kind,
            span: Span::new(start, end),
            leading_trivia,
            trailing_trivia,
        })
    }

    fn register_import_operators(&mut self, u: &Import) -> Result<(), Error> {
        insert_consumer_grammars(&mut self.operator_scope, u)
    }

    /// Selection tags distinguish ordinary names, label namespaces and complete
    /// operator grammars without consulting the provider.
    fn parse_import_item(&mut self) -> Result<ImportItem, Error> {
        let selection = self.import_selection_kind();
        if selection == ImportSelectionKind::Label {
            self.expect_kind(TokenKind::LBrace, "`{`")?;
            let (name, name_span) = self.expect_ident()?;
            Self::validate_label_name(&name, name_span)?;
            self.expect_kind(TokenKind::RBrace, "`}`")?;
            return Ok(ImportItem::Label {
                name,
                span: name_span,
                leading_trivia: Vec::new(),
            });
        }
        if matches!(
            selection,
            ImportSelectionKind::FixedOperator | ImportSelectionKind::VariadicOperator
        ) {
            let start = self.peek_span().start;
            let grammar = if self.peek_ident_named("varop") {
                let (open, close) = self.parse_variadic_head()?;
                crate::ast::OperatorGrammar::Variadic { open, close }
            } else {
                self.expect_ident_named("op")?;
                crate::ast::OperatorGrammar::fixed(&self.parse_op_pattern(true)?)
            };
            let span = Span::new(start, self.peek_span().start);
            return Ok(ImportItem::OperatorPattern {
                grammar,
                span,
                leading_trivia: Vec::new(),
            });
        }
        let (name, span) = self.expect_ident()?;
        Self::validate_value_or_type_name(&name, span)?;
        Ok(ImportItem::Name {
            name,
            span,
            leading_trivia: Vec::new(),
        })
    }

    fn import_selection_kind(&self) -> ImportSelectionKind {
        if matches!(self.peek(), Some(TokenKind::LBrace)) {
            ImportSelectionKind::Label
        } else if !matches!(self.peek_at(1), Some(TokenKind::Comma | TokenKind::RParen))
            && self.peek_ident_named("op")
        {
            ImportSelectionKind::FixedOperator
        } else if !matches!(self.peek_at(1), Some(TokenKind::Comma | TokenKind::RParen))
            && self.peek_ident_named("varop")
        {
            ImportSelectionKind::VariadicOperator
        } else if matches!(self.peek(), Some(TokenKind::Ident(_))) {
            ImportSelectionKind::Name
        } else {
            ImportSelectionKind::Any
        }
    }

    fn import_cursor(
        &mut self,
        provider: &ModulePath,
        selected: &[ImportItem],
        kind: ImportSelectionKind,
        start: u32,
        tag_end: Option<u32>,
    ) {
        let Some(tooling) = &self.tooling else {
            return;
        };
        let Some(cursor) = tooling.cursor else {
            return;
        };
        if tooling
            .facts
            .cursor
            .as_ref()
            .is_some_and(|context| context.import.is_some())
        {
            return;
        }
        let failed_token_end = if matches!(
            kind,
            ImportSelectionKind::VariadicOperator | ImportSelectionKind::Label
        ) && matches!(
            self.peek(),
            Some(TokenKind::SymbolRun(_) | TokenKind::Ident(_) | TokenKind::Slot1)
        ) {
            self.peek_span().end
        } else {
            start
        };
        let end = if kind == ImportSelectionKind::Any {
            self.tooling_cursor_end(tooling.cursor_limit)
        } else {
            tooling.last_end.max(failed_token_end).max(start)
        };
        if cursor > end || tooling.facts.suppression.is_some() {
            return;
        }
        if cursor >= start {
            let replacement = if kind == ImportSelectionKind::Any {
                Span::new(cursor, cursor)
            } else {
                Span::new(start, end)
            };
            self.cursor_owned_atom(CursorSlot::ImportSelection, replacement);
        }
        if let Some(context) = self
            .tooling
            .as_mut()
            .and_then(|tooling| tooling.facts.cursor.as_mut())
            && context.slot == CursorSlot::ImportSelection
            && context.import.is_none()
        {
            context.import = Some(ImportCursor {
                provider: provider.clone(),
                kind: if cursor < start || tag_end.is_some_and(|end| cursor <= end) {
                    ImportSelectionKind::Any
                } else {
                    kind
                },
                selected: selected.to_vec(),
            });
        }
    }

    // =====================================================================
    // Top-level items
    // =====================================================================

    fn item_modifiers(&mut self) -> Result<ItemModifiers, Error> {
        self.item_modifiers_in(DeclarationContext::Module)
    }

    fn item_modifiers_in(&mut self, context: DeclarationContext) -> Result<ItemModifiers, Error> {
        let mut modifiers = ItemModifiers::impure_private();
        loop {
            self.declaration_keywords(context, &modifiers, false);
            if self.peek_ident_named("pub") {
                let span = self.peek_span();
                self.advance();
                self.record_keyword(span, KeywordRole::Declaration);
                if modifiers.vis.is_pub() {
                    return Err(self.err(span, "duplicate `pub` modifier"));
                }
                let mut end = span.end;
                modifiers.vis = if self.peek_kind_at(0, &TokenKind::LParen) {
                    self.expect_kind(TokenKind::LParen, "`(`")?;
                    let path = self.module_path()?;
                    end = self.expect_kind(TokenKind::RParen, "`)`")?.end;
                    Visibility::PublicIn(path)
                } else {
                    Visibility::Public
                };
                modifiers.vis_span = Some(Span::new(span.start, end));
                continue;
            }
            if self.peek_ident_named("pure") {
                let span = self.peek_span();
                self.advance();
                self.record_keyword(span, KeywordRole::Declaration);
                if modifiers.purity.is_pure() {
                    return Err(self.err(span, "duplicate `pure` modifier"));
                }
                modifiers.purity = Purity::Pure;
                continue;
            }
            // `host` is a contextual keyword: it is the host-declaration
            // modifier only when followed by `type` / `fn` (optionally
            // with a `pub` in between, accepting `host pub type`). In any
            // other position it stays an ordinary identifier.
            if self.peek_ident_named("host")
                && self
                    .tooling
                    .as_ref()
                    .is_some_and(|tooling| tooling.cursor.is_some())
            {
                let pub_span = self.peek_token_at(1).and_then(|token| {
                    matches!(&token.kind, TokenKind::Ident(name) if name == "pub")
                        .then_some(token.span)
                });
                let allowed = !(modifiers.host
                    || modifiers.purity.is_pure()
                    || matches!(context, DeclarationContext::TypeGroup { .. }));
                let choices = [
                    ("fn", allowed),
                    ("pub", allowed && !modifiers.vis.is_pub()),
                    ("type", allowed),
                ];
                self.cursor_choices_at(
                    CursorSlot::Grammar,
                    choices
                        .into_iter()
                        .filter_map(|(keyword, valid)| valid.then_some(keyword)),
                    1,
                    Some(self.peek_span().end),
                );
                if let Some(pub_span) = pub_span {
                    self.cursor_choices_at(
                        CursorSlot::Grammar,
                        ["fn", "type"]
                            .into_iter()
                            .filter(|_| allowed && !modifiers.vis.is_pub()),
                        2,
                        Some(pub_span.end),
                    );
                }
            }
            if self.peek_ident_named("host") && self.host_decl_follows() {
                let span = self.peek_span();
                self.advance();
                self.record_keyword(span, KeywordRole::Declaration);
                if modifiers.host {
                    return Err(self.err(span, "duplicate `host` modifier"));
                }
                modifiers.host = true;
                continue;
            }
            break;
        }
        Ok(modifiers)
    }

    fn declaration_keywords(
        &mut self,
        context: DeclarationContext,
        modifiers: &ItemModifiers,
        imports: bool,
    ) {
        if self
            .tooling
            .as_ref()
            .is_none_or(|tooling| tooling.cursor.is_none())
        {
            return;
        }
        let module = matches!(context, DeclarationContext::Module);
        let type_group = matches!(context, DeclarationContext::TypeGroup { .. });
        if (modifiers.purity.is_pure() && modifiers.host)
            || (type_group && (modifiers.purity.is_pure() || modifiers.host))
        {
            self.cursor_choices(CursorSlot::Grammar, &[]);
            return;
        }
        let ordinary = !modifiers.purity.is_pure() && !modifiers.host;
        let surface = module || matches!(context, DeclarationContext::TypeGroup { surface: true });
        let recursive =
            surface || matches!(context, DeclarationContext::Signature { recursive: true });
        let choices = [
            ("import", imports),
            ("pub", !modifiers.vis.is_pub()),
            ("pure", ordinary && !type_group),
            ("host", ordinary && !type_group),
            (
                "fn",
                !type_group && (module || modifiers.host || modifiers.vis.is_pub()),
            ),
            ("rec", ordinary && recursive && !type_group),
            ("type", !modifiers.purity.is_pure()),
            ("literal", ordinary && module),
            ("newtype", ordinary),
            ("labels", ordinary && surface),
            ("equiv", ordinary && module && !modifiers.vis.is_pub()),
            ("elab", ordinary && module),
            ("op", ordinary && module),
            ("varop", ordinary && module),
        ];
        self.cursor_choices_from(
            CursorSlot::Grammar,
            choices
                .into_iter()
                .filter_map(|(keyword, remaining)| remaining.then_some(keyword)),
        );
    }

    /// True when a leading `host` ident introduces a `host type` /
    /// `host fn` declaration — i.e. the next token after `host` is
    /// `type` / `fn`, or `pub` followed by `type` / `fn`.
    fn host_decl_follows(&self) -> bool {
        if self.peek_ident_named_at(1, "type") || self.peek_ident_named_at(1, "fn") {
            return true;
        }
        self.peek_ident_named_at(1, "pub")
            && (self.peek_ident_named_at(2, "type") || self.peek_ident_named_at(2, "fn"))
    }

    fn item_after_modifiers(&mut self, modifiers: ItemModifiers) -> Result<Item, Error> {
        if modifiers.purity.is_pure() && modifiers.host && self.peek_ident_named("fn") {
            return Err(self.err_here(PURE_HOST_FN_DIAGNOSTIC));
        }
        if modifiers.purity.is_pure() && self.peek_ident_named("rec") {
            return Err(self.err_here(PURE_REC_DIAGNOSTIC));
        }
        if modifiers.purity.is_pure() && (modifiers.host || !self.peek_ident_named("fn")) {
            return Err(self.err_here("`pure` is valid only on ordinary `fn` declarations"));
        }
        if modifiers.host {
            // Host items are always public; `pub host` / `host pub` are
            // accepted but redundant. `pure host` is rejected.
            let start = self.peek_span().start;
            return if self.peek_ident_named("type") {
                self.expect_ident_named("type")?;
                self.host_type_rest(start).map(Item::HostType)
            } else if self.peek_ident_named("fn") {
                self.expect_ident_named("fn")?;
                self.host_fn_rest(start).map(Item::HostFn)
            } else {
                Err(self.err_here("expected `type` or `fn` after `host`"))
            };
        }
        if self.peek_ident_named("fn") {
            self.fn_item(modifiers).map(Item::FnDef)
        } else if self.peek_ident_named("rec") {
            self.rec_group_item(modifiers.vis.clone(), modifiers.vis_span)
        } else if self.peek_ident_named("type") {
            if self.peek_kind_at(1, &TokenKind::LBrace) {
                self.label_forward(modifiers)
                    .map(|forward| Item::LabelForward(forward, ()))
            } else {
                self.type_alias(modifiers).map(Item::TypeAlias)
            }
        } else if self.peek_ident_named("literal") {
            self.literal_alias(modifiers.vis)
                .map(|literal| Item::LiteralAlias(literal, ()))
        } else if self.peek_ident_named("newtype") {
            self.newtype(modifiers).map(Item::Newtype)
        } else if self.peek_ident_named("labels") {
            self.labels(modifiers).map(|d| Item::Labels(d, ()))
        } else if self.peek_ident_named("tags") {
            Err(self.err_here("`tags` is no longer a keyword — use `labels`"))
        } else if self.peek_ident_named("equiv") {
            // `equiv` decls have no visibility (they're a test-runner
            // concern, not a public API surface); reject `pub equiv`.
            if modifiers.vis.is_pub() {
                return Err(self.err_here(
                    "`equiv` declarations cannot be `pub` — they are not part of any public API",
                ));
            }
            self.equiv().map(|e| Item::Equiv(e, ()))
        } else if self.peek_ident_named("elab") {
            self.elaborator_item(modifiers)
                .map(|s| Item::Elaborator(s, ()))
        } else if self.peek_ident_named("varop") {
            let fold = self.variadic_operator(modifiers)?;
            Ok(Item::VariadicOperator(Box::new(fold), ()))
        } else if self.peek_ident_named("op") {
            // Only preceding local declarations enter the expression scope.
            // Imported grammars enter through their explicit selection lists.
            let op = self.op(modifiers)?;
            // Register the binding in the operator scope so subsequent
            // expression parsing recognizes the operator. The registry
            // enforces the prefix-forbidden rule: same-token-sequence
            // collisions and strict-prefix relationships are both
            // rejected as name conflicts per spec.
            let span = op.meta.span;
            let OpBody::Normal { pattern, .. } = &op.body;
            self.operator_scope
                .insert(pattern.clone())
                .map_err(|message| self.err(span, message))?;
            Ok(Item::Op(Box::new(op), ()))
        } else if let Some(old) = self.peek_renamed_item_keyword() {
            // Retired item-leading keywords get a directed
            // suggestion rather than the generic "expected …" error.
            let suggestion = match old {
                "defn" => "fn",
                "deftype" => "newtype",
                "defop" => "op",
                _ => {
                    unreachable!("peek_renamed_item_keyword only returns renamed keywords")
                }
            };
            Err(self.err_here(format!(
                "`{old}` is no longer a keyword — use `{suggestion}`"
            )))
        } else if let Some(error) = self.foreign_file_header_error(true) {
            Err(error)
        } else {
            Err(self.err_here(
                "expected `fn`, `rec`, `type`, `literal`, `newtype`, `labels`, `equiv`, `elab`, or `op`",
            ))
        }
    }

    fn elaborator_item(&mut self, modifiers: ItemModifiers) -> Result<UserElaboratorDef, Error> {
        let start = self.peek_span().start;
        self.expect_ident_named("elab")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        self.expect_kind(TokenKind::sym(":"), "`:` after elab name")?;
        let call_ty = self.type_expr()?;
        let (captures, schedule, implementation, trailing_blocks) = self.parse_elaborator_body()?;
        let semi_end = self.syntax_end;
        Ok(UserElaboratorDef {
            vis: modifiers.vis.clone(),
            name,
            name_span,
            trailing_blocks,
            captures,
            call_ty,
            schedule,
            implementation,
            body_trivia: Vec::new(),
            meta: Meta::new(Span::new(start, semi_end)),
            doc: None,
        })
    }

    fn parse_elaborator_body(
        &mut self,
    ) -> Result<
        (
            Vec<crate::ast::UserElaboratorCapture>,
            crate::ast::ElaboratorSchedule,
            crate::ast::LexicalCallablePath,
            Vec<crate::ast::TrailingBlockDecl>,
        ),
        Error,
    > {
        self.expect_kind(TokenKind::LBrace, "`{` after elab call type")?;
        self.leading_block_separator(false)?;
        let mut captures = None;
        let mut implementation = None;
        let mut trailing_blocks = Vec::new();
        while let Some(field) = self.next_named_entry(
            CursorSlot::Grammar,
            &[
                (
                    "captures",
                    captures.is_none(),
                    "duplicate `captures` field in `elab` body",
                ),
                (
                    "impl",
                    implementation.is_none(),
                    "duplicate `impl` field in `elab` body",
                ),
                ("trailing", true, "trailing block descriptor"),
            ],
            "expected `captures`, `impl`, or `trailing` field in `elab` body",
        )? {
            match field {
                "captures" => {
                    captures = Some(self.parse_elaborator_captures()?);
                }
                "impl" => {
                    self.expect_ident_named("impl")?;
                    let (schedule, path, _) = self.parse_elaborator_implementation()?;
                    implementation = Some((schedule, path));
                }
                "trailing" => trailing_blocks.push(self.trailing_block_descriptor()?),
                _ => unreachable!("named-entry dispatch selected an elab field"),
            }
            self.block_separator(false, false, &["captures", "impl", "trailing"])?;
        }
        let Some((schedule, implementation)) = implementation else {
            return Err(self.err_here("`elab` body requires an `impl` field"));
        };
        self.expect_kind(TokenKind::RBrace, "`}` closing `elab` body")?;
        Ok((
            captures.unwrap_or_default(),
            schedule,
            implementation,
            trailing_blocks,
        ))
    }

    fn trailing_block_descriptor(&mut self) -> Result<crate::ast::TrailingBlockDecl, Error> {
        let start = self.expect_ident_named("trailing")?.start;
        self.cursor_choices(CursorSlot::Grammar, &["product", "thunk", "sequence"]);
        let (kind, span) = self.expect_ident()?;
        let exposure = match kind.as_str() {
            "product" => crate::ast::BlockExposure::Product,
            "thunk" => crate::ast::BlockExposure::Thunk,
            "sequence" => crate::ast::BlockExposure::Sequence,
            _ => {
                return Err(self.err(
                    span,
                    "expected `product`, `thunk`, or `sequence` after `trailing`",
                ));
            }
        };
        self.record_keyword(span, KeywordRole::Declaration);
        self.cursor_choices(CursorSlot::Grammar, &[]);
        let label = if matches!(self.peek(), Some(TokenKind::Ident(_))) {
            let (name, span) = self.expect_ident()?;
            Self::validate_value_name(&name, span)?;
            self.record_keyword(span, KeywordRole::Control);
            Some(PathSegment::new(name, span))
        } else {
            None
        };
        Ok(crate::ast::TrailingBlockDecl {
            exposure,
            label,
            meta: Meta::new(Span::new(start, self.syntax_end)),
        })
    }

    fn has_fills_marker_prefix(&self) -> bool {
        matches!(self.peek_at(0), Some(TokenKind::LParen))
            && self.peek_ident_named_at(1, "fills")
            && matches!(self.peek_at(2), Some(TokenKind::RParen))
    }

    fn parse_elaborator_implementation(
        &mut self,
    ) -> Result<
        (
            crate::ast::ElaboratorSchedule,
            crate::ast::LexicalCallablePath,
            u32,
        ),
        Error,
    > {
        if matches!(self.peek(), Some(TokenKind::LParen)) {
            self.cursor_choices_at(
                CursorSlot::Grammar,
                std::iter::once("fills"),
                1,
                Some(self.peek_span().end),
            );
        }
        let schedule = if self.has_fills_marker_prefix() {
            self.advance();
            let marker = self.advance().span;
            self.record_keyword(marker, KeywordRole::Declaration);
            self.advance();
            crate::ast::ElaboratorSchedule::Fills
        } else {
            crate::ast::ElaboratorSchedule::Late
        };
        let (implementation, _) = self.parse_lexical_callable_path(
            "elaborator implementations must name a lexical callable",
            "move any reordering or wrapping logic into a named `pure fn`",
        )?;
        Ok((schedule, implementation, self.syntax_end))
    }

    fn parse_elaborator_captures(
        &mut self,
    ) -> Result<Vec<crate::ast::UserElaboratorCapture>, Error> {
        self.expect_ident_named("captures")?;
        let mut captures = Vec::new();
        if matches!(self.peek(), Some(TokenKind::LParen)) {
            self.advance();
            while !matches!(self.peek(), Some(TokenKind::RParen)) {
                captures.push(self.parse_elaborator_capture_path()?);
                if matches!(self.peek(), Some(TokenKind::Comma)) {
                    self.advance();
                    if matches!(self.peek(), Some(TokenKind::RParen)) {
                        break;
                    }
                } else {
                    break;
                }
            }
            self.expect_kind(TokenKind::RParen, "`)` after `captures (...)`")?;
        } else {
            captures.push(self.parse_elaborator_capture_path()?);
        }
        if captures.is_empty() {
            return Err(self.err_here("expected at least one name in `captures`"));
        }
        Ok(captures)
    }

    fn parse_elaborator_capture_path(
        &mut self,
    ) -> Result<crate::ast::UserElaboratorCapture, Error> {
        let (segments, span_end) = self.value_path_segments()?;
        Self::validate_elaborator_capture_path(&segments)?;
        let start = segments
            .first()
            .map(|segment| segment.span.start)
            .unwrap_or(span_end);
        Ok(crate::ast::UserElaboratorCapture {
            segments,
            span: Span::new(start, span_end),
        })
    }

    fn rec_group_item(
        &mut self,
        leading_vis: Visibility,
        leading_vis_span: Option<Span>,
    ) -> Result<Item, Error> {
        let start = self.peek_span().start;
        let rec_span = self.expect_ident_named("rec")?;
        self.cursor_choices(CursorSlot::Grammar, &["newtype", "labels"]);
        if self.peek_ident_named("newtype") {
            let mut declaration = self.with_recursive_type_scope(rec_span, |parser| {
                parser.newtype(ItemModifiers {
                    vis: leading_vis,
                    vis_span: leading_vis_span,
                    purity: Purity::Impure,
                    host: false,
                })
            })?;
            declaration.rec_span = Some(rec_span);
            return Ok(Item::Newtype(declaration));
        }
        if self.peek_ident_named("labels") {
            let mut declaration = self.with_recursive_type_scope(rec_span, |parser| {
                parser.labels(ItemModifiers {
                    vis: leading_vis,
                    vis_span: leading_vis_span,
                    purity: Purity::Impure,
                    host: false,
                })
            })?;
            declaration.rec_span = Some(rec_span);
            return Ok(Item::Labels(declaration, ()));
        }
        if self.peek_ident_named("type") {
            return Err(self
                .err_here("`rec type` is not a declaration form")
                .with_help(
                    "transparent aliases may participate only in a nominally grounded mutual `rec { ... }` group",
                ));
        }
        if matches!(self.peek(), Some(TokenKind::LBrace)) {
            return self.type_rec_group_after_rec(start, leading_vis, leading_vis_span, true);
        }
        self.expect_kind(TokenKind::LParen, "`(` after `rec`")?;
        let (loop_path, _) = self.value_path_segments()?;
        Self::validate_reference_value_path_leaf(&loop_path)?;
        self.expect_kind(TokenKind::RParen, "`)` after `rec(...)`")?;
        if matches!(self.peek(), Some(TokenKind::LBrace)) {
            if leading_vis.is_pub() {
                return Err(self.err_here(
                    "a braced `rec(...)` group has no leading visibility; put visibility on each member",
                ));
            }
            if self
                .tooling
                .as_ref()
                .is_some_and(|tooling| tooling.collect_roles)
            {
                return self.rec_group_with_tooling(start, loop_path);
            }
            self.advance();
            let mut members = Vec::new();
            let mut pending = self.leading_block_separator(false)?;
            while !matches!(self.peek(), Some(TokenKind::RBrace)) {
                let mut leading = std::mem::take(&mut pending);
                leading.extend(self.peek_leading_trivia());
                let doc = extract_doc_comment(&mut leading);
                let mut member = self.rec_group_member_fn(Visibility::Private)?;
                member.doc = doc;
                inject_meta_trivia(&mut member.meta, leading);
                members.push(member);
                pending = self.block_separator(false, false, &["pub", "fn", "pure"])?;
            }
            pending.extend(self.peek_leading_trivia());
            let end = self
                .expect_kind(TokenKind::RBrace, "`}` closing `rec(...)` group")?
                .end;
            if members.is_empty() {
                return Err(self.err(
                    Span::new(start, end),
                    "`rec(...) { ... }` must contain at least one `fn` member",
                ));
            }
            return Ok(Item::RecGroup(
                RecGroup {
                    loop_path,
                    members,
                    meta: {
                        let mut meta = Meta::new(Span::new(start, end));
                        set_trailing_trivia(&mut meta, pending);
                        meta
                    },
                },
                (),
            ));
        }
        let member = self.rec_group_member_fn(leading_vis)?;
        let end = member.meta.span.end;
        Ok(Item::RecGroup(
            RecGroup {
                loop_path,
                members: vec![member],
                meta: Meta::new(Span::new(start, end)),
            },
            (),
        ))
    }

    fn rec_group_with_tooling(
        &mut self,
        start: u32,
        loop_path: Vec<PathSegment>,
    ) -> Result<Item, Error> {
        self.advance();
        let scope = self.begin_scope_region(ScopeOwnerKind::RecursiveGroup, None);
        let interval = scope.and_then(|(owner, _)| {
            self.tooling
                .as_ref()?
                .facts
                .regions
                .iter()
                .find(|region| {
                    region.open == owner && region.kind == ScopeOwnerKind::RecursiveGroup
                })
                .map(|region| region.interior)
        });
        let mut names = Vec::new();
        let mut polymorphic = false;
        let result = (|| {
            let mut members = Vec::new();
            let mut first_error = None;
            let mut pending = self.leading_block_separator(false)?;
            self.cursor_choices(CursorSlot::Grammar, &["pub", "fn"]);
            while !matches!(self.peek(), Some(TokenKind::RBrace)) {
                let mut leading = std::mem::take(&mut pending);
                leading.extend(self.peek_leading_trivia());
                let doc = extract_doc_comment(&mut leading);
                let (modifiers, header) = match self
                    .rec_member_modifiers(Visibility::Private)
                    .and_then(|modifiers| self.function_header().map(|header| (modifiers, header)))
                {
                    Ok(parsed) => parsed,
                    Err(error) => return Err(first_error.unwrap_or(error)),
                };
                if interval.is_some() {
                    names.push(PathSegment::new(header.name.clone(), header.name_span));
                    polymorphic |= header
                        .sig
                        .params
                        .iter()
                        .any(|param| matches!(param, SignatureParam::Type(_)));
                }
                let body_open = header.body_open;
                let body_cursor = self.cursor.clone();
                match self.function_body(modifiers, header) {
                    Ok(mut member) => {
                        member.doc = doc;
                        inject_meta_trivia(&mut member.meta, leading);
                        members.push(member);
                    }
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                        self.cursor = body_cursor;
                        if let Err(error) = self.skip_open_body_group(body_open) {
                            return Err(first_error.unwrap_or(error));
                        }
                    }
                }
                pending = self.block_separator(false, false, &["pub", "fn", "pure"])?;
                self.cursor_choices(CursorSlot::Grammar, &["pub", "fn"]);
            }
            pending.extend(self.peek_leading_trivia());
            let end = self
                .expect_kind(TokenKind::RBrace, "`}` closing `rec(...)` group")?
                .end;
            if let Some(error) = first_error {
                return Err(error);
            }
            if members.is_empty() {
                return Err(self.err(
                    Span::new(start, end),
                    "`rec(...) { ... }` must contain at least one `fn` member",
                ));
            }
            Ok(Item::RecGroup(
                RecGroup {
                    loop_path,
                    members,
                    meta: {
                        let mut meta = Meta::new(Span::new(start, end));
                        set_trailing_trivia(&mut meta, pending);
                        meta
                    },
                },
                (),
            ))
        })();
        if let Some((owner, _)) = scope
            && let Some(interval) = interval
            && !names.is_empty()
        {
            self.tooling
                .as_mut()
                .expect("region owns a sink")
                .facts
                .scope_prefix
                .push(ScopePrefix {
                    owner,
                    owner_kind: ScopeOwnerKind::RecursiveGroup,
                    interval,
                    syntax: ScopeSyntax::RecursiveMembers { names, polymorphic },
                });
        }
        self.finish_body_scope(scope, None);
        result
    }

    fn rec_group_item_deferred(
        &mut self,
        leading_vis: Visibility,
        leading_vis_span: Option<Span>,
        start: u32,
    ) -> Result<Item, Error> {
        let rec_span = self.expect_ident_named("rec")?;
        if self.peek_ident_named("newtype") {
            let mut declaration = self.with_recursive_type_scope(rec_span, |parser| {
                parser.newtype(ItemModifiers {
                    vis: leading_vis,
                    vis_span: leading_vis_span,
                    purity: Purity::Impure,
                    host: false,
                })
            })?;
            declaration.rec_span = Some(rec_span);
            return Ok(Item::Newtype(declaration));
        }
        if self.peek_ident_named("labels") {
            let mut declaration = self.with_recursive_type_scope(rec_span, |parser| {
                parser.labels(ItemModifiers {
                    vis: leading_vis,
                    vis_span: leading_vis_span,
                    purity: Purity::Impure,
                    host: false,
                })
            })?;
            declaration.rec_span = Some(rec_span);
            return Ok(Item::Labels(declaration, ()));
        }
        if self.peek_ident_named("type") {
            return Err(self
                .err_here("`rec type` is not a declaration form")
                .with_help(
                    "transparent aliases may participate only in a nominally grounded mutual `rec { ... }` group",
                ));
        }
        if matches!(self.peek(), Some(TokenKind::LBrace)) {
            return self.type_rec_group_after_rec(start, leading_vis, leading_vis_span, true);
        }
        self.expect_kind(TokenKind::LParen, "`(` after `rec`")?;
        let (loop_path, _) = self.value_path_segments()?;
        Self::validate_reference_value_path_leaf(&loop_path)?;
        self.expect_kind(TokenKind::RParen, "`)` after `rec(...)`")?;
        if matches!(self.peek(), Some(TokenKind::LBrace)) {
            if leading_vis.is_pub() {
                return Err(self.err_here(
                    "a braced `rec(...)` group has no leading visibility; put visibility on each member",
                ));
            }
            self.advance();
            let mut members = Vec::new();
            let mut pending = self.leading_block_separator(false)?;
            while !matches!(self.peek(), Some(TokenKind::RBrace)) {
                let mut leading = std::mem::take(&mut pending);
                leading.extend(self.peek_leading_trivia());
                let doc = extract_doc_comment(&mut leading);
                let member_start = self.peek_span().start;
                let mut member =
                    self.rec_group_member_fn_deferred(Visibility::Private, member_start)?;
                member.doc = doc;
                inject_meta_trivia(&mut member.meta, leading);
                members.push(member);
                pending = self.block_separator(false, false, &["pub", "fn", "pure"])?;
            }
            pending.extend(self.peek_leading_trivia());
            let end = self
                .expect_kind(TokenKind::RBrace, "`}` closing `rec(...)` group")?
                .end;
            if members.is_empty() {
                return Err(self.err(
                    Span::new(start, end),
                    "`rec(...) { ... }` must contain at least one `fn` member",
                ));
            }
            return Ok(Item::RecGroup(
                RecGroup {
                    loop_path,
                    members,
                    meta: {
                        let mut meta = Meta::new(Span::new(start, end));
                        set_trailing_trivia(&mut meta, pending);
                        meta
                    },
                },
                (),
            ));
        }
        let member_start = self.peek_span().start;
        let member = self.rec_group_member_fn_deferred(leading_vis, member_start)?;
        let end = member.meta.span.end;
        Ok(Item::RecGroup(
            RecGroup {
                loop_path,
                members: vec![member],
                meta: Meta::new(Span::new(start, end)),
            },
            (),
        ))
    }

    fn type_rec_group_after_rec(
        &mut self,
        start: u32,
        leading_vis: Visibility,
        leading_vis_span: Option<Span>,
        surface: bool,
    ) -> Result<Item, Error> {
        let open_brace_span = self.expect_kind(TokenKind::LBrace, "`{` after `rec`")?;
        let scope = self.begin_scope_region(ScopeOwnerKind::RecursiveGroup, None);
        if let Some((owner, _)) = scope
            && let Some(tooling) = &mut self.tooling
            && let Some(region) = tooling
                .facts
                .regions
                .iter()
                .find(|region| region.open == owner)
        {
            tooling.facts.scope_prefix.push(ScopePrefix {
                owner,
                owner_kind: ScopeOwnerKind::RecursiveGroup,
                interval: region.interior,
                syntax: ScopeSyntax::RecursiveTypes(Vec::new()),
            });
        }
        let result = self.type_rec_group_body(
            start,
            leading_vis,
            leading_vis_span,
            surface,
            open_brace_span,
        );
        self.finish_body_scope(scope, None);
        result
    }

    fn type_rec_group_body(
        &mut self,
        start: u32,
        leading_vis: Visibility,
        leading_vis_span: Option<Span>,
        surface: bool,
        open_brace_span: Span,
    ) -> Result<Item, Error> {
        let mut members = Vec::new();
        let mut first_error = None;
        let mut member_source_spans = Vec::new();
        let mut member_marker_offsets = Vec::new();
        let mut separator_spans = Vec::new();
        let mut source_layout_is_unambiguous = true;
        if matches!(self.peek(), Some(TokenKind::Semicolon)) {
            separator_spans.push(self.peek_span());
        }
        let mut pending = self.leading_block_separator(false)?;
        self.declaration_keywords(
            DeclarationContext::TypeGroup { surface },
            &ItemModifiers::impure_private(),
            false,
        );
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            if self.peek().is_none() {
                return Err(first_error.unwrap_or_else(|| {
                    self.err_here("expected `}` closing bare `rec` type group")
                }));
            }
            let recovery = self
                .tooling
                .as_ref()
                .filter(|tooling| tooling.collect_roles)
                .map(|_| {
                    let next = self
                        .cursor
                        .remaining_current_frame()
                        .iter()
                        .find_map(|node| match node {
                            SkeletonNode::Leaf(Token {
                                kind: TokenKind::Semicolon,
                                span,
                                ..
                            }) => Some(span.end),
                            _ => None,
                        })
                        .unwrap_or_else(|| {
                            self.cursor
                                .current_group_close_span()
                                .map_or(self.eof, |span| span.start)
                        });
                    (self.cursor.clone(), next)
                });
            let mut leading = std::mem::take(&mut pending);
            leading.extend(self.peek_leading_trivia());
            // A plain comment between two members can describe either side;
            // unlike `///`, it has no declaration attachment in the grammar.
            // Keep parsing/formatting it, but withhold structural move/split
            // fixes instead of guessing ownership. A comment before the first
            // member is unambiguously group-leading and may move with it.
            if !members.is_empty()
                && leading
                    .iter()
                    .any(|trivia| matches!(trivia, Trivia::LineComment { .. }))
            {
                source_layout_is_unambiguous = false;
            }
            let leading_comment_span = trivia_comment_span(&leading);
            let doc = extract_doc_comment(&mut leading);
            let parsed_member = (|| {
                let modifiers =
                    self.item_modifiers_in(DeclarationContext::TypeGroup { surface })?;
                let member_start = leading_comment_span
                    .map(|span| span.start)
                    .or_else(|| modifiers.vis_span.map(|span| span.start))
                    .unwrap_or_else(|| self.peek_span().start);
                let marker_offset = self.peek_span().start;
                if modifiers.purity.is_pure() || modifiers.host {
                    return Err(self.err_here(
                    "a bare `rec { ... }` type group admits only `type`, `newtype`, and `labels` declarations",
                ));
                }
                let member = if self.peek_ident_named("type") {
                    TypeRecMember::TypeAlias(
                        self.type_alias_in(modifiers, DeclarationEnd::BlockEntry)?,
                    )
                } else if self.peek_ident_named("newtype") {
                    TypeRecMember::Newtype(self.newtype(modifiers)?)
                } else if self.peek_ident_named("labels") {
                    TypeRecMember::Labels(
                        self.labels_in(modifiers, DeclarationEnd::BlockEntry)?,
                        (),
                    )
                } else if self.peek_ident_named("rec") {
                    let nested_rec_span = self.expect_ident_named("rec")?;
                    if self.peek_ident_named("newtype") {
                        let mut newtype = self.newtype(modifiers)?;
                        newtype.rec_span = Some(nested_rec_span);
                        TypeRecMember::Newtype(newtype)
                    } else if self.peek_ident_named("labels") {
                        let mut labels = self.labels_in(modifiers, DeclarationEnd::BlockEntry)?;
                        labels.rec_span = Some(nested_rec_span);
                        TypeRecMember::Labels(labels, ())
                    } else if self.peek_ident_named("type") {
                        return Err(self
                        .err_here("`rec type` is not a declaration form")
                        .with_help(
                            "transparent aliases participate directly in the enclosing nominally grounded group",
                        ));
                    } else {
                        return Err(self.err_here(
                            "expected `newtype` or `labels` after a nested `rec` marker",
                        ));
                    }
                } else if self.peek_ident_named("fn") {
                    return Err(self
                        .err_here("a bare `rec { ... }` type group admits only type declarations")
                        .with_help("recursive functions use `rec(loop)`"));
                } else {
                    return Err(self.err_here(
                        "expected `type`, `newtype`, or `labels` in bare `rec { ... }` type group",
                    ));
                };
                Ok((member, member_start, marker_offset))
            })();
            let (mut member, member_start, marker_offset) = match parsed_member {
                Ok(member) => member,
                Err(error) => {
                    let Some((cursor, next)) = recovery else {
                        return Err(error);
                    };
                    first_error.get_or_insert(error);
                    self.cursor = cursor;
                    while self.peek().is_some() && self.peek_span().start < next {
                        self.advance();
                    }
                    continue;
                }
            };
            match &mut member {
                TypeRecMember::TypeAlias(alias) => alias.doc = doc,
                TypeRecMember::Newtype(newtype) => newtype.doc = doc,
                TypeRecMember::Labels(labels, _) => labels.doc = doc,
            }
            inject_meta_trivia(member.meta_mut(), leading);
            member_source_spans.push(Span::new(member_start, member.meta().span.end));
            member_marker_offsets.push(marker_offset);
            members.push(member);
            if matches!(self.peek(), Some(TokenKind::Semicolon)) {
                separator_spans.push(self.peek_span());
            }
            pending =
                self.block_separator(false, false, &["pub", "type", "newtype", "labels", "rec"])?;
            self.declaration_keywords(
                DeclarationContext::TypeGroup { surface },
                &ItemModifiers::impure_private(),
                false,
            );
        }
        let mut close_leading = pending;
        close_leading.extend(self.peek_leading_trivia());
        let trailing_comment_span = trivia_comment_span(&close_leading);
        let close_brace_span =
            self.expect_kind(TokenKind::RBrace, "`}` closing bare `rec` type group")?;
        let end = close_brace_span.end;
        if let Some(error) = first_error {
            return Err(error);
        }
        if members.is_empty() {
            return Err(self.err(
                Span::new(start, end),
                "a bare `rec { ... }` type group must contain type declarations",
            ));
        }
        let mut meta = Meta::new(Span::new(start, end));
        set_trailing_trivia(&mut meta, close_leading);
        let group = TypeRecGroup {
            members,
            doc: None,
            source_layout: source_layout_is_unambiguous.then_some(
                crate::ast::TypeRecSourceLayout {
                    member_spans: member_source_spans,
                    separator_spans,
                    member_marker_offsets,
                    trailing_comment_span,
                },
            ),
            rec_span: Some(Span::new(start, start.saturating_add(3))),
            open_brace_span: Some(open_brace_span),
            close_brace_span: Some(close_brace_span),
            deferred_rec_labels_diagnostic: None,
            meta,
        };
        if leading_vis.is_pub() {
            let vis_span =
                leading_vis_span.expect("public item modifier retains its exact source span");
            let mut error = self
                .err(
                    vis_span,
                    "a bare `rec { ... }` type group has no leading visibility",
                )
                .with_help("put the same visibility on each type-group member");
            let every_member_is_private = group.members.iter().all(|member| match member {
                TypeRecMember::TypeAlias(alias) => !alias.vis.is_pub(),
                TypeRecMember::Newtype(newtype) => !newtype.vis.is_pub(),
                TypeRecMember::Labels(labels, _) => !labels.vis.is_pub(),
            });
            if every_member_is_private {
                let prefix = match &leading_vis {
                    Visibility::Private => unreachable!("checked public visibility above"),
                    Visibility::Public => "pub ".to_owned(),
                    Visibility::PublicIn(path) => {
                        format!("pub({}) ", path.segments.join("/"))
                    }
                };
                let mut edits = vec![FixEdit::new(vis_span, "")];
                edits.extend(group.members.iter().map(|member| {
                    FixEdit::new(
                        Span::new(member.meta().span.start, member.meta().span.start),
                        prefix.clone(),
                    )
                }));
                error = error.with_fix(Fix::machine_applicable(
                    "Put visibility on each recursive type member",
                    edits,
                ));
            }
            return Err(error);
        }
        Ok(Item::TypeRecGroup(group))
    }

    fn rec_group_member_fn_deferred(
        &mut self,
        leading_vis: Visibility,
        start: u32,
    ) -> Result<FnDef, Error> {
        let mut vis = leading_vis;
        if self.peek_ident_named("pub") {
            if vis.is_pub() {
                return Err(self.err_here("duplicate `pub` modifier on `rec` member"));
            }
            let (member_vis, _) = self.parse_visibility_prefix()?;
            vis = member_vis;
        }
        if self.peek_ident_named("pure") {
            return Err(self.err_here(PURE_REC_DIAGNOSTIC));
        }
        if !self.peek_ident_named("fn") {
            return Err(self.err_here("expected `fn` or visibility-modified `fn` after `rec(...)`"));
        }
        self.fn_item_deferred(
            ItemModifiers {
                vis,
                vis_span: None,
                purity: Purity::Impure,
                host: false,
            },
            start,
        )
    }

    fn rec_group_member_fn(&mut self, leading_vis: Visibility) -> Result<FnDef, Error> {
        let modifiers = self.rec_member_modifiers(leading_vis)?;
        if !self
            .tooling
            .as_ref()
            .is_some_and(|tooling| tooling.collect_roles)
        {
            return self.fn_item(modifiers);
        }
        let header = self.function_header()?;
        let owner = header.body_open;
        let polymorphic = header
            .sig
            .params
            .iter()
            .any(|param| matches!(param, SignatureParam::Type(_)));
        let name = self.tooling.as_ref().and_then(|tooling| {
            let cursor = tooling.cursor?;
            let close = self.cursor.current_group_close_span()?;
            let end = if self.cursor.just_entered_recovered() {
                self.eof.min(tooling.cursor_limit)
            } else {
                close.start
            };
            (owner.end <= cursor && cursor <= end)
                .then(|| PathSegment::new(header.name.clone(), header.name_span))
        });
        let result = self.function_body(modifiers, header);
        if let Some(name) = name
            && let Some(tooling) = &mut self.tooling
            && let Some(region) = tooling
                .facts
                .regions
                .iter()
                .find(|region| region.open == owner && region.kind == ScopeOwnerKind::Body)
        {
            tooling.facts.scope_prefix.push(ScopePrefix {
                owner,
                owner_kind: ScopeOwnerKind::Body,
                interval: region.interior,
                syntax: ScopeSyntax::RecursiveMembers {
                    names: vec![name],
                    polymorphic,
                },
            });
        }
        result
    }

    fn rec_member_modifiers(&mut self, leading_vis: Visibility) -> Result<ItemModifiers, Error> {
        let mut vis = leading_vis;
        self.cursor_choices_from(
            CursorSlot::Grammar,
            ["pub", "fn"]
                .into_iter()
                .filter(|keyword| *keyword != "pub" || !vis.is_pub()),
        );
        if self.peek_ident_named("pub") {
            if vis.is_pub() {
                return Err(self.err_here("duplicate `pub` modifier on `rec` member"));
            }
            let (member_vis, _) = self.parse_visibility_prefix()?;
            vis = member_vis;
        }
        self.cursor_choices(CursorSlot::Grammar, &["fn"]);
        if self.peek_ident_named("pure") {
            return Err(self.err_here(PURE_REC_DIAGNOSTIC));
        }
        if !self.peek_ident_named("fn") {
            return Err(self.err_here("expected `fn` or visibility-modified `fn` after `rec(...)`"));
        }
        Ok(ItemModifiers {
            vis,
            vis_span: None,
            purity: Purity::Impure,
            host: false,
        })
    }

    /// If the next token is a retired item-leading keyword, return its
    /// spelling so the item parser can emit a directed "use `<new>`"
    /// diagnostic.
    fn peek_renamed_item_keyword(&self) -> Option<&'static str> {
        ["defn", "deftype", "defop"]
            .into_iter()
            .find(|&kw| self.peek_ident_named(kw))
    }

    /// Parse a `literal` declaration:
    ///
    ///   `pub? literal name = <bare-literal> ;`
    fn literal_alias(&mut self, vis: Visibility) -> Result<LiteralAlias, Error> {
        let start = self.peek_span().start;
        self.expect_ident_named("literal")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        self.expect_sym("=", "`=`")?;
        let value = self.literal_alias_value()?;
        let semi_end = self
            .expect_kind(TokenKind::Semicolon, "`;` after the literal declaration")?
            .end;
        Ok(LiteralAlias {
            vis,
            name,
            value,
            meta: Meta::new(Span::new(start, semi_end)),
            doc: None,
        })
    }

    fn literal_alias_value(&mut self) -> Result<LiteralAliasValue, Error> {
        match self.peek() {
            Some(TokenKind::StrLit(_)) => {
                let tok = self.advance();
                let TokenKind::StrLit(value) = tok.kind else {
                    unreachable!()
                };
                Ok(LiteralAliasValue::Str {
                    value,
                    span: tok.span,
                })
            }
            Some(TokenKind::IntLit { .. }) => {
                let tok = self.advance();
                let TokenKind::IntLit { digits } = tok.kind else {
                    unreachable!()
                };
                Ok(LiteralAliasValue::Int {
                    digits,
                    span: tok.span,
                })
            }
            Some(TokenKind::FloatLit { .. }) => {
                let tok = self.advance();
                let TokenKind::FloatLit { digits } = tok.kind else {
                    unreachable!()
                };
                Ok(LiteralAliasValue::Float {
                    digits,
                    span: tok.span,
                })
            }
            Some(TokenKind::BoolLit(_)) => {
                let tok = self.advance();
                let TokenKind::BoolLit(value) = tok.kind else {
                    unreachable!()
                };
                Ok(LiteralAliasValue::Bool {
                    value,
                    span: tok.span,
                })
            }
            _ => Err(self.err_here("expected a bare literal token after `=`")),
        }
    }

    /// Parse a `op` declaration:
    ///
    ///   op  PatternPart+  { impl Path; } ;
    ///
    /// where `PatternPart` is `_` (plain slot), `__` (recursive
    /// slot — at most one per pattern), or a `SymbolRun` literal.
    /// Validates the pattern shape:
    /// - at most one `__` slot
    /// - no two adjacent slots
    /// - at least one operator token
    /// - the recursive slot (`__`), if present, sits at one of
    ///   the operand-position ends (first or last operand slot)
    /// - each operator token satisfies the reserved-spelling rules
    ///   (dot-led one-dot runs and the `//…` family are held back).
    ///
    /// Supported pattern shapes:
    ///   - **Binary**: `_ OP _`, `_ OP __`, `__ OP _`
    ///   - **Prefix-unary**: `OP _`, `OP __`
    ///   - **Postfix-unary**: `_ OP`, `__ OP`
    ///   - **Multi-token** (≥2 operator tokens with operand slots
    ///     between them): `_ OP1 _ OP2 _`, `_ ? _ : _`,
    ///     `_ ? _ : __`, `_ ? __ : _`. Multi-token patterns can
    ///     freely use language-reserved tokens (e.g., `:`) since
    ///     the reserved-standalone-token rule applies only to the
    ///     single-token case.
    fn op(&mut self, modifiers: ItemModifiers) -> Result<Op, Error> {
        let start = self.peek_span().start;
        self.expect_ident_named("op")?;
        let pattern = self.parse_op_pattern(false)?;
        let function = self.parse_normal_op_body()?;
        let end = self.syntax_end;
        Ok(Op {
            vis: modifiers.vis.clone(),
            body: OpBody::Normal { pattern, function },
            body_trivia: Vec::new(),
            meta: Meta::new(Span::new(start, end)),
            doc: None,
        })
    }

    fn parse_op_pattern(&mut self, import_item: bool) -> Result<Vec<OpPart>, Error> {
        let start = self.peek_span().start;
        let mut pattern: Vec<OpPart> = Vec::new();
        let mut last_was_slot = false;
        // `Some(group_start_span)` while inside a `( … )` lenient
        // grouping marker. Tracked across pattern parts so slots
        // emitted while open carry `lenient = true`.
        let mut group_open: Option<Span> = None;
        loop {
            match self.peek() {
                Some(TokenKind::Slot1) => {
                    let span = self.peek_span();
                    self.advance();
                    if last_was_slot {
                        return Err(
                            self.err(span, "two adjacent slots in `op` pattern are ambiguous")
                        );
                    }
                    pattern.push(OpPart::SlotPlain {
                        span,
                        lenient: group_open.is_some(),
                    });
                    last_was_slot = true;
                }
                Some(TokenKind::Slot2) => {
                    let span = self.peek_span();
                    self.advance();
                    if last_was_slot {
                        return Err(
                            self.err(span, "two adjacent slots in `op` pattern are ambiguous")
                        );
                    }
                    if pattern.iter().any(|p| {
                        matches!(p, OpPart::SlotRecursive { .. } | OpPart::SlotGreedy { .. })
                    }) {
                        return Err(self.err(
                            span,
                            "at most one recursive slot (`__` or `___`) per `op` pattern \
                             (the recursive slot determines associativity)",
                        ));
                    }
                    pattern.push(OpPart::SlotRecursive {
                        span,
                        lenient: group_open.is_some(),
                    });
                    last_was_slot = true;
                }
                Some(TokenKind::Slot3) => {
                    let span = self.peek_span();
                    self.advance();
                    if last_was_slot {
                        return Err(
                            self.err(span, "two adjacent slots in `op` pattern are ambiguous")
                        );
                    }
                    if group_open.is_some() {
                        return Err(self.err(
                            span,
                            "`___` is redundant inside a `( … )` lenient-grouping marker — \
                             the greedy slot is already full-chain at use sites",
                        ));
                    }
                    if pattern.iter().any(|p| {
                        matches!(p, OpPart::SlotRecursive { .. } | OpPart::SlotGreedy { .. })
                    }) {
                        return Err(self.err(
                            span,
                            "at most one recursive slot (`__` or `___`) per `op` pattern; \
                             dual-greedy patterns have no position info to pin associativity",
                        ));
                    }
                    pattern.push(OpPart::SlotGreedy { span });
                    last_was_slot = true;
                }
                Some(TokenKind::Ident(s)) if s.chars().all(|c| c == '_') && s.len() >= 4 => {
                    let span = self.peek_span();
                    return Err(self.err(
                        span,
                        "underscore runs of 4 or more (`____`+) are reserved; \
                         only `_`, `__`, and `___` are slot tokens in `op` patterns",
                    ));
                }
                Some(TokenKind::LParen) => {
                    let span = self.peek_span();
                    self.advance();
                    if group_open.is_some() {
                        return Err(self.err(
                            span,
                            "nested `( … )` lenient-grouping markers are not permitted in \
                             `op` patterns",
                        ));
                    }
                    group_open = Some(span);
                    // Reset slot-adjacency tracking: the `(` is a
                    // pattern boundary, not a slot.
                    last_was_slot = false;
                }
                Some(TokenKind::RParen) => {
                    let span = self.peek_span();
                    if import_item && group_open.is_none() {
                        break;
                    }
                    let Some(open_span) = group_open else {
                        return Err(
                            self.err(span, "unmatched `)` in `op` pattern (no preceding `(`)")
                        );
                    };
                    self.advance();
                    // Reject empty `()` groups: there must be at
                    // least one slot or token between `(` and `)`.
                    let group_start_idx = pattern
                        .iter()
                        .position(|p| op_part_span(p).start > open_span.start);
                    let Some(group_start_idx) = group_start_idx else {
                        return Err(self.err(
                            Span::new(open_span.start, span.end),
                            "empty `( )` grouping marker — must contain at least one slot \
                             or token",
                        ));
                    };
                    // Slots inside ⇒ slot-grouping marker; only
                    // tokens inside ⇒ operator-token quotation.
                    let group_has_slots = pattern[group_start_idx..].iter().any(|p| {
                        matches!(
                            p,
                            OpPart::SlotPlain { .. }
                                | OpPart::SlotRecursive { .. }
                                | OpPart::SlotGreedy { .. }
                        )
                    });
                    if !group_has_slots {
                        for part in pattern[group_start_idx..].iter_mut() {
                            if let OpPart::Token {
                                lenient, quoted, ..
                            } = part
                            {
                                *lenient = false;
                                *quoted = true;
                            }
                        }
                    }
                    group_open = None;
                    last_was_slot = false;
                }
                Some(TokenKind::Comma) if import_item && group_open.is_none() => break,
                Some(TokenKind::Comma) | Some(TokenKind::Semicolon) => {
                    let span = self.peek_span();
                    let tok = if matches!(self.peek(), Some(TokenKind::Comma)) {
                        ","
                    } else {
                        ";"
                    };
                    return Err(self.err(
                        span,
                        format!(
                            "`{tok}` is not admissible as an `op` operator token; \
                             commas separate list elements and semicolons separate statements"
                        ),
                    ));
                }
                _ => {
                    // `{` at top level ends the pattern (body
                    // separator); inside a `( … )` group it's
                    // invalid pattern content that falls through to
                    // the generic "expected body" error below.
                    let inside_group = group_open.is_some();
                    if matches!(self.peek(), Some(TokenKind::LBrace)) && !inside_group {
                        break;
                    }
                    let Some(content) = self.peek().and_then(token_kind_to_op_string) else {
                        break;
                    };
                    let span = self.peek_span();
                    if is_reserved_standalone_equals_op_token(&content) && !inside_group {
                        return Err(reserved_standalone_equals_op_token_error(span));
                    }
                    self.advance();
                    pattern.push(OpPart::Token {
                        content,
                        span,
                        lenient: inside_group,
                        quoted: false,
                    });
                    last_was_slot = false;
                }
            }
        }
        if let Some(open_span) = group_open {
            return Err(self.err(
                open_span,
                "unmatched `(` in `op` pattern (no closing `)` before `{`)",
            ));
        }
        if pattern.is_empty() {
            return Err(self.err_here("expected `_`, `__`, or operator characters in `op` pattern"));
        }
        if !pattern.iter().any(|p| matches!(p, OpPart::Token { .. })) {
            return Err(self.err(
                Span::new(start, self.peek_span().start),
                "`op` pattern must include at least one operator token",
            ));
        }
        let pattern_span = Span::new(start, self.peek_span().start);
        validate_op_pattern(&pattern, pattern_span)?;
        Ok(pattern)
    }

    fn variadic_operator(&mut self, modifiers: ItemModifiers) -> Result<VariadicOperator, Error> {
        let start = self.peek_span().start;
        let (open, close) = self.parse_variadic_head()?;
        let (mode, initializer, step, finalize) = self.parse_variadic_op_body()?;
        let end = self.syntax_end;
        let span = Span::new(start, end);
        let spec = crate::ast::VariadicSpec {
            close,
            mode,
            initializer,
            step,
            finalize,
        };
        self.operator_scope
            .insert_variadic(open.clone(), spec.clone())
            .map_err(|message| self.err(span, message))?;
        Ok(VariadicOperator {
            vis: modifiers.vis.clone(),
            open,
            spec: Box::new(spec),
            body_trivia: Vec::new(),
            meta: Meta::new(span),
            doc: None,
        })
    }

    fn parse_variadic_head(&mut self) -> Result<(Vec<String>, Vec<String>), Error> {
        self.expect_ident_named("varop")?;
        let Some(TokenKind::SymbolRun(open)) = self.peek() else {
            return Err(self.err_here("expected a varop OPEN delimiter such as `[[` or `[*`"));
        };
        let Some(close) = mirrored_varop_close(open) else {
            return Err(self.err_here("varop OPEN must be a non-bare symbol run containing `[` and no `]`; both delimiters must obey the dot-led reservation"));
        };
        let open = open.clone();
        let open_end = self.peek_span().end;
        self.advance();
        if self.peek_span().start == open_end {
            return Err(self.err_here("varop OPEN and CLOSE must be separated by whitespace"));
        }
        self.expect_kind(
            TokenKind::sym(&close),
            &format!("mirrored varop CLOSE `{close}`"),
        )?;
        Ok((vec![open], vec![close]))
    }

    fn parse_normal_op_body(&mut self) -> Result<crate::ast::LexicalCallablePath, Error> {
        self.expect_kind(TokenKind::LBrace, "`{` after `op` pattern")?;
        self.leading_block_separator(false)?;
        let mut function: Option<crate::ast::LexicalCallablePath> = None;
        self.cursor_choices(CursorSlot::Grammar, &["impl"]);
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            if self.peek_ident_named("impl") {
                if function.is_some() {
                    return Err(self.err_here("duplicate `impl` field in `op` body"));
                }
                self.expect_ident_named("impl")?;
                function = Some(
                    self.parse_lexical_callable_path(
                        "operator implementations must name a lexical callable",
                        "move any reordering or wrapping logic into a named fn",
                    )?
                    .0,
                );
            } else {
                return Err(self.err_here("expected `impl` field in `op` body"));
            }
            self.block_separator(false, false, &["impl"])?;
            self.cursor_choices(CursorSlot::Grammar, &[]);
        }
        self.expect_kind(TokenKind::RBrace, "`}` closing `op` body")?;
        let Some(function) = function else {
            return Err(self.err_here("`op` body requires an `impl` field"));
        };
        Ok(function)
    }

    fn parse_variadic_op_body(
        &mut self,
    ) -> Result<
        (
            crate::ast::VariadicMode,
            CallableSpec,
            CallableSpec,
            Option<CallableSpec>,
        ),
        Error,
    > {
        self.expect_kind(TokenKind::LBrace, "`{` after the variadic pattern")?;
        self.leading_block_separator(false)?;
        let mut primary = None;
        let mut finalize = None;
        let duplicate_primary = "a variadic operator body admits only one primary fold clause";
        while let Some(field) = self.next_named_entry(
            CursorSlot::Grammar,
            &[
                ("foldl", primary.is_none(), duplicate_primary),
                ("foldr", primary.is_none(), duplicate_primary),
                ("foldl1", primary.is_none(), duplicate_primary),
                ("foldr1", primary.is_none(), duplicate_primary),
                (
                    "finalize",
                    finalize.is_none(),
                    "duplicate `finalize` clause in variadic operator body",
                ),
            ],
            "expected `foldl`, `foldr`, `foldl1`, `foldr1`, or `finalize`",
        )? {
            self.expect_ident_named(field)?;
            if field == "finalize" {
                finalize = Some(self.parse_variadic_callable()?);
            } else {
                let mode = match field {
                    "foldl" => crate::ast::VariadicMode::FoldLeft,
                    "foldr" => crate::ast::VariadicMode::FoldRight,
                    "foldl1" => crate::ast::VariadicMode::FoldLeftOne,
                    "foldr1" => crate::ast::VariadicMode::FoldRightOne,
                    _ => unreachable!("named-entry dispatch selected a primary fold clause"),
                };
                let step = self.parse_variadic_callable()?;
                let initializer = self.parse_variadic_callable()?;
                primary = Some((mode, initializer, step));
            }
            self.block_separator(
                false,
                false,
                &["foldl", "foldr", "foldl1", "foldr1", "finalize"],
            )?;
        }
        let Some((mode, initializer, step)) = primary else {
            return Err(self.err_here(
                "a variadic operator body requires a `foldl`, `foldr`, `foldl1`, or `foldr1` clause",
            ));
        };
        self.expect_kind(TokenKind::RBrace, "`}` closing the variadic operator body")?;
        Ok((mode, initializer, step, finalize))
    }

    fn parse_variadic_callable(&mut self) -> Result<CallableSpec, Error> {
        let (path, _) = self.parse_lexical_callable_path(
            "variadic callables must name a lexical callable",
            "move any reordering or wrapping logic into a named fn",
        )?;
        Ok(CallableSpec { path })
    }

    /// `equiv name[A](x: A) { expr; expr; ... }` —
    /// an equivalence claim that all arm bodies reduce to the
    /// same normal form. Semicolon runs separate arms; repeated and
    /// trailing separators are ignored, and comments attached to
    /// skipped separators flow to the next arm.
    fn equiv(&mut self) -> Result<crate::ast::Equiv, Error> {
        let start = self.peek_span().start;
        self.expect_ident_named("equiv")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        self.record_source_name(name_span, SourceNameRole::Function);
        let sig = if self.peek_starts_signature_group() {
            self.signature()?
        } else {
            Signature::new(Vec::new())
        };
        self.expect_kind(TokenKind::LBrace, "`{` after `equiv` head")?;
        let scope = self.begin_body_scope(Some(&sig));
        let result = self.equiv_terms(start);
        self.finish_body_scope(scope, None);
        let (terms, close_end) = result?;
        Ok(crate::ast::Equiv {
            name,
            name_span,
            sig,
            terms,
            meta: Meta::new(Span::new(start, close_end)),
        })
    }

    fn equiv_terms(&mut self, start: u32) -> Result<(Vec<crate::ast::EquivTerm>, u32), Error> {
        let mut terms: Vec<crate::ast::EquivTerm> = Vec::new();
        let mut pending_separator_trivia: Vec<crate::pass::lexer::Trivia> = Vec::new();
        loop {
            let mut term_trivia = std::mem::take(&mut pending_separator_trivia);
            term_trivia.extend(self.skip_semicolon_separators());
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                // Comment dangling before the closing `}` — stash on
                // the last arm's trailing slot so it survives. The
                // `}`'s own leading trivia holds it; merge it in.
                term_trivia.extend(self.peek_leading_trivia());
                if let Some(last) = terms.last_mut() {
                    set_trailing_trivia(&mut last.meta, term_trivia);
                }
                break;
            }
            if matches!(self.peek(), Some(TokenKind::Comma)) {
                return Err(self.err_here(
                    "`equiv` arms are separated with `;`, not `,`; write \
                     `equiv name { lhs; rhs }` and wrap let-heavy arms in \
                     `.() { ... }()`",
                ));
            }
            term_trivia.extend(self.peek_leading_trivia());
            let term_start = self.peek_span().start;
            let body = self.expr()?;
            let body_end = body.span().end;
            terms.push(crate::ast::EquivTerm {
                body,
                meta: Meta {
                    span: Span::new(term_start, body_end),
                    leading_trivia: term_trivia,
                    trailing_trivia: Vec::new(),
                },
            });
            match self.peek() {
                Some(TokenKind::Semicolon) => {
                    pending_separator_trivia = self.skip_semicolon_separators();
                }
                Some(TokenKind::Comma) => {
                    return Err(self.err_here(
                        "`equiv` arms are separated with `;`, not `,`; write \
                         `equiv name { lhs; rhs }` and wrap let-heavy arms in \
                         `.() { ... }()`",
                    ));
                }
                Some(TokenKind::RBrace) => {}
                _ => {
                    return Err(self.err_here("expected `;` or `}` after `equiv` arm"));
                }
            }
        }
        let close_end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        if terms.len() < 2 {
            return Err(self.err(
                Span::new(start, close_end),
                format!(
                    "`equiv` requires at least two arms (found {}); a singleton or empty \
                     block has nothing to compare",
                    terms.len()
                ),
            ));
        }
        Ok((terms, close_end))
    }

    fn fn_item(&mut self, modifiers: ItemModifiers) -> Result<FnDef, Error> {
        let header = self.function_header()?;
        self.function_body(modifiers, header)
    }

    fn function_header(&mut self) -> Result<FunctionHeader, Error> {
        let owner = self.expect_ident_named("fn")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        self.record_source_name(name_span, SourceNameRole::Function);
        let sig = self.signature()?;
        // The `.> Type` annotation is the one optional type position
        // in Kio's grammar (language.md § Function definitions): when
        // the next token is `{` rather than `.>`, the return type
        // defaults to `()`. Synthesize a `Type::Unit` carrying the
        // brace position so diagnostics keep meaningful spans and
        // record the elision so `kio fmt` can round-trip.
        // Comments wedged on the `->` arrow or the return type (a
        // position the AST gives no slot) are interstitial; hoist them
        // to the top of the body — the nearest canonical line that
        // keeps global source order (after the signature params, which
        // are preserved in place, and before the body's own comments).
        let mut hoisted: Vec<Trivia> = Vec::new();
        let (ret, ret_elided) = if self.at_sym_prefix("->") {
            self.expect_fn_arrow()?;
            // The comment after `->` (before the return type) is the
            // return type's first token's leading trivia; capture it
            // before `type_expr` discards it.
            Self::collect_comments_into(self.peek_leading_trivia(), &mut hoisted);
            (
                self.with_type_parameters(
                    owner,
                    ScopeOwnerKind::Signature,
                    sig.params.iter().filter_map(|param| match param {
                        SignatureParam::Type(param) => Some(param),
                        SignatureParam::Value(_) => None,
                    }),
                    Self::type_expr,
                )?,
                false,
            )
        } else {
            let unit_span = self.peek_span();
            (
                Type::Unit {
                    meta: Meta::new(Span::new(unit_span.start, unit_span.start)),
                },
                true,
            )
        };
        // A comment between the return type and the body's `{` also has
        // no slot; hoist it too.
        Self::collect_comments_into(self.peek_leading_trivia(), &mut hoisted);
        let body_open = self.expect_kind(
            TokenKind::LBrace,
            "`{` to open the function body, after an optional `->` return type",
        )?;
        Ok(FunctionHeader {
            owner,
            name,
            name_span,
            sig,
            ret,
            ret_elided,
            hoisted,
            body_open,
        })
    }

    fn function_body(
        &mut self,
        modifiers: ItemModifiers,
        header: FunctionHeader,
    ) -> Result<FnDef, Error> {
        let FunctionHeader {
            owner,
            name,
            name_span: _,
            sig,
            ret,
            ret_elided,
            hoisted,
            body_open: _,
        } = header;
        let (mut body, body_close_end) = self.block_body_with_signature(Some(&sig))?;
        // Prepend the hoisted return-type comments to the body's leading
        // run so they emit at the top of the block, in source order.
        inject_leading_trivia(&mut body, hoisted);
        Ok(FnDef {
            vis: modifiers.vis.clone(),
            purity: modifiers.purity,
            name,
            sig,
            ret,
            ret_elided,
            body,
            meta: Meta::new(Span::new(owner.start, body_close_end)),
            doc: None,
        })
    }

    fn label_forward(&mut self, modifiers: ItemModifiers) -> Result<LabelForward, Error> {
        let start = self.expect_ident_named("type")?.start;
        let mut content_cursor = self.cursor.clone();
        self.expect_kind(TokenKind::LBrace, "`{` before the forwarded label name")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_value_name(&name, name_span)?;
        if name.starts_with('_') {
            return Err(self.err(
                name_span,
                "a forwarded label name must start with a lowercase letter",
            ));
        }
        self.expect_kind(TokenKind::RBrace, "`}` after the forwarded label name")?;
        self.expect_sym("=", "`=` before the forwarded label target")?;
        self.expect_kind(TokenKind::LBrace, "`{` before the forwarded label target")?;
        let target = self.label_path_parts()?;
        self.expect_kind(TokenKind::RBrace, "`}` after the forwarded label target")?;
        let end = self
            .expect_kind(
                TokenKind::Semicolon,
                "`;` after the label forwarding declaration",
            )?
            .end;
        let mut body_trivia = Vec::new();
        while let Some(token) = content_cursor.advance() {
            Self::collect_comments_into(token.leading_trivia, &mut body_trivia);
            if token.span.end == end {
                break;
            }
        }
        let editable_start = modifiers.vis_span.map_or(start, |span| span.start);
        Ok(LabelForward {
            vis: modifiers.vis,
            name,
            name_span,
            target: target.full,
            target_span: target.full_span,
            body_trivia,
            meta: Meta::new(Span::new(start, end)),
            editable_span: Some(Span::new(editable_start, end)),
            doc: None,
        })
    }

    /// Parse a `type` declaration:
    ///
    ///   `pub? type Name TypeParams? = Type ;`
    fn type_alias(&mut self, modifiers: ItemModifiers) -> Result<TypeAlias, Error> {
        self.type_alias_in(modifiers, DeclarationEnd::Outer)
    }

    fn type_alias_in(
        &mut self,
        modifiers: ItemModifiers,
        end: DeclarationEnd,
    ) -> Result<TypeAlias, Error> {
        let owner = self.peek_span();
        let header = self.type_alias_header()?;
        let TypeAliasHeader {
            start,
            name,
            name_span,
            type_params,
        } = header;
        let body = self.with_type_parameters(
            owner,
            ScopeOwnerKind::Declaration,
            &type_params,
            |parser| {
                if matches!(
                    parser.peek(),
                    Some(
                        TokenKind::IntLit { .. }
                            | TokenKind::FloatLit { .. }
                            | TokenKind::StrLit(_)
                            | TokenKind::BoolLit(_)
                    )
                ) {
                    return Err(parser.err_here("type RHS must be a type expression"));
                }
                parser.type_expr()
            },
        )?;
        let semi_end = self.declaration_end(end, "`;` after the type alias")?;
        let editable_start = modifiers.vis_span.map_or(start, |span| span.start);
        Ok(TypeAlias {
            vis: modifiers.vis.clone(),
            name,
            name_span,
            type_params,
            body,
            meta: Meta::new(Span::new(start, semi_end)),
            editable_span: Some(Span::new(editable_start, semi_end)),
            doc: None,
        })
    }

    fn type_alias_header(&mut self) -> Result<TypeAliasHeader, Error> {
        let start = self.peek_span().start;
        self.expect_ident_named("type")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_type_name(&name, name_span)?;
        self.record_source_name(name_span, SourceNameRole::Type);
        let type_params = if self.peek_starts_type_param_list() {
            self.type_param_list()?
        } else {
            Vec::new()
        };
        self.expect_sym("=", "`=` before the type alias body")?;
        self.record_recursive_type_header(&name, name_span, &type_params);
        Ok(TypeAliasHeader {
            start,
            name,
            name_span,
            type_params,
        })
    }

    fn newtype(&mut self, modifiers: ItemModifiers) -> Result<Newtype, Error> {
        let owner = self.peek_span();
        let header = self.newtype_header()?;
        let NewtypeHeader {
            start,
            name,
            name_span,
            type_params,
            existential_params,
        } = header;
        let payload = self.with_type_parameters(
            owner,
            ScopeOwnerKind::Declaration,
            type_params.iter().chain(&existential_params),
            Self::type_expr,
        )?;

        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut constructor: Option<TypeMember> = None;
        let mut projector: Option<TypeMember> = None;
        let mut pending_separator_trivia: Vec<Trivia> = Vec::new();
        let mut members_trailing: Vec<Trivia> = Vec::new();
        self.newtype_member_choices(false, false, true);
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let mut member_trivia = std::mem::take(&mut pending_separator_trivia);
            member_trivia.extend(self.skip_semicolon_separators());
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                // Comment dangling between a trailing `;` run and `}`.
                members_trailing = member_trivia;
                break;
            }
            member_trivia.extend(self.peek_leading_trivia());
            self.newtype_member_choices(constructor.is_some(), projector.is_some(), true);
            let (member_vis, vis_span) = self.parse_visibility_prefix()?;
            self.newtype_member_choices(constructor.is_some(), projector.is_some(), false);
            let (kw, kw_span) = self.expect_ident()?;
            if matches!(kw.as_str(), "constructor" | "projector") {
                self.record_keyword(kw_span, KeywordRole::Declaration);
            }
            if self.peek_ident_named("pub") {
                return Err(self.err_here(
                    "visibility precedes a newtype member keyword — write `pub constructor mk`, \
                     not `constructor pub mk`",
                ));
            }
            let (member_name, member_span) = self.expect_binder()?;
            Self::validate_value_name(&member_name, member_span)?;
            if matches!(kw.as_str(), "constructor" | "projector") {
                self.record_source_name(member_span, SourceNameRole::Function);
            }
            let member = TypeMember {
                vis: member_vis,
                name: member_name,
                span: Span::new(
                    vis_span.map(|s| s.start).unwrap_or(kw_span.start),
                    member_span.end,
                ),
                leading_trivia: member_trivia,
            };
            match kw.as_str() {
                "constructor" => {
                    if constructor.is_some() {
                        return Err(self.err(kw_span, "duplicate `constructor` item in newtype"));
                    }
                    constructor = Some(member);
                }
                "projector" => {
                    if projector.is_some() {
                        return Err(self.err(kw_span, "duplicate `projector` item in newtype"));
                    }
                    projector = Some(member);
                }
                other => {
                    return Err(self.err(
                        kw_span,
                        format!(
                            "expected `constructor` or `projector` in newtype body, got `{other}`"
                        ),
                    ));
                }
            }
            pending_separator_trivia =
                self.block_separator(true, false, &["pub", "constructor", "projector"])?;
            self.newtype_member_choices(constructor.is_some(), projector.is_some(), true);
        }
        // The `}`'s own leading run holds a comment trailing the last
        // member (or dangling before `}`); fold it onto whatever the
        // loop already gathered so neither is lost. (The loop only
        // captures `members_trailing` when it iterates once more after
        // the last member; when the last member's `;` is directly
        // followed by the comment + `}`, the loop exits on the `while`
        // guard and this is the sole capture point.)
        members_trailing.extend(std::mem::take(&mut pending_separator_trivia));
        members_trailing.extend(self.peek_leading_trivia());
        self.expect_kind(TokenKind::RBrace, "`}`")?;
        let semi_end = self.syntax_end;

        let constructor = constructor
            .ok_or_else(|| self.err(name_span, "newtype is missing a `constructor` item"))?;
        let projector = projector
            .ok_or_else(|| self.err(name_span, "newtype is missing a `projector` item"))?;

        let mut meta = Meta::new(Span::new(start, semi_end));
        // A comment dangling after the last member, before `}`, rides
        // the newtype's trailing slot — the formatter emits it before
        // the closing `}` so it survives.
        set_trailing_trivia(&mut meta, members_trailing);
        let editable_start = modifiers.vis_span.map_or(start, |span| span.start);
        Ok(Newtype {
            vis: modifiers.vis.clone(),
            rec_span: None,
            name,
            name_span,
            type_params,
            existential_params,
            payload,
            constructor,
            projector,
            meta,
            editable_span: Some(Span::new(editable_start, semi_end)),
            // Attached by `set_item_doc` from any preceding `///` block.
            doc: None,
        })
    }

    fn newtype_header(&mut self) -> Result<NewtypeHeader, Error> {
        let start = self.peek_span().start;
        self.expect_ident_named("newtype")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_type_name(&name, name_span)?;
        self.record_source_name(name_span, SourceNameRole::Type);
        let type_params = if self.peek_starts_type_param_list() {
            self.type_param_list()?
        } else {
            Vec::new()
        };
        // Trailing existential binders: `newtype Foo[A] <U> <V> : body`.
        // Zero or more `<X>` atoms, whitespace-separated (no commas), parsed
        // before the `:`. The names are in scope inside the payload.
        let mut existential_params: Vec<TypeParam> = Vec::new();
        while self.at_sym("<") {
            existential_params.push(self.angle_binder()?);
        }
        self.expect_sym(":", "`:` before the newtype payload type")?;
        self.record_recursive_type_header(&name, name_span, &type_params);
        Ok(NewtypeHeader {
            start,
            name,
            name_span,
            type_params,
            existential_params,
        })
    }

    fn newtype_member_choices(&mut self, constructor: bool, projector: bool, visibility: bool) {
        let choices = match (constructor, projector, visibility) {
            (false, false, true) => &["pub", "constructor", "projector"][..],
            (false, false, false) => &["constructor", "projector"],
            (false, true, true) => &["pub", "constructor"],
            (false, true, false) => &["constructor"],
            (true, false, true) => &["pub", "projector"],
            (true, false, false) => &["projector"],
            (true, true, _) => &[],
        };
        self.cursor_choices(CursorSlot::NewtypeMember, choices);
    }

    /// `labels { f: X, g[A]: Y };` (anonymous) or
    /// `labels T[A] = { ... } | { ... };` (named, also introduces a
    /// structural `type T` over the written arms). Each entry's
    /// payload type follows ordinary source-ordered declaration scope.
    /// A `rec labels` declaration supplies its own explicit atomic
    /// recursive scope; a surrounding bare `rec { ... }` supplies the
    /// complete written mutual scope.
    fn labels(&mut self, modifiers: ItemModifiers) -> Result<Labels, Error> {
        self.labels_in(modifiers, DeclarationEnd::Outer)
    }

    fn labels_in(
        &mut self,
        modifiers: ItemModifiers,
        end: DeclarationEnd,
    ) -> Result<Labels, Error> {
        let owner = self.peek_span();
        let visible_until = self.declaration_scope_end();
        let header = self.labels_header()?;
        let LabelsHeader {
            start,
            type_alias_name,
            type_alias_span,
            type_alias_params,
        } = header;
        let mut arms = vec![self.with_type_parameters(
            owner,
            ScopeOwnerKind::Declaration,
            &type_alias_params,
            |parser| parser.labels_arm(visible_until),
        )?];
        if type_alias_name.is_none() && self.at_sym("|") {
            return Err(self
                .err_here("sum label declaration must define a named alias")
                .with_help("write `labels Foo = {a: A} | {b: B};`"));
        }
        while type_alias_name.is_some() && self.at_sym("|") {
            self.advance();
            arms.push(self.with_type_parameters(
                owner,
                ScopeOwnerKind::Declaration,
                &type_alias_params,
                |parser| parser.labels_arm(visible_until),
            )?);
        }
        let semi_end = self.declaration_end(end, "`;` after the labels declaration")?;
        let entries: Vec<LabelEntry> = arms
            .iter()
            .flat_map(|arm| arm.entries.iter().cloned())
            .collect();
        if entries.is_empty() {
            return Err(self.err(
                Span::new(start, semi_end),
                "`labels` block must declare at least one label",
            ));
        }
        let type_alias_arms = if type_alias_name.is_some() {
            Some(arms)
        } else {
            None
        };
        let editable_start = modifiers.vis_span.map_or(start, |span| span.start);
        Ok(Labels {
            vis: modifiers.vis.clone(),
            rec_span: None,
            type_alias_name,
            type_alias_span,
            type_alias_params,
            type_alias_arms,
            entries,
            meta: Meta::new(Span::new(start, semi_end)),
            editable_span: Some(Span::new(editable_start, semi_end)),
            doc: None,
        })
    }

    fn labels_header(&mut self) -> Result<LabelsHeader, Error> {
        let start = self.peek_span().start;
        self.expect_ident_named("labels")?;
        // Named form starts with a type-name identifier; anonymous form
        // jumps straight to `{`. Distinguish by lookahead.
        let (type_alias_name, type_alias_span, type_alias_params) =
            if matches!(self.peek(), Some(TokenKind::Ident(_))) {
                let (name, name_span) = self.expect_binder()?;
                Self::validate_type_name(&name, name_span)?;
                self.record_source_name(name_span, SourceNameRole::Type);
                let params = if self.peek_starts_type_param_list() {
                    self.type_param_list()?
                } else {
                    Vec::new()
                };
                self.expect_sym("=", "`=` before the named labels declaration body")?;
                (Some(name), Some(name_span), params)
            } else {
                (None, None, Vec::new())
            };
        if let (Some(name), Some(span)) = (&type_alias_name, type_alias_span) {
            self.record_recursive_type_header(name, span, &type_alias_params);
        }
        Ok(LabelsHeader {
            start,
            type_alias_name,
            type_alias_span,
            type_alias_params,
        })
    }

    fn labels_arm(&mut self, visible_until: u32) -> Result<LabelsArm, Error> {
        let start = self.peek_span().start;
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut entries: Vec<LabelEntry> = Vec::new();
        let mut first_error = None;
        let mut seen: std::collections::HashMap<String, Span> = std::collections::HashMap::new();
        let mut pending_trivia = self.peek_leading_trivia();
        while matches!(self.peek(), Some(TokenKind::Comma)) {
            self.advance();
            pending_trivia.extend(self.peek_leading_trivia());
        }
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let leading = std::mem::take(&mut pending_trivia);
            let mut entry = match self.label_entry() {
                Ok(entry) => entry,
                Err(error) => {
                    if self
                        .tooling
                        .as_ref()
                        .is_none_or(|tooling| !tooling.collect_roles)
                    {
                        return Err(error);
                    }
                    first_error.get_or_insert(error);
                    if self.skip_commas() {
                        continue;
                    }
                    return Err(first_error.expect("recorded label error"));
                }
            };
            if let Some(first_span) = seen.insert(entry.name.clone(), entry.name_span) {
                return Err(Error::parse(
                    entry.name_span,
                    format!("duplicate label `{}` in this product arm", entry.name),
                )
                .with_secondary(first_span, format!("`{}` first written here", entry.name))
                .with_help("write each label at most once in a product arm"));
            }
            inject_meta_trivia(&mut entry.meta, leading);
            if !entry.is_reuse_marker() {
                self.record_type_declaration(
                    PathSegment::new(
                        crate::ast::mint_label_newtype_name(&entry.name),
                        entry.name_span,
                    ),
                    Span::new(entry.meta.span.end, visible_until),
                );
            }
            entries.push(entry);
            pending_trivia = self.peek_leading_trivia();
            let mut saw_comma = false;
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                saw_comma = true;
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `}`"));
            }
        }
        // Last entry's trailing / dangling comment before the `}` —
        // stash on the last entry so it survives the round-trip.
        let closing_trivia = std::mem::take(&mut pending_trivia);
        if let Some(last) = entries.last_mut() {
            set_trailing_trivia(&mut last.meta, closing_trivia);
        }
        let end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        if entries.is_empty() {
            return Err(self.err(
                Span::new(start, end),
                "`labels` arm must declare at least one label",
            ));
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(LabelsArm {
            entries,
            meta: Meta::new(Span::new(start, end)),
        })
    }

    fn label_entry(&mut self) -> Result<LabelEntry, Error> {
        let header = self.label_entry_header()?;
        let LabelEntryHeader {
            name,
            name_span,
            type_params,
            existential_params,
            payload_start,
        } = header;
        if !matches!(self.peek(), Some(TokenKind::Slot1)) {
            self.record_recursive_type_header(
                &crate::ast::mint_label_newtype_name(&name),
                name_span,
                &type_params,
            );
        }
        let depth = self.cursor.depth();
        let payload = self.with_type_parameters(
            name_span,
            ScopeOwnerKind::Declaration,
            type_params.iter().chain(&existential_params),
            |parser| {
                let result = parser.type_expr();
                if result.is_err()
                    && parser
                        .tooling
                        .as_ref()
                        .is_some_and(|tooling| tooling.collect_roles)
                {
                    while parser.cursor.depth() > depth && parser.peek().is_some() {
                        parser.cursor_choices(CursorSlot::Type, &[]);
                        parser.advance();
                    }
                    while !matches!(
                        parser.peek(),
                        None | Some(TokenKind::Comma | TokenKind::RBrace)
                    ) {
                        parser.cursor_choices(CursorSlot::Type, &[]);
                        parser.advance();
                    }
                    if !matches!(parser.peek(), Some(TokenKind::Comma)) {
                        parser.cursor_choices(CursorSlot::Type, &[]);
                    }
                }
                result
            },
        )?;
        if matches!(payload, Type::Infer { .. }) && payload.span().start != payload_start {
            return Err(Error::parse(
                payload.span(),
                "a label reuse marker must be written as exact bare `_`",
            )
            .with_help(
                "remove the surrounding grouping and write `name: _`, or write an explicit payload type",
            ));
        }
        let payload_end = payload.span().end;
        Ok(LabelEntry {
            name,
            name_span,
            type_params,
            existential_params,
            payload,
            meta: Meta::new(Span::new(name_span.start, payload_end)),
        })
    }

    fn label_entry_header(&mut self) -> Result<LabelEntryHeader, Error> {
        let (name, name_span) = self.expect_binder()?;
        Self::validate_label_name(&name, name_span)?;
        self.record_source_name(name_span, SourceNameRole::Label);
        let type_params = if self.peek_starts_type_param_list() {
            self.type_param_list()?
        } else {
            Vec::new()
        };
        // Trailing existential binders, same shape as `newtype`'s header:
        // `f[A] <l> <r> : body`. Zero or more `<X>` atoms before the `:`.
        let mut existential_params: Vec<TypeParam> = Vec::new();
        while self.at_sym("<") {
            existential_params.push(self.angle_binder()?);
        }
        if self.at_sym(".") {
            return Err(Error::parse(
                self.peek_span(),
                "a `labels` entry must use an unqualified module-local label name",
            )
            .with_secondary(name_span, format!("`{name}` starts this label entry"))
            .with_help(
                "declare the label explicitly in this module, or reuse an earlier local declaration with `name: _`",
            ));
        }
        self.expect_sym(":", "`:`")?;
        let payload_start = self.peek_span().start;
        Ok(LabelEntryHeader {
            name,
            name_span,
            type_params,
            existential_params,
            payload_start,
        })
    }

    // =====================================================================
    // Parameter lists
    // =====================================================================

    /// Type-parameter-only groups used by type aliases, newtypes,
    /// host types, and label heads. Adjacent bracket binders (`[A][B]`)
    /// are canonical; a comma group (`[A, B]`) parses to the same
    /// ordered binder run and the formatter prints the canonical form.
    fn type_param_list(&mut self) -> Result<Vec<TypeParam>, Error> {
        let mut params = Vec::new();
        while self.peek_starts_type_param() {
            params.extend(self.type_param_group()?);
        }
        Ok(params)
    }

    fn type_param_group(&mut self) -> Result<Vec<TypeParam>, Error> {
        let group_start = self.peek_span().start;
        self.expect_type_param_open()?;
        self.skip_commas();
        if self.at_type_param_close() {
            let end = self.expect_type_param_close()?.end;
            return Err(self.err(
                Span::new(group_start, end),
                "type-parameter group cannot be empty",
            ));
        }
        let mut params = Vec::new();
        while !self.at_type_param_close() {
            let param_start = self.peek_span().start;
            let star_count = self.consume_leading_stars();
            if self.at_fragment_eof() {
                self.record_unclosed_structural_forall_at_eof();
            }
            let (name, name_span) = self.expect_binder()?;
            Self::validate_type_name(&name, name_span)?;
            self.record_source_name(name_span, SourceNameRole::Parameter);
            let kind = if star_count == 0 {
                None
            } else {
                Some(crate::ast::Kind::arrow_chain(star_count))
            };
            params.push(TypeParam {
                name,
                span: Span::new(param_start, name_span.end),
                kind,
            });
            let saw_comma = self.skip_commas();
            if self.at_type_param_close() {
                break;
            }
            if !saw_comma {
                if self.at_fragment_eof() {
                    self.record_unclosed_structural_forall_at_eof();
                }
                return Err(self.err_here("expected `,` or `]`"));
            }
        }
        let end = self.expect_type_param_close()?.end;
        if let [param] = params.as_mut_slice() {
            param.span = Span::new(group_start, end);
        }
        Ok(params)
    }

    /// Consume a leading run of `*` characters inside a type binder
    /// and return the count. The lexer greedily fuses adjacent op-
    /// chars, so `**` is one `SymbolRun("**")`; an identifier breaks
    /// the run, so `*f` is `SymbolRun("*")` then `IDENT(f)`. Returns
    /// 0 when the next token is not an all-stars run.
    fn consume_leading_stars(&mut self) -> usize {
        match self.peek() {
            Some(TokenKind::SymbolRun(run)) if !run.is_empty() && run.chars().all(|c| c == '*') => {
                let n = run.chars().count();
                self.advance();
                n
            }
            _ => 0,
        }
    }

    /// Returns true if the next operator run starts with the structural
    /// `[` required by a universal type binder. The lexer keeps brackets
    /// in ordinary maximal operator runs, so compact `[*F]` begins with
    /// `SymbolRun("[*")`; only this binder context peels the bracket.
    fn peek_starts_type_param(&self) -> bool {
        self.at_sym_prefix("[")
    }

    fn expect_type_param_open(&mut self) -> Result<Span, Error> {
        let span = match self.peek() {
            Some(TokenKind::SymbolRun(run)) if run == "[" => Ok(self.advance().span),
            Some(TokenKind::SymbolRun(run)) if run.starts_with('[') => {
                Ok(self.split_current_sym(1).span)
            }
            _ => Err(self.err_here("expected `[`")),
        }?;
        if let Some(tooling) = &mut self.tooling {
            tooling
                .facts
                .structural_forall
                .bracket_offsets
                .push(span.start);
        }
        Ok(span)
    }

    fn at_type_param_close(&self) -> bool {
        self.at_sym_prefix("]")
    }

    fn expect_type_param_close(&mut self) -> Result<Span, Error> {
        let span = match self.peek() {
            Some(TokenKind::SymbolRun(run)) if run == "]" => Ok(self.advance().span),
            Some(TokenKind::SymbolRun(run)) if run.starts_with(']') => {
                Ok(self.split_current_sym(1).span)
            }
            _ => Err(self.err_here("expected `]`")),
        }?;
        if let Some(tooling) = &mut self.tooling {
            tooling
                .facts
                .structural_forall
                .bracket_offsets
                .push(span.start);
        }
        Ok(span)
    }

    fn peek_starts_type_param_list(&self) -> bool {
        self.peek_starts_type_param()
    }

    fn peek_starts_signature_group(&self) -> bool {
        self.peek_starts_type_param() || matches!(self.peek(), Some(TokenKind::LParen))
    }

    fn signature(&mut self) -> Result<Signature, Error> {
        let groups = self.signature_groups(true)?;
        let sig = Signature::from_groups(groups);
        if sig.value_group_count() == 0 {
            return Err(self.err_here("callable signatures require an explicit value group `()`"));
        }
        Ok(sig)
    }

    fn fn_signature(&mut self) -> Result<Signature, Error> {
        let groups = self.signature_groups(false)?;
        let sig = Signature::from_groups(groups);
        if sig.value_group_count() == 0 {
            return Err(self.err_here("lambda signatures require an explicit value group `()`"));
        }
        Ok(sig)
    }

    fn signature_groups(&mut self, annotated: bool) -> Result<Vec<SignatureGroup>, Error> {
        let owner = self.peek_span();
        let mut groups = Vec::new();
        while self.peek_starts_signature_group() {
            if self.peek_starts_type_param() {
                groups.push(SignatureGroup::Type(self.type_param_group()?));
                continue;
            }
            let type_params = groups.iter().flat_map(|group| match group {
                SignatureGroup::Type(params) => params.as_slice(),
                SignatureGroup::Value(_) => &[],
            });
            let parsed = self.with_type_parameters(
                owner,
                ScopeOwnerKind::Signature,
                type_params,
                |parser| parser.parenthesized_signature_group(annotated),
            )?;
            groups.extend(parsed);
        }
        if groups.is_empty() {
            return Err(self.err_here("expected a signature parameter group"));
        }
        Ok(groups)
    }

    fn parenthesized_signature_group(
        &mut self,
        annotated: bool,
    ) -> Result<Vec<SignatureGroup>, Error> {
        self.expect_kind(TokenKind::LParen, "`(`")?;
        // Capture per-param leading trivia and the last param's
        // trailing run before `)`, the same way `call_arg_list` does,
        // so a comment above or trailing a value parameter survives a
        // `kio fmt` round-trip instead of being dropped.
        let mut pending_trivia = self.peek_leading_trivia();
        while matches!(self.peek(), Some(TokenKind::Comma)) {
            self.advance();
            pending_trivia.extend(self.peek_leading_trivia());
        }
        let mut params = Vec::new();
        while !matches!(self.peek(), Some(TokenKind::RParen)) {
            let leading = std::mem::take(&mut pending_trivia);
            let mut param = if annotated {
                self.value_param()?
            } else {
                self.fn_value_param()?
            };
            inject_meta_trivia(&mut param.meta, leading);
            params.push(param);
            let mut saw_comma = false;
            pending_trivia = self.peek_leading_trivia();
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                saw_comma = true;
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if matches!(self.peek(), Some(TokenKind::RParen)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `)`"));
            }
        }
        let closing_trivia = std::mem::take(&mut pending_trivia);
        if let Some(last) = params.last_mut() {
            set_trailing_trivia(&mut last.meta, closing_trivia);
        }
        self.expect_kind(TokenKind::RParen, "`)`")?;
        Ok(vec![SignatureGroup::Value(params)])
    }

    fn value_param(&mut self) -> Result<Param, Error> {
        // Bare top-level destructuring pattern: `(<pat-elems>)`.
        // At top-level param position `(` is unambiguously a pattern
        // — the existing param productions admit `[T]` (type param)
        // and `IDENT (: T)?` (value param) but no leading-`(` shape,
        // so a `(` here is always our new destructuring form.
        if matches!(self.peek(), Some(TokenKind::LParen)) {
            let pattern = self.param_pattern_tuple(true)?;
            let pat_span = pattern.span;
            self.reject_outer_pattern_annotation()?;
            let name = self.synth_pattern_param_name();
            return Ok(Param {
                name,
                ty: None,
                pattern: Some(pattern),
                meta: Meta::new(pat_span),
            });
        }
        let (name, name_span) = self.expect_binder()?;
        // `_` is the wildcard-discard parameter, admitted here; every
        // other name is validated (letterless names are rejected).
        if name != "_" {
            Self::validate_value_name(&name, name_span)?;
            self.record_source_name(name_span, SourceNameRole::Parameter);
        }
        self.expect_sym(":", "`:`")?;
        // As-pattern at top: `name : (<pat-elems>)`.
        if matches!(self.peek(), Some(TokenKind::LParen)) && self.peek_paren_starts_param_pattern()
        {
            let pattern = self.param_pattern_tuple(true)?;
            let pat_end = pattern.span.end;
            return Ok(Param {
                name,
                ty: None,
                pattern: Some(pattern),
                meta: Meta::new(Span::new(name_span.start, pat_end)),
            });
        }
        let ty = self.type_expr()?;
        let ty_span = ty.span();
        Ok(Param {
            name,
            ty: Some(ty),
            pattern: None,
            meta: Meta::new(Span::new(name_span.start, ty_span.end)),
        })
    }

    fn fn_value_param(&mut self) -> Result<Param, Error> {
        // Bare top-level destructuring pattern: `(<pat-elems>)`. At
        // top-level param position `(` is unambiguously a pattern
        // (see `value_param` for the rationale).
        if matches!(self.peek(), Some(TokenKind::LParen)) {
            let pattern = self.param_pattern_tuple(true)?;
            let pat_span = pattern.span;
            self.reject_outer_pattern_annotation()?;
            let name = self.synth_pattern_param_name();
            return Ok(Param {
                name,
                ty: None,
                pattern: Some(pattern),
                meta: Meta::new(pat_span),
            });
        }
        let (name, name_span) = self.expect_binder()?;
        // `_` is the wildcard-discard parameter, admitted here; every
        // other name is validated (letterless names are rejected).
        if name != "_" {
            Self::validate_value_name(&name, name_span)?;
            self.record_source_name(name_span, SourceNameRole::Parameter);
        }
        // Optional `: T` annotation, or as-pattern `: (...)`.
        // `_` parses through `type_expr` as `Type::Infer` and is
        // treated identically to no annotation (the typer recovers
        // the parameter's type from the call-site context).
        if self.at_sym(":") {
            self.advance();
            if matches!(self.peek(), Some(TokenKind::LParen))
                && self.peek_paren_starts_param_pattern()
            {
                let pattern = self.param_pattern_tuple(true)?;
                let pat_end = pattern.span.end;
                return Ok(Param {
                    name,
                    ty: None,
                    pattern: Some(pattern),
                    meta: Meta::new(Span::new(name_span.start, pat_end)),
                });
            }
            let ty = self.type_expr()?;
            Ok(Param {
                name,
                ty: Some(ty),
                pattern: None,
                meta: Meta::new(name_span),
            })
        } else {
            Ok(Param {
                name,
                ty: None,
                pattern: None,
                meta: Meta::new(name_span),
            })
        }
    }

    /// True iff the current `(` opens a [`ParamPattern`] at an
    /// after-colon disambiguation site (`name : <here>`, where the
    /// RHS is either a `Type` or a `ParamPatternTuple`). Returns
    /// true only when the inner shape unambiguously distinguishes
    /// pattern from type — specifically when at least one slot
    /// among the elements at the outer level carries an `IDENT :` /
    /// `_ :` annotation (a type-position paren-group never has
    /// `IDENT :` at top level, since type expressions don't take
    /// the `name : Type` shape). Bare-name slots like `(a, b)` look
    /// indistinguishable from a function-type param list at this
    /// position, so they default to type parsing — the user can
    /// pin a slot with `(a: _, b: _)` to force the pattern reading.
    ///
    /// Caller is responsible for verifying the cursor is at `(`
    /// (we read from offset 1).
    fn peek_paren_starts_param_pattern(&self) -> bool {
        // Skip a leading nested-paren chain; the disambiguation
        // happens at the first non-paren token.
        let mut offset = 1;
        while matches!(self.peek_at(offset), Some(TokenKind::LParen)) {
            offset += 1;
        }
        // Walk a balanced slice of the outer paren until we either
        // close it or hit a `:` (pattern indicator). We don't try to
        // recover from imbalanced parens — the outer parser will
        // surface that as a parse error once it reaches the same
        // position.
        let mut depth: i32 = 0;
        loop {
            match self.peek_at(offset) {
                Some(TokenKind::LParen) => depth += 1,
                Some(TokenKind::RParen) => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                Some(t) if depth == 0 && t == TokenKind::sym(":") => return true,
                Some(_) => {}
                None => return false,
            }
            offset += 1;
        }
    }

    /// Reject the banned `(<pat-elems>): T` form — an outer type
    /// annotation pinned onto a bare destructuring pattern. The
    /// pattern's slot types already constitute the product type, so
    /// the outer annotation is redundant.
    fn reject_outer_pattern_annotation(&mut self) -> Result<(), Error> {
        if self.at_sym(":") {
            return Err(self.err_here(
                "outer type annotation on a parameter pattern is redundant — \
                 the pattern's slot types already define the product type; \
                 to give the whole product a name, write the as-pattern form \
                 `name: (...)` instead",
            ));
        }
        Ok(())
    }

    /// Mint a compiler-reserved outer name for a bare destructuring
    /// pattern (`(a: A, b: B)` with no user-given name). Desugaring keeps it on
    /// the flattened outer binding; the Lowered → Prime substitution boundary
    /// materializes it into a collision-allocated name in the ordinary Kio
    /// value namespace.
    fn synth_pattern_param_name(&mut self) -> String {
        let id = self.fresh_node_id();
        format!("__pat_param_n{}__", id.0)
    }

    fn synth_row_let_temp_name(&mut self) -> String {
        let id = self.fresh_node_id();
        format!("__row_let_n{}__", id.0)
    }

    /// Parse `'(' ParamPatternElem (',' ParamPatternElem)* ','? ')'`.
    /// Caller must have verified `peek() == LParen` and that the
    /// content of the parens is a pattern (via
    /// [`Self::peek_paren_starts_param_pattern`]).
    fn param_pattern_tuple(&mut self, parameter_owner: bool) -> Result<ParamPattern, Error> {
        let start = self.expect_kind(TokenKind::LParen, "`(`")?.start;
        self.skip_commas();
        let mut elems: Vec<ParamPatternElem> = Vec::new();
        while !matches!(self.peek(), Some(TokenKind::RParen)) {
            elems.push(self.param_pattern_elem(parameter_owner)?);
            let saw_comma = self.skip_commas();
            if matches!(self.peek(), Some(TokenKind::RParen)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `)`"));
            }
        }
        let end = self.expect_kind(TokenKind::RParen, "`)`")?.end;
        let span = Span::new(start, end);
        if elems.is_empty() {
            return Err(self.err(
                span,
                "parameter pattern `(...)` must have at least one slot",
            ));
        }
        let match_id = self.fresh_node_id();
        Ok(ParamPattern {
            elems,
            span,
            match_id,
        })
    }

    /// Parse one `ParamPatternElem`: `name: T`, `name`, `_: T`, `_`,
    /// `(...)`, or `name: (...)`. Bare `name` and bare `_` carry an
    /// implicit `Type::Infer` (`_`) annotation — admissible at slot
    /// positions where the surrounding context pins the type (a
    /// lambda literal, a `match!` clause). For top-level fn definitions
    /// the typer rejects `_` slots that no context resolves.
    fn param_pattern_elem(&mut self, parameter_owner: bool) -> Result<ParamPatternElem, Error> {
        // Nested bare tuple: `(...)`. We commit here because the
        // outer caller has already verified we're inside a pattern.
        if matches!(self.peek(), Some(TokenKind::LParen)) {
            let pattern = self.param_pattern_tuple(parameter_owner)?;
            // A nested bare tuple can also carry an outer name if it
            // were really an `IDENT : (...)` shape — but we already
            // know there's no leading `IDENT` here, so reject any
            // trailing `: T` (which would be the banned outer
            // annotation form at this slot).
            self.reject_outer_pattern_annotation()?;
            return Ok(ParamPatternElem::Tuple(pattern));
        }
        // `name: T` or `_: T` or `name: (...)`, or the bare-name
        // shapes `name` / `_`. `Slot1` (`_`) is admitted as the
        // wildcard form; otherwise we validate as an ordinary
        // value-name.
        let (name, name_span) = self.expect_binder()?;
        if name != "_" {
            Self::validate_value_name(&name, name_span)?;
            if parameter_owner {
                self.record_source_name(name_span, SourceNameRole::Parameter);
            }
        }
        // Bare slot: `name` or `_` with no `:`. The slot's type
        // becomes `Type::Infer` — the typer pulls it from the
        // surrounding context (a lambda literal's expected type, a
        // `match!` clause's dispatch pattern). For top-level fn defs
        // the typer rejects an unresolvable `_`.
        if !self.at_sym(":") {
            return Ok(ParamPatternElem::Bind {
                name,
                name_span,
                ty: Type::Infer {
                    meta: Meta::new(name_span),
                    ext: (),
                },
            });
        }
        self.expect_sym(":", "`:`")?;
        // Nested as-pattern: `name : (<pat-elems>)`. Disambiguate from
        // a parenthesized type by peeking inside.
        if matches!(self.peek(), Some(TokenKind::LParen)) && self.peek_paren_starts_param_pattern()
        {
            let inner = self.param_pattern_tuple(parameter_owner)?;
            if name == "_" {
                return Err(self.err(
                    name_span,
                    "`_: (...)` is not admissible — the wildcard discards its slot, so naming the nested product is pointless; use `(...)` for the bare nested pattern instead",
                ));
            }
            Ok(ParamPatternElem::BindTuple {
                name,
                name_span,
                inner,
            })
        } else {
            let ty = self.type_expr()?;
            Ok(ParamPatternElem::Bind {
                name,
                name_span,
                ty,
            })
        }
    }

    fn unary_bind_from_pattern(pat: &ParamPattern) -> Option<(String, Span, Option<Type>)> {
        let [
            ParamPatternElem::Bind {
                name,
                name_span,
                ty,
            },
        ] = pat.elems.as_slice()
        else {
            return None;
        };
        let ty = if matches!(ty, Type::Infer { .. }) && ty.span() == *name_span {
            None
        } else {
            Some(ty.clone())
        };
        Some((name.clone(), *name_span, ty))
    }

    // =====================================================================
    // Type expressions
    // =====================================================================

    pub(super) fn type_expr(&mut self) -> Result<Type, Error> {
        self.cursor_choices(CursorSlot::Type, &[]);
        if self.peek_starts_type_param() {
            return self.type_prefix_signature();
        }
        let (lhs, lhs_is_chain) = self.type_chain_expr()?;
        if self.at_sym_prefix("->") {
            if lhs_is_chain {
                return Err(self.err_here(
                    "product and sum types on the left of `->` must be parenthesized; \
                     write `(A & B) -> R` or `(A | B) -> R`",
                ));
            }
            self.expect_fn_arrow()?;
            let ret = self.type_expr()?;
            let ret_end = ret.span().end;
            let span = Span::new(lhs.span().start, ret_end);
            return Ok(Type::synth_function(vec![lhs], ret, span));
        }
        Ok(lhs)
    }

    fn type_arrow_expr(&mut self) -> Result<Type, Error> {
        let lhs = self.type_atom()?;
        if self.at_sym_prefix("->") {
            self.expect_fn_arrow()?;
            let ret = self.type_expr()?;
            let ret_end = ret.span().end;
            let span = Span::new(lhs.span().start, ret_end);
            return Ok(Type::synth_function(vec![lhs], ret, span));
        }
        Ok(lhs)
    }

    /// Parse a type chain expression, including bare same-operator
    /// chains (`A & B`, `A | B | C`) at the top of the production.
    /// Mixing `&` and `|` in one chain still requires explicit
    /// parentheses. A leading-operator run (`& A & B`) is also
    /// accepted — that's the multi-line layout the formatter emits,
    /// mirroring the leading-comma rule for lists.
    ///
    /// Bare same-operator chains are admissible in Kio' as well as
    /// at the surface — they're parse-time associativity sugar that
    /// folds to the same right-associated `Type::Product` /
    /// `Type::Sum` AST as the parenthesized form, with no later
    /// phase able to distinguish the two shapes.
    fn type_chain_expr(&mut self) -> Result<(Type, bool), Error> {
        // Optional leading-operator run (the multi-line layout).
        let (leading_kind, leading_span) = match self.peek() {
            Some(t) if is_amp_chain_sep(t) => {
                let start = self.peek_span().start;
                let end = self.skip_amp_chain_ops().unwrap_or(start);
                (Some(ChainKind::Amp), Some(Span::new(start, end)))
            }
            Some(t) if is_pipe_chain_sep(t) => {
                let start = self.peek_span().start;
                let end = self.skip_pipe_chain_ops().unwrap_or(start);
                (Some(ChainKind::Pipe), Some(Span::new(start, end)))
            }
            _ => (None, None),
        };

        if let Some(kind) = leading_kind
            && !self.peek_starts_type_atom()
        {
            if matches!(self.peek(), Some(t) if match kind {
                ChainKind::Amp => is_pipe_chain_sep(t),
                ChainKind::Pipe => is_amp_chain_sep(t),
            }) {
                return Err(self.err_here(
                    "mixing `&` and `|` requires explicit parentheses; \
                     e.g., `A & (B | C)` or `(A & B) | C`",
                ));
            }
            return Ok((
                match kind {
                    ChainKind::Amp => Type::Unit {
                        meta: Meta::new(leading_span.expect("leading product chain has a span")),
                    },
                    ChainKind::Pipe => Type::Bottom {
                        meta: Meta::new(leading_span.expect("leading sum chain has a span")),
                    },
                },
                true,
            ));
        }
        let first = self.type_arrow_expr()?;

        // Determine chain operator: from leading run if seen, else
        // from the next token. No chain → return atom.
        let chain_kind = match leading_kind {
            Some(k) => k,
            None => match self.peek() {
                Some(t) if is_amp_chain_sep(t) => ChainKind::Amp,
                Some(t) if is_pipe_chain_sep(t) => ChainKind::Pipe,
                _ => return Ok((first, false)),
            },
        };

        let span_start = leading_span
            .map(|span| span.start)
            .unwrap_or_else(|| first.span().start);
        let mut chain = vec![first];

        loop {
            let next = self.peek();
            let is_match = match chain_kind {
                ChainKind::Amp => next.is_some_and(is_amp_chain_sep),
                ChainKind::Pipe => next.is_some_and(is_pipe_chain_sep),
            };
            let is_other = match chain_kind {
                ChainKind::Amp => next.is_some_and(is_pipe_chain_sep),
                ChainKind::Pipe => next.is_some_and(is_amp_chain_sep),
            };
            if is_match {
                match chain_kind {
                    ChainKind::Amp => self.skip_amp_chain_ops(),
                    ChainKind::Pipe => self.skip_pipe_chain_ops(),
                };
                if !self.peek_starts_type_atom() {
                    break;
                }
                chain.push(self.type_atom()?);
            } else if is_other {
                return Err(self.err_here(
                    "mixing `&` and `|` requires explicit parentheses; \
                     e.g., `A & (B | C)` or `(A & B) | C`",
                ));
            } else {
                break;
            }
        }

        let last_end = chain.last().unwrap().span().end;
        let span = Span::new(span_start, last_end);
        Ok((
            match chain_kind {
                ChainKind::Amp => fold_product_chain(chain, span),
                ChainKind::Pipe => fold_sum_chain(chain, span),
            },
            true,
        ))
    }

    fn skip_amp_chain_ops(&mut self) -> Option<u32> {
        let mut end = None;
        while self.peek().is_some_and(is_amp_chain_sep) {
            let tok = self.peek().cloned();
            end = Some(match tok {
                Some(TokenKind::SymbolRun(run)) => {
                    let len =
                        chain_sep_prefix_len(&run, '&').expect("caller checked product chain sep");
                    if len == run.len() {
                        self.advance().span.end
                    } else {
                        self.split_current_sym(len).span.end
                    }
                }
                _ => unreachable!("caller checked product chain sep"),
            });
        }
        end
    }

    fn skip_pipe_chain_ops(&mut self) -> Option<u32> {
        let mut end = None;
        while self.peek().is_some_and(is_pipe_chain_sep) {
            let tok = self.peek().cloned();
            end = Some(match tok {
                Some(TokenKind::SymbolRun(run)) => {
                    let len =
                        chain_sep_prefix_len(&run, '|').expect("caller checked sum chain sep");
                    if len == run.len() {
                        self.advance().span.end
                    } else {
                        self.split_current_sym(len).span.end
                    }
                }
                _ => unreachable!("caller checked sum chain sep"),
            });
        }
        end
    }

    fn at_unit_type_dot(&self) -> bool {
        matches!(
            self.peek(),
            Some(TokenKind::SymbolRun(run))
                if run == "."
                    || run.starts_with(".->")
                    || run.starts_with(".&")
                    || run.starts_with(".|")
        )
    }

    /// True iff the next token can begin a `type_atom`. Used by
    /// the chain loop to recognize a trailing-operator run that
    /// absorbs into the chain's terminator (mirrors the inside-
    /// paren rule from `language.md` § Comma-separated lists and
    /// operator chains).
    fn peek_starts_type_atom(&self) -> bool {
        self.at_unit_type_dot()
            || self.at_sym("!")
            || self.peek_starts_type_param()
            || matches!(
                self.peek(),
                Some(TokenKind::Ident(_))
                    | Some(TokenKind::Slot1)
                    | Some(TokenKind::LParen)
                    | Some(TokenKind::LBrace)
            )
    }

    /// Single-atom type production: bottom (`!`), `_`, path /
    /// parenthesized / rejected braced-label type forms. Used as the chain element for the
    /// chain-aware `type_expr`, and directly inside `type_paren`'s
    /// explicit-paren chain logic where
    /// the mixing rule must be enforced at each chain step.
    fn type_atom(&mut self) -> Result<Type, Error> {
        match self.peek() {
            Some(_) if self.at_unit_type_dot() => {
                let tok = match self.peek() {
                    Some(TokenKind::SymbolRun(run)) if run == "." => self.advance(),
                    Some(TokenKind::SymbolRun(_)) => self.split_current_sym(1),
                    _ => unreachable!("caller checked a type-level dot"),
                };
                Ok(Type::Unit {
                    meta: Meta::new(tok.span),
                })
            }
            Some(k) if k.is_sym("!") => {
                let span = self.peek_span();
                self.advance();
                Ok(Type::Bottom {
                    meta: Meta::new(span),
                })
            }
            // `_` placeholder: a position the typer should infer.
            // Surface-only — kio-prime rejects via `prime::lower::lower_type`.
            // The placeholder has no children; `_(args)` and dotted
            // `_.foo` / `m._` forms are rejected naturally because
            // the next token after consuming `_` is `(`/`.` and the
            // surrounding parse context fails on that.
            Some(TokenKind::Slot1) => {
                let span = self.peek_span();
                self.advance();
                Ok(Type::Infer {
                    meta: Meta::new(span),
                    ext: (),
                })
            }
            Some(TokenKind::Ident(_)) => self.type_path(),
            Some(_) if self.peek_starts_type_param() => self.type_prefix_signature(),
            Some(TokenKind::LParen) => self.type_paren(),
            Some(TokenKind::LBrace) => Err(self.err_here(
                "labels are value syntax; use the generated nominal type name in type position",
            )),
            _ => Err(self.err_here("expected type expression")),
        }
    }

    fn type_path(&mut self) -> Result<Type, Error> {
        // A qualified type reference is `module/path.TypeName`: the
        // module segments are `/`-separated and the final type name
        // is reached with `.`. A bare `TypeName` is a local type.
        let first_span = self.peek_span();
        let segments = self.type_path_segments()?;
        for qualifier in segments.iter().take(segments.len().saturating_sub(1)) {
            Self::validate_value_name(&qualifier.name, qualifier.span)?;
        }
        if let Some(last) = segments.last() {
            Self::validate_type_path_name(&last.name, last.span)?;
            self.record_source_name(last.span, SourceNameRole::Type);
        }
        let span_end = segments
            .last()
            .map(|s| s.span.end)
            .unwrap_or(first_span.end);
        let (args, end) = if matches!(self.peek(), Some(TokenKind::LParen)) {
            self.type_arg_list()?
        } else {
            (Vec::new(), span_end)
        };
        Ok(Type::Path {
            segments,
            args,
            meta: Meta::new(Span::new(first_span.start, end)),
        })
    }

    /// `(T, T, ...)` — comma-separated type arguments. Each captured
    /// per-arg leading-trivia run is injected onto that arg's
    /// `meta.leading_trivia` (so the pretty-printer reads it from
    /// the inner node). Returns the argument list and the end
    /// byte-offset of the closing paren.
    fn type_arg_list(&mut self) -> Result<(Vec<Type>, u32), Error> {
        self.expect_kind(TokenKind::LParen, "`(`")?;
        let mut args = Vec::new();
        let mut pending_trivia = self.peek_leading_trivia();
        while matches!(self.peek(), Some(TokenKind::Comma)) {
            self.advance();
            pending_trivia.extend(self.peek_leading_trivia());
        }
        while !matches!(self.peek(), Some(TokenKind::RParen)) {
            let leading = std::mem::take(&mut pending_trivia);
            let mut arg = self.type_expr()?;
            inject_meta_trivia(arg.meta_mut(), leading);
            args.push(arg);
            pending_trivia = self.peek_leading_trivia();
            let mut saw_comma = false;
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                saw_comma = true;
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if matches!(self.peek(), Some(TokenKind::RParen)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `)`"));
            }
        }
        // Last type-arg's trailing / dangling comment run before the
        // `)` — stash on the last arg's trailing slot so the formatter
        // keeps it across the round-trip.
        let closing_trivia = std::mem::take(&mut pending_trivia);
        if let Some(last) = args.last_mut() {
            set_trailing_trivia(last.meta_mut(), closing_trivia);
        }
        let end = self.expect_kind(TokenKind::RParen, "`)`")?.end;
        Ok((args, end))
    }

    fn type_prefix_signature(&mut self) -> Result<Type, Error> {
        let owner = self.peek_span();
        let start = owner.start;
        let mut params = Vec::new();
        while self.peek_starts_type_param() {
            params.extend(self.type_param_group()?);
        }
        if self.at_sym_prefix("->") {
            return Err(self.err_here("type-binder run must be followed by a body type"));
        }
        let body =
            self.with_type_parameters(owner, ScopeOwnerKind::Type, &params, Self::type_expr)?;
        let body_end = body.span().end;
        let span = Span::new(start, body_end);
        let mut acc = body;
        for param in params.into_iter().rev() {
            acc = Type::Forall {
                param,
                body: Box::new(acc),
                meta: Meta::new(span),
            }
        }
        Ok(acc)
    }

    /// Collect an `&`- or `|`-chained type list inside outer parens —
    /// e.g., `(A & B & C)` builds `[A, B, C]`. The first element is
    /// passed in (already parsed), and the matching operator is given
    /// in `op_kind`. Mixing `&` and `|` in one chain is a parse error
    /// (per spec: the two operators bind at different precedences;
    /// users wanting both must explicitly parenthesize).
    ///
    /// The chain accepts any number of `op_kind` tokens between items
    /// and a trailing run before `)` — these all collapse to the same
    /// AST as the canonical one-operator-per-separator layout. The
    /// formatter relies on this to emit the leading-operator multi-
    /// line style (`(\n  & A\n  & B\n  )`) without distinguishing it
    /// from the inline shape at the AST level.
    fn collect_type_chain(&mut self, first: Type, kind: ChainKind) -> Result<Vec<Type>, Error> {
        let mut chain = vec![first];
        // Consume the first operator (and any immediate repeats).
        debug_assert!(self.peek().is_some_and(match kind {
            ChainKind::Amp => is_amp_chain_sep,
            ChainKind::Pipe => is_pipe_chain_sep,
        }));
        match kind {
            ChainKind::Amp => self.skip_amp_chain_ops(),
            ChainKind::Pipe => self.skip_pipe_chain_ops(),
        };
        // The chain has at least two items: `first` plus what follows
        // the first operator. Accept any number of chain-op tokens
        // (each all-`&` / all-`|` `SymbolRun` counts as one
        // separator) in any position between items; trailing runs
        // absorb into the closer.
        //
        // Each chain element is parsed as a single atom (not a
        // bare chain) so the mixing rule fires immediately on
        // `(A & B | C)` rather than letting the inner chain
        // swallow `B | C` as a sum.
        loop {
            // We've just consumed one or more chain ops. If we've
            // landed on the closer, the trailing run terminates.
            if matches!(self.peek(), Some(TokenKind::RParen)) {
                break;
            }
            chain.push(self.type_atom()?);
            // After an item, expect one or more chain-op tokens (or
            // the closer). A different chain operator is the
            // explicit-paren mixing rule.
            let next = self.peek();
            let is_match = match kind {
                ChainKind::Amp => next.is_some_and(is_amp_chain_sep),
                ChainKind::Pipe => next.is_some_and(is_pipe_chain_sep),
            };
            let is_other = match kind {
                ChainKind::Amp => next.is_some_and(is_pipe_chain_sep),
                ChainKind::Pipe => next.is_some_and(is_amp_chain_sep),
            };
            if is_match {
                match kind {
                    ChainKind::Amp => self.skip_amp_chain_ops(),
                    ChainKind::Pipe => self.skip_pipe_chain_ops(),
                };
            } else if is_other {
                return Err(self.err_here(
                    "mixing `&` and `|` requires explicit parentheses; \
                     e.g., `(A & (B | C))` or `((A & B) | C)`",
                ));
            } else {
                break;
            }
        }
        Ok(chain)
    }

    /// Handles a type expression that opens with `(`. Possibilities:
    /// - `(T)` → parenthesized type, or `T -> R` function type with domain `T`.
    /// - `(T & U [& V …])` → right-folded product chain.
    /// - `(T | U [| V …])` → right-folded sum chain.
    fn type_paren(&mut self) -> Result<Type, Error> {
        let group = self.cursor.remaining_current_frame().first();
        self.type_paren_inner().map_err(|error| {
            group
                .and_then(|group| value_shaped_type_error(group, &error))
                .unwrap_or(error)
        })
    }

    fn type_paren_inner(&mut self) -> Result<Type, Error> {
        let start = self.peek_span().start;
        self.expect_kind(TokenKind::LParen, "`(`")?;

        // Look for a leading operator that signals which chain shape
        // we're in. A leading `&` / `|` means a product / sum chain in
        // the formatter's multi-line layout (`(\n  & A\n  & B\n  )`).
        // Each leading token is consumed plus any immediate repeats.
        let leading_chain_op: Option<LeadingOp> = match self.peek() {
            Some(t) if is_amp_chain_sep(t) => {
                self.skip_amp_chain_ops();
                Some(LeadingOp::Chain(ChainKind::Amp))
            }
            Some(t) if is_pipe_chain_sep(t) => {
                self.skip_pipe_chain_ops();
                Some(LeadingOp::Chain(ChainKind::Pipe))
            }
            Some(TokenKind::Comma) => {
                return Err(tuple_type_error(self.peek_span()));
            }
            _ => None,
        };

        // Empty product and sum chains use their identity types.
        // Without a leading chain operator, parentheses are only
        // grouping and must contain a type.
        if matches!(self.peek(), Some(TokenKind::RParen)) {
            let close_end = self.peek_span().end;
            self.advance();
            return match leading_chain_op {
                Some(LeadingOp::Chain(ChainKind::Amp)) => Ok(Type::Unit {
                    meta: Meta::new(Span::new(start, close_end)),
                }),
                Some(LeadingOp::Chain(ChainKind::Pipe)) => Ok(Type::Bottom {
                    meta: Meta::new(Span::new(start, close_end)),
                }),
                None => Err(self.err(
                    Span::new(start, close_end),
                    "expected type expression inside parentheses",
                )),
            };
        }

        if let Some(LeadingOp::Chain(kind)) = leading_chain_op
            && matches!(self.peek(), Some(t) if match kind {
                ChainKind::Amp => is_pipe_chain_sep(t),
                ChainKind::Pipe => is_amp_chain_sep(t),
            })
        {
            return Err(self.err_here(
                "mixing `&` and `|` requires explicit parentheses; \
                 e.g., `(A & (B | C))` or `((A & B) | C)`",
            ));
        }

        if let Some(LeadingOp::Chain(kind)) = leading_chain_op
            && !self.peek_starts_type_atom()
        {
            if matches!(self.peek(), Some(TokenKind::Comma)) {
                return Err(tuple_type_error(self.peek_span()));
            }
            return match kind {
                ChainKind::Amp => Ok(Type::Unit {
                    meta: Meta::new(Span::new(start, self.peek_span().start)),
                }),
                ChainKind::Pipe => Ok(Type::Bottom {
                    meta: Meta::new(Span::new(start, self.peek_span().start)),
                }),
            };
        }

        // Standalone existential type expressions `(<U>, body)` are no
        // longer admissible: existentials are a `newtype`-only feature
        // (declared as a trailing `<U>` binder run on the newtype's
        // header) and consumed via the newtype's CPS projector — see
        // `specs/language.md` § Existential type binders.
        if self.at_sym("<") {
            return Err(self.err_here(
                "existential type expressions are no longer admissible at this position — \
                 existentials are declared on a `newtype` header (`newtype Name[A] <U> : body`) \
                 and consumed via the newtype's CPS projector. \
                 See specs/language.md § Existential type binders.",
            ));
        }

        // Leading `&` / `|` layouts stay in the explicit chain
        // path below. Their elements are atoms, so a function-typed
        // element must be parenthesized. Otherwise a parenthesized
        // item is a full type expression, so `(A -> B)` works naturally.
        let first = if matches!(leading_chain_op, Some(LeadingOp::Chain(_))) {
            self.type_atom()?
        } else {
            self.type_expr()?
        };

        // Branch on the leading operator if we saw one — it pins the
        // chain shape. Otherwise dispatch on what follows `first`.
        if let Some(op) = leading_chain_op {
            return match op {
                LeadingOp::Chain(ChainKind::Amp) => {
                    let chain = self.collect_type_chain_after_first(first, ChainKind::Amp)?;
                    let end = self.expect_kind(TokenKind::RParen, "`)`")?.end;
                    Ok(fold_product_chain(chain, Span::new(start, end)))
                }
                LeadingOp::Chain(ChainKind::Pipe) => {
                    let chain = self.collect_type_chain_after_first(first, ChainKind::Pipe)?;
                    let end = self.expect_kind(TokenKind::RParen, "`)`")?.end;
                    Ok(fold_sum_chain(chain, Span::new(start, end)))
                }
            };
        }

        match self.peek() {
            Some(t) if is_amp_chain_sep(t) => {
                let chain = self.collect_type_chain(first, ChainKind::Amp)?;
                self.finish_paren_chain_or_fn_param(start, chain, ChainKind::Amp)
            }
            Some(t) if is_pipe_chain_sep(t) => {
                let chain = self.collect_type_chain(first, ChainKind::Pipe)?;
                self.finish_paren_chain_or_fn_param(start, chain, ChainKind::Pipe)
            }
            Some(TokenKind::Comma) => Err(tuple_type_error(self.peek_span())),
            Some(TokenKind::RParen) => {
                self.advance();
                if self.at_sym_prefix("->") {
                    self.expect_fn_arrow()?;
                    let ret = self.type_expr()?;
                    let ret_end = ret.span().end;
                    Ok(Type::synth_function(
                        vec![first],
                        ret,
                        Span::new(start, ret_end),
                    ))
                } else {
                    Ok(first)
                }
            }
            _ => Err(self.err_here("expected `&`, `|`, or `)` in type expression")),
        }
    }

    /// After collecting a chain inside `( … )` (no leading op),
    /// dispatch on what follows the chain:
    ///
    /// - `)` — the chain is the parenthesized type expression. May
    ///   be followed by `-> R` (single-param function type whose
    ///   sole parameter is the chain).
    fn finish_paren_chain_or_fn_param(
        &mut self,
        outer_start: u32,
        chain: Vec<Type>,
        kind: ChainKind,
    ) -> Result<Type, Error> {
        match self.peek() {
            Some(TokenKind::RParen) => {
                let end = self.peek_span().end;
                self.advance();
                let folded = match kind {
                    ChainKind::Amp => fold_product_chain(chain, Span::new(outer_start, end)),
                    ChainKind::Pipe => fold_sum_chain(chain, Span::new(outer_start, end)),
                };
                if self.at_sym_prefix("->") {
                    self.expect_fn_arrow()?;
                    let ret = self.type_expr()?;
                    let ret_end = ret.span().end;
                    Ok(Type::synth_function(
                        vec![folded],
                        ret,
                        Span::new(outer_start, ret_end),
                    ))
                } else {
                    Ok(folded)
                }
            }
            Some(TokenKind::Comma) => Err(tuple_type_error(self.peek_span())),
            _ => Err(self.err_here("expected `)` after type chain")),
        }
    }

    /// Parse a single existential type-binder `<A>`. Validates the name follows
    /// type-name conventions (optional single underscore, then uppercase).
    /// Existential binders are kind-`*` only (the typer rejects a
    /// higher-kind existential), so no star annotation is accepted
    /// here and the kind field is always `None`.
    fn angle_binder(&mut self) -> Result<TypeParam, Error> {
        let start = self.peek_span().start;
        self.expect_sym("<", "`<`")?;
        let (name, name_span) = self.expect_binder()?;
        Self::validate_type_name(&name, name_span)?;
        self.record_source_name(name_span, SourceNameRole::Parameter);
        let end = self.expect_existential_close()?.end;
        Ok(TypeParam {
            name,
            span: Span::new(start, end),
            kind: None,
        })
    }

    /// Variant of `collect_type_chain` for the leading-operator entry
    /// path: the leading run was already consumed, so we don't expect
    /// a `op_kind` immediately. The first element has already been
    /// parsed; we accept any number of `op_kind` tokens between items
    /// and a trailing run before `)`.
    fn collect_type_chain_after_first(
        &mut self,
        first: Type,
        kind: ChainKind,
    ) -> Result<Vec<Type>, Error> {
        let mut chain = vec![first];
        loop {
            // After an item: zero or more chain ops, then either an
            // item or the closer.
            let next = self.peek();
            let is_match = match kind {
                ChainKind::Amp => next.is_some_and(is_amp_chain_sep),
                ChainKind::Pipe => next.is_some_and(is_pipe_chain_sep),
            };
            let is_other = match kind {
                ChainKind::Amp => next.is_some_and(is_pipe_chain_sep),
                ChainKind::Pipe => next.is_some_and(is_amp_chain_sep),
            };
            if is_match {
                match kind {
                    ChainKind::Amp => self.skip_amp_chain_ops(),
                    ChainKind::Pipe => self.skip_pipe_chain_ops(),
                };
            } else if matches!(next, Some(TokenKind::RParen)) {
                break;
            } else if is_other {
                return Err(self.err_here(
                    "mixing `&` and `|` requires explicit parentheses; \
                     e.g., `(A & (B | C))` or `((A & B) | C)`",
                ));
            } else {
                let label = match kind {
                    ChainKind::Amp => "&",
                    ChainKind::Pipe => "|",
                };
                return Err(self.err_here(format!("expected `{label}` or `)` in type chain")));
            }
            if matches!(self.peek(), Some(TokenKind::RParen)) {
                break;
            }
            chain.push(self.type_atom()?);
        }
        Ok(chain)
    }

    // =====================================================================
    // Value expressions
    // =====================================================================

    pub(super) fn expr(&mut self) -> Result<Expr, Error> {
        let lhs = self.expr_postfix()?;
        let mut lhs = self.try_operator_continuation(lhs)?;
        // A left-greedy operator (`___ OP _`, `___ OP`, ...)
        // attaches to *any* preceding expression — even one that
        // already settled into a chain through a different operator.
        // Iterate so chained left-greedy ops compose naturally
        // (`a + b $> f $> g`).
        while self.operator_scope.next_is_left_greedy(self) {
            lhs = self.try_operator_continuation(lhs)?;
        }
        Ok(lhs)
    }

    #[cfg(any(test, feature = "repl"))]
    pub(super) fn enable_structural_forall_facts(&mut self) {
        self.tooling = Some(ParserTooling::default());
    }

    #[cfg(any(test, feature = "cli"))]
    pub(super) fn enable_tooling(&mut self, cursor: Option<u32>) {
        self.tooling = Some(ParserTooling {
            cursor,
            cursor_limit: self.eof,
            collect_roles: true,
            ..ParserTooling::default()
        });
    }

    #[cfg(any(test, feature = "cli"))]
    pub(super) fn set_cursor_suppression(&mut self, suppression: Option<CursorSuppression>) {
        self.tooling
            .as_mut()
            .expect("tooling enabled")
            .facts
            .suppression = suppression;
    }

    #[cfg(any(test, feature = "cli"))]
    pub(super) fn take_tooling_facts(&mut self) -> ParserFacts {
        self.tooling.take().expect("tooling enabled").facts
    }

    fn record_keyword(&mut self, span: Span, role: KeywordRole) {
        if span.start != span.end
            && let Some(tooling) = &mut self.tooling
            && tooling.collect_roles
        {
            tooling.facts.keywords.push(KeywordFact { span, role });
        }
    }

    fn record_source_name(&mut self, span: Span, role: SourceNameRole) {
        if let Some(tooling) = &mut self.tooling
            && tooling.collect_roles
            && span.start != span.end
        {
            tooling
                .facts
                .source_names
                .push(SourceNameFact { span, role });
        }
    }

    fn with_type_parameters<'p, T>(
        &mut self,
        owner: Span,
        owner_kind: ScopeOwnerKind,
        params: impl IntoIterator<Item = &'p TypeParam>,
        parse: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let start = self
            .tooling
            .as_ref()
            .map_or(owner.end, |tooling| tooling.last_end);
        let prefix_index = self
            .tooling
            .as_ref()
            .map_or(0, |tooling| tooling.facts.scope_prefix.len());
        let result = parse(self);
        let end = self.tooling.as_ref().map_or(start, |tooling| {
            if result.is_ok() {
                tooling.last_end
            } else {
                self.tooling_cursor_end(tooling.cursor_limit)
            }
        });
        if let Some(tooling) = &mut self.tooling
            && let Some(cursor) = tooling.cursor
            && start <= cursor
            && cursor <= end
        {
            let params: Vec<_> = params.into_iter().cloned().collect();
            if !params.is_empty() {
                tooling.facts.scope_prefix.insert(
                    prefix_index,
                    ScopePrefix {
                        owner,
                        owner_kind,
                        interval: Span::new(start, end),
                        syntax: ScopeSyntax::TypeParameters(params),
                    },
                );
            }
        }
        result
    }

    fn suppress_current_cursor(&mut self, kind: CursorSuppressionKind) {
        let Some(tooling) = &self.tooling else { return };
        let Some(cursor) = tooling.cursor else { return };
        if tooling.facts.suppression.is_some() {
            return;
        }
        // The end of a consumed keyword still belongs to its replacement
        // atom; whitespace after it belongs to the following binder slot.
        if tooling.facts.cursor.as_ref().is_some_and(|context| {
            !context.keywords.is_empty()
                && context.atom.replacement.start < cursor
                && context.atom.replacement.end == cursor
                && tooling.last_span == Some(context.atom.replacement)
        }) {
            return;
        }
        let end = self.tooling_cursor_end(tooling.cursor_limit);
        if tooling.last_end <= cursor && cursor <= end {
            let span = Span::new(tooling.last_end, end);
            let tooling = self.tooling.as_mut().expect("enabled above");
            tooling.facts.cursor = None;
            tooling.facts.suppression = Some(CursorSuppression { span, kind });
        }
    }

    fn tooling_cursor_end(&self, limit: u32) -> u32 {
        self.tooling_span_end(self.peek_span(), self.peek(), limit)
    }

    fn tooling_span_end(&self, span: Span, kind: Option<&TokenKind>, limit: u32) -> u32 {
        if span.start == span.end {
            self.eof.min(limit)
        } else if matches!(kind, Some(TokenKind::RParen | TokenKind::RBrace)) {
            span.start.min(limit)
        } else {
            span.end.min(limit)
        }
    }

    fn begin_body_scope(&mut self, signature: Option<&Signature>) -> Option<(Span, Option<Span>)> {
        self.begin_scope_region(ScopeOwnerKind::Body, signature)
    }

    fn begin_scope_region(
        &mut self,
        kind: ScopeOwnerKind,
        signature: Option<&Signature>,
    ) -> Option<(Span, Option<Span>)> {
        let tooling = self.tooling.as_ref()?;
        let cursor = tooling.cursor?;
        let open = tooling.last_span.expect("body follows its consumed opener");
        let close = self
            .cursor
            .current_group_close_span()
            .expect("body is in a skeleton group");
        let recovered = self.cursor.just_entered_recovered();
        let end = if recovered {
            self.eof.min(tooling.cursor_limit)
        } else {
            close.start
        };
        let interior = Span::new(open.end, end);
        let tooling = self.tooling.as_mut().expect("enabled above");
        let previous = tooling.body_owner.replace(open);
        if interior.start <= cursor && cursor <= interior.end {
            tooling.facts.regions.push(ScopeRegion {
                kind,
                open,
                close,
                interior,
                body: None,
                recovered,
                lambda: false,
            });
            if let Some(signature) = signature {
                tooling.facts.scope_prefix.push(ScopePrefix {
                    owner: open,
                    owner_kind: kind,
                    interval: interior,
                    syntax: ScopeSyntax::Signature(signature.clone()),
                });
            }
        }
        Some((open, previous))
    }

    fn finish_body_scope(&mut self, state: Option<(Span, Option<Span>)>, body: Option<Span>) {
        if let Some((owner, previous)) = state {
            let tooling = self.tooling.as_mut().expect("scope owns an enabled sink");
            tooling.body_owner = previous;
            if let Some(region) = tooling
                .facts
                .regions
                .iter_mut()
                .find(|region| region.open == owner)
            {
                region.body = body;
            }
        }
    }

    fn record_recursive_type_header(&mut self, name: &str, span: Span, params: &[TypeParam]) {
        let Some(tooling) = &mut self.tooling else {
            return;
        };
        let Some(owner) = tooling.body_owner else {
            return;
        };
        if let Some(ScopePrefix {
            syntax: ScopeSyntax::RecursiveTypes(names),
            ..
        }) = tooling.facts.scope_prefix.iter_mut().find(|prefix| {
            prefix.owner == owner && matches!(prefix.syntax, ScopeSyntax::RecursiveTypes(_))
        }) {
            names.push((PathSegment::new(name.to_owned(), span), params.to_vec()));
        }
    }

    fn declaration_scope_end(&self) -> u32 {
        self.cursor
            .remaining_current_frame()
            .iter()
            .find_map(|node| match node {
                SkeletonNode::Leaf(Token {
                    kind: TokenKind::Semicolon,
                    span,
                    ..
                }) => Some(span.end),
                _ => None,
            })
            .unwrap_or_else(|| {
                self.tooling
                    .as_ref()
                    .map_or(self.eof, |tooling| tooling.cursor_limit)
            })
    }

    fn with_recursive_type_scope<T>(
        &mut self,
        owner: Span,
        parse: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let end = self.declaration_scope_end();
        let Some(tooling) = &mut self.tooling else {
            return parse(self);
        };
        let Some(cursor) = tooling.cursor else {
            return parse(self);
        };
        if cursor < owner.end
            || cursor > end
            || tooling.facts.scope_prefix.iter().any(|prefix| {
                Some(prefix.owner) == tooling.body_owner
                    && matches!(prefix.syntax, ScopeSyntax::RecursiveTypes(_))
            })
        {
            return parse(self);
        }
        let previous = tooling.body_owner.replace(owner);
        tooling.facts.scope_prefix.push(ScopePrefix {
            owner,
            owner_kind: ScopeOwnerKind::RecursiveGroup,
            interval: Span::new(owner.end, end),
            syntax: ScopeSyntax::RecursiveTypes(Vec::new()),
        });
        let result = parse(self);
        self.tooling
            .as_mut()
            .expect("recursive scope owns tooling")
            .body_owner = previous;
        result
    }

    fn record_type_declaration(&mut self, name: PathSegment, interval: Span) {
        if let Some(tooling) = &mut self.tooling
            && tooling
                .cursor
                .is_some_and(|cursor| interval.start <= cursor && cursor <= interval.end)
        {
            tooling.facts.scope_prefix.push(ScopePrefix {
                owner: name.span,
                owner_kind: ScopeOwnerKind::Declaration,
                interval,
                syntax: ScopeSyntax::TypeDeclarations(vec![name]),
            });
        }
    }

    fn record_scope_prefix(&mut self, syntax: impl FnOnce() -> ScopeSyntax) {
        let Some(tooling) = &mut self.tooling else {
            return;
        };
        let Some(cursor) = tooling.cursor else { return };
        let Some(owner) = tooling.body_owner else {
            return;
        };
        let Some(region) = tooling
            .facts
            .regions
            .iter()
            .find(|region| region.open == owner)
        else {
            return;
        };
        let interval = Span::new(tooling.last_end, region.interior.end);
        if interval.start <= cursor && cursor <= interval.end {
            tooling.facts.scope_prefix.push(ScopePrefix {
                owner,
                owner_kind: ScopeOwnerKind::Body,
                interval,
                syntax: syntax(),
            });
        }
    }

    fn cursor_choices(&mut self, slot: CursorSlot, keywords: &[&'static str]) {
        self.cursor_choices_from(slot, keywords.iter().copied());
    }

    fn cursor_owned_atom(&mut self, slot: CursorSlot, replacement: Span) {
        let Some(tooling) = &mut self.tooling else {
            return;
        };
        let Some(cursor) = tooling.cursor else {
            return;
        };
        let start = replacement.start.min(tooling.last_end);
        if cursor < start || cursor > replacement.end || tooling.facts.suppression.is_some() {
            return;
        }
        if tooling
            .facts
            .cursor
            .as_ref()
            .is_some_and(|context| context.atom.replacement.end < replacement.start)
        {
            return;
        }
        let replacement = if cursor < replacement.start {
            Span::new(cursor, cursor)
        } else {
            replacement
        };
        tooling.facts.cursor = Some(CursorContext {
            span: Span::new(start, replacement.end),
            atom: CursorAtom {
                replacement,
                prefix: Span::new(replacement.start, cursor),
            },
            slot,
            keywords: Vec::new(),
            path: None,
            import: None,
            target: None,
            target_ids: Vec::new(),
            operator_prefix: None,
            call: None,
        });
    }

    fn operator_cursor(&mut self, is_prefix: bool) {
        let Some(cursor) = self.tooling.as_ref().and_then(|tooling| tooling.cursor) else {
            return;
        };
        let Some((span, prefix)) = self.operator_scope.cursor_prefix(self, is_prefix, cursor)
        else {
            return;
        };
        let slot = if is_prefix {
            CursorSlot::Value
        } else {
            CursorSlot::OperatorContinuation
        };
        self.cursor_owned_atom(slot, span);
        if let Some(context) = self
            .tooling
            .as_mut()
            .and_then(|tooling| tooling.facts.cursor.as_mut())
            && context.atom.replacement == span
        {
            context.operator_prefix = Some(prefix);
        }
    }

    fn next_named_entry(
        &mut self,
        slot: CursorSlot,
        entries: &[(&'static str, bool, &'static str)],
        expected: &str,
    ) -> Result<Option<&'static str>, Error> {
        self.cursor_choices_from(
            slot,
            entries
                .iter()
                .filter_map(|(name, available, _)| available.then_some(*name)),
        );
        if self.at_end() || matches!(self.peek(), Some(TokenKind::RBrace)) {
            return Ok(None);
        }
        let Some(&(name, available, duplicate)) = entries
            .iter()
            .find(|(name, _, _)| self.peek_ident_named(name))
        else {
            return Err(self.err_here(expected));
        };
        if !available {
            return Err(self.err_here(duplicate));
        }
        Ok(Some(name))
    }

    fn field_keywords(&mut self, fields: &[(&'static str, bool)]) {
        if !self
            .tooling
            .as_ref()
            .is_some_and(|tooling| tooling.collect_roles)
        {
            return;
        }
        self.cursor_choices_from(
            CursorSlot::Grammar,
            fields
                .iter()
                .filter_map(|(name, remaining)| remaining.then_some(*name)),
        );
        if let Some(Token {
            kind: TokenKind::Ident(name),
            span,
            ..
        }) = self.cursor.peek_token()
            && fields.iter().any(|(field, _)| field == name)
        {
            self.record_keyword(*span, KeywordRole::Declaration);
        }
    }

    fn cursor_choices_from(
        &mut self,
        slot: CursorSlot,
        keywords: impl Iterator<Item = &'static str>,
    ) {
        self.cursor_choices_at(slot, keywords, 0, None);
    }

    fn cursor_path_choices(
        &mut self,
        slot: CursorSlot,
        prefix: &[PathSegment],
        separator: Span,
        separator_kind: PathSeparator,
        offset: usize,
    ) {
        self.cursor_choices_at(slot, std::iter::empty(), offset, Some(separator.end));
        if let Some(tooling) = &mut self.tooling
            && let Some(context) = &mut tooling.facts.cursor
            && context.span.start == separator.end
            && context.path.is_none()
        {
            context.path = Some(CursorPath {
                prefix: prefix.into(),
                separator,
                separator_kind,
            });
        }
    }

    fn cursor_choices_at(
        &mut self,
        slot: CursorSlot,
        keywords: impl Iterator<Item = &'static str>,
        offset: usize,
        after: Option<u32>,
    ) {
        let Some(tooling) = &self.tooling else {
            return;
        };
        let Some(cursor) = tooling.cursor else {
            return;
        };
        if tooling.facts.cursor.is_some() || tooling.facts.suppression.is_some() {
            return;
        }
        let start = after.unwrap_or(tooling.last_end);
        if cursor < start {
            return;
        }
        let lookahead = (offset != 0).then(|| self.peek_token_at(offset)).flatten();
        let token = if offset == 0 {
            self.cursor.peek_token()
        } else {
            lookahead.as_ref()
        };
        let span = token.map_or_else(
            || {
                if offset == 0 {
                    self.peek_span()
                } else {
                    Span::new(self.eof, self.eof)
                }
            },
            |token| token.span,
        );
        let end = self.tooling_span_end(span, token.map(|token| &token.kind), tooling.cursor_limit);
        let suppression = token
            .and_then(|token| token_cursor_suppression(token, cursor))
            .or_else(|| trivia_cursor_suppression(&self.file_trailing_trivia, cursor));
        if let Some(suppression) = suppression {
            self.tooling
                .as_mut()
                .expect("enabled above")
                .facts
                .suppression = Some(suppression);
            return;
        }
        let replacement = token
            .filter(|token| {
                matches!(
                    token.kind,
                    TokenKind::Ident(_) | TokenKind::SymbolRun(_) | TokenKind::Slot1
                ) && token.span.start <= cursor
                    && cursor <= token.span.end
            })
            .map_or(Span::new(cursor, cursor), |token| token.span);
        let Some(tooling) = &mut self.tooling else {
            return;
        };
        if cursor <= end && tooling.facts.cursor.is_none() {
            tooling.facts.cursor = Some(CursorContext {
                span: Span::new(start, end),
                atom: CursorAtom {
                    replacement,
                    prefix: Span::new(replacement.start, cursor),
                },
                slot,
                keywords: keywords.collect(),
                path: None,
                import: None,
                target: None,
                target_ids: Vec::new(),
                operator_prefix: None,
                call: None,
            });
        }
    }

    fn tooling_checkpoint(&self) -> Option<ToolingCheckpoint> {
        self.tooling.as_ref().map(|tooling| ToolingCheckpoint {
            brackets: tooling.facts.structural_forall.bracket_offsets.len(),
            unclosed: tooling.facts.structural_forall.unclosed_at_eof,
            keywords: tooling.facts.keywords.len(),
            source_names: tooling.facts.source_names.len(),
            cursor: tooling.facts.cursor.clone(),
            block_cursor: tooling.facts.block_cursor.clone(),
            suppression: tooling.facts.suppression,
            regions: tooling.facts.regions.len(),
            scope_prefix: tooling.facts.scope_prefix.len(),
            last_span: tooling.last_span,
            body_owner: tooling.body_owner,
            last_end: tooling.last_end,
            recovered_group: tooling.facts.recovered_group,
        })
    }

    /// Move only the speculative suffix; the committed file prefix stays in
    /// its existing buffers across every alternative.
    fn take_tooling_suffix(
        &mut self,
        checkpoint: &Option<ToolingCheckpoint>,
    ) -> Option<ToolingSuffix> {
        let checkpoint = checkpoint.as_ref()?;
        let tooling = self
            .tooling
            .as_mut()
            .expect("checkpoint owns an enabled sink");
        let suffix = ToolingSuffix {
            brackets: tooling
                .facts
                .structural_forall
                .bracket_offsets
                .split_off(checkpoint.brackets),
            unclosed: tooling.facts.structural_forall.unclosed_at_eof,
            keywords: tooling.facts.keywords.split_off(checkpoint.keywords),
            source_names: tooling
                .facts
                .source_names
                .split_off(checkpoint.source_names),
            cursor: tooling.facts.cursor.take(),
            block_cursor: tooling.facts.block_cursor.take(),
            suppression: tooling.facts.suppression,
            regions: tooling.facts.regions.split_off(checkpoint.regions),
            scope_prefix: tooling
                .facts
                .scope_prefix
                .split_off(checkpoint.scope_prefix),
            last_span: tooling.last_span,
            body_owner: tooling.body_owner,
            last_end: tooling.last_end,
            recovered_group: tooling.facts.recovered_group,
        };
        tooling.facts.structural_forall.unclosed_at_eof = checkpoint.unclosed;
        tooling.facts.cursor = checkpoint.cursor.clone();
        tooling.facts.block_cursor = checkpoint.block_cursor.clone();
        tooling.facts.suppression = checkpoint.suppression;
        tooling.last_span = checkpoint.last_span;
        tooling.body_owner = checkpoint.body_owner;
        tooling.last_end = checkpoint.last_end;
        tooling.facts.recovered_group = checkpoint.recovered_group;
        Some(suffix)
    }

    fn restore_tooling_suffix(
        &mut self,
        checkpoint: &Option<ToolingCheckpoint>,
        suffix: Option<ToolingSuffix>,
    ) {
        let _discarded = self.take_tooling_suffix(checkpoint);
        if let Some(suffix) = suffix {
            let tooling = self.tooling.as_mut().expect("suffix owns an enabled sink");
            tooling
                .facts
                .structural_forall
                .bracket_offsets
                .extend(suffix.brackets);
            tooling.facts.structural_forall.unclosed_at_eof = suffix.unclosed;
            tooling.facts.keywords.extend(suffix.keywords);
            tooling.facts.source_names.extend(suffix.source_names);
            tooling.facts.cursor = suffix.cursor;
            tooling.facts.block_cursor = suffix.block_cursor;
            tooling.facts.suppression = suffix.suppression;
            tooling.facts.regions.extend(suffix.regions);
            tooling.facts.scope_prefix.extend(suffix.scope_prefix);
            tooling.last_span = suffix.last_span;
            tooling.body_owner = suffix.body_owner;
            tooling.last_end = suffix.last_end;
            tooling.facts.recovered_group = suffix.recovered_group;
        }
    }

    fn record_unclosed_structural_forall_at_eof(&mut self) {
        if let Some(tooling) = &mut self.tooling {
            tooling.facts.structural_forall.unclosed_at_eof = true;
        }
    }

    /// The real end of an expression fragment, including the zero-width
    /// synthetic closer which the tree skeleton gives a recovered outer
    /// parenthesis or brace. Normal source closers have a non-empty span and
    /// are syntax errors rather than continuation evidence.
    fn at_fragment_eof(&self) -> bool {
        if self.peek().is_none() {
            return true;
        }
        self.tooling.is_some()
            && matches!(self.peek(), Some(TokenKind::RParen | TokenKind::RBrace))
            && self.peek_span().start == self.peek_span().end
    }

    #[cfg(any(test, feature = "repl"))]
    pub(super) fn structural_forall_facts(&self) -> StructuralForallFacts {
        self.tooling
            .as_ref()
            .map(|tooling| tooling.facts.structural_forall.clone())
            .unwrap_or_default()
    }

    /// After parsing a primary-and-postfix expression, check for a
    /// user-defined operator continuation. If the next token is a
    /// `SymbolRun` registered in `operator_scope`, consume it and
    /// parse a right-hand side with the operator's associativity:
    ///
    /// - **Non-assoc** (`op _ OP _ { impl …; };`): exactly one binary
    ///   application; chaining (`a OP b OP c`) is a parse error.
    /// - **Right-assoc** (`op _ OP __ { impl …; };`): recurse on the RHS
    ///   so `a OP b OP c` parses as `OP(a, OP(b, c))`.
    /// - **Left-assoc** (`op __ OP _ { impl …; };`): iterate so
    ///   `a OP b OP c` parses as `OP(OP(a, b), c)`.
    ///
    /// Mixing different operators in one chain (e.g., `a + b * c`)
    /// is a parse error per spec — operator dispatch is precedence-
    /// free; users disambiguate with explicit parens.
    fn try_operator_continuation(&mut self, mut lhs: Expr) -> Result<Expr, Error> {
        if !self.operator_scope.non_prefix.leaves.is_empty() {
            self.cursor_choices(CursorSlot::OperatorContinuation, &[]);
        }
        self.operator_cursor(false);
        let Some((binding, n_tokens)) = self.operator_scope.find_at(self, false) else {
            return Ok(lhs);
        };
        // Display name for diagnostics: the leading op-token run we
        // matched on. (Multi-token patterns like `_ ?? _` display as
        // `"??"`, but `_ && _` displays as `"&&"` because the trie
        // walked two `Amp` tokens.)
        let leading_run = leading_op_run(&binding.pattern);
        let content = leading_run.join("");
        self.consume_operator_run_after(&mut lhs, n_tokens);
        // Skip past the first slot (the LHS we already parsed) and
        // the leading op-token run we just consumed.
        let pattern = binding.pattern.clone();
        let first_slot_pos = pattern
            .iter()
            .position(|p| matches!(p, OpPart::SlotPlain { .. } | OpPart::SlotRecursive { .. }))
            .expect("non-prefix pattern starts with a slot");
        let after_leading_run = first_slot_pos + 1 + n_tokens;
        let pattern_tail = &pattern[after_leading_run..];
        // Dispatch by pattern shape:
        // - Postfix unary: tail is empty (just `__ Tok` or `_ Tok`).
        // - Binary (right-assoc): tail is `[Recursive]`.
        // - Binary (left-assoc): tail is `[Plain]` and the FIRST
        //   slot of the pattern was Recursive.
        // - Binary (non-assoc): tail is `[Plain]` and the first
        //   slot was Plain.
        // - Multi-token (ternary etc.): tail starts with a slot,
        //   has multiple Token entries, etc.
        let first_slot_is_recursive = matches!(pattern.first(), Some(OpPart::SlotRecursive { .. }));
        let last_slot_is_recursive = pattern
            .iter()
            .rev()
            .find(|p| matches!(p, OpPart::SlotPlain { .. } | OpPart::SlotRecursive { .. }))
            .map(|p| matches!(p, OpPart::SlotRecursive { .. }))
            .unwrap_or(false);
        // Walk the rest of the pattern, parsing operands and
        // expecting the operator tokens between them. The
        // recursive slot, when it's the last slot of the pattern,
        // drives right-associative chain continuation (same-op
        // only). It need not be the literal-last pattern element —
        // matched-pair patterns like `_ <| __ |>` have a closing
        // token after the recursive slot, and the closing token
        // terminates the nested usage's parse so chaining still
        // composes via the recursive slot.
        let last_slot_index_in_tail = pattern_tail
            .iter()
            .enumerate()
            .rev()
            .find(|(_, p)| matches!(p, OpPart::SlotPlain { .. } | OpPart::SlotRecursive { .. }))
            .map(|(i, _)| i);
        let mut operands: Vec<Expr> = vec![lhs];
        for (i, part) in pattern_tail.iter().enumerate() {
            match part {
                OpPart::Token {
                    content: expected, ..
                } => {
                    let actual = self.peek().and_then(token_kind_to_op_string);
                    if actual.as_deref() != Some(expected.as_str()) {
                        return Err(self.err_here(format!(
                            "expected `{expected}` to continue the `{content}` operator pattern"
                        )));
                    }
                    self.advance();
                }
                OpPart::SlotPlain { .. }
                | OpPart::SlotRecursive { .. }
                | OpPart::SlotGreedy { .. } => {
                    let is_last_slot = Some(i) == last_slot_index_in_tail;
                    let is_recursive = matches!(part, OpPart::SlotRecursive { .. });
                    let is_greedy = matches!(part, OpPart::SlotGreedy { .. });
                    let is_lenient = matches!(
                        part,
                        OpPart::SlotPlain { lenient: true, .. }
                            | OpPart::SlotRecursive { lenient: true, .. }
                    );
                    let has_closer = pattern_tail
                        .get(i + 1)
                        .is_some_and(|part| matches!(part, OpPart::Token { .. }));
                    let operand_result = self.with_block_slot_boundary(has_closer, |parser| {
                        Ok(if is_greedy {
                            // Greedy slot (`___`) admits any single-op
                            // chain in its operand position — call the
                            // full expression parser (which itself runs
                            // operator-continuation). Cross-op mixing
                            // inside the slot is still rejected by the
                            // inner `try_operator_continuation` (no
                            // precedence in Kio). No outer continuation
                            // tokens to preserve, so `lenient_depth`
                            // stays at zero.
                            parser.expr()?
                        } else if is_lenient {
                            // Lenient `( … )`-wrapped slot admits the
                            // same single-op chain, but the outer
                            // operator may have continuation tokens
                            // after the slot — so bump `lenient_depth`
                            // for the duration of the inner parse, and
                            // the chain-mixing check leaves any
                            // unconsumed op-tokens for the enclosing
                            // pattern walk to match.
                            parser.lenient_depth += 1;
                            let r = parser.expr();
                            parser.lenient_depth -= 1;
                            r?
                        } else if is_last_slot && is_recursive && last_slot_is_recursive {
                            // Right-recursive (`__` at the last slot):
                            // parse the operand as a postfix expression,
                            // then continue with the same operator only
                            // — mixing different operators is rejected.
                            let inner = parser.expr_postfix()?;
                            parser.try_operator_continuation_same_op(
                                &content,
                                &binding,
                                &pattern_tail[i + 1..],
                                inner,
                            )?
                        } else {
                            parser.expr_postfix()?
                        })
                    });
                    operands.push(operand_result?);
                }
            }
        }

        // Emit the OpChain placeholder. The operator-fold pass
        // re-looks-up the leading run in the module's operator
        // scope (now augmented with cross-module imports) and
        // rebuilds a concrete `Expr::Call`. The full pattern
        // travels with the chain so both the fold pass and the
        // pretty-printer can reconstruct token positions without
        // consulting any external scope.
        let last_end = operands.last().unwrap().span().end;
        let span = Span::new(operands[0].span().start, last_end);
        let chain = Expr::OpChain {
            occurrence: Default::default(),
            kind: OpChainKind::Normal {
                pattern: pattern.clone(),
                slots: operands,
            },
            meta: Meta::new(span),
            ext: (),
        };

        // If the FIRST slot was the recursive one and the pattern
        // is binary-shaped (single token, two slots), we iterate
        // for left-associative chains — `a OP b OP c` →
        // `OP(OP(a, b), c)`. Multi-token left-recursive patterns
        // aren't supported in this slice; the validator already
        // accepted them, so the iteration only fires for the
        // exact 3-element `[Recursive, Token, Plain]` shape.
        if first_slot_is_recursive
            && pattern.len() == 2 + n_tokens
            && pattern[1..1 + n_tokens]
                .iter()
                .all(|p| matches!(p, OpPart::Token { .. }))
        {
            let mut acc = chain;
            loop {
                match self.operator_scope.find_at(self, false) {
                    Some((next_binding, next_n))
                        if same_operator_binding(&next_binding, &binding) =>
                    {
                        self.consume_operator_run_after(&mut acc, next_n);
                        let rhs = self.expr_postfix()?;
                        let span = Span::new(acc.span().start, rhs.span().end);
                        acc = Expr::OpChain {
                            occurrence: Default::default(),
                            kind: OpChainKind::Normal {
                                pattern: pattern.clone(),
                                slots: vec![acc, rhs],
                            },
                            meta: Meta::new(span),
                            ext: (),
                        };
                    }
                    Some((next_binding, _)) => {
                        if self.lenient_depth > 0 {
                            break;
                        }
                        let next_content = leading_op_run(&next_binding.pattern).join("");
                        return Err(self.operator_chain_error(&content, &next_content));
                    }
                    None => break,
                }
            }
            return Ok(acc);
        }

        // For postfix unary (tail empty), iterate so `a OP OP`
        // wraps repeatedly.
        if pattern_tail.is_empty() {
            let mut acc = chain;
            loop {
                match self.operator_scope.find_at(self, false) {
                    Some((next_binding, next_n))
                        if same_operator_binding(&next_binding, &binding) =>
                    {
                        let next_span = self.peek_span();
                        self.consume_operator_run_after(&mut acc, next_n);
                        let span = Span::new(acc.span().start, next_span.end);
                        acc = Expr::OpChain {
                            occurrence: Default::default(),
                            kind: OpChainKind::Normal {
                                pattern: pattern.clone(),
                                slots: vec![acc],
                            },
                            meta: Meta::new(span),
                            ext: (),
                        };
                    }
                    Some((next_binding, _)) => {
                        if self.lenient_depth > 0 {
                            break;
                        }
                        let next_content = leading_op_run(&next_binding.pattern).join("");
                        return Err(self.operator_chain_error(&content, &next_content));
                    }
                    None => break,
                }
            }
            return Ok(acc);
        }

        // Non-assoc check: if the pattern has no recursive slot
        // anywhere, reject any immediate chain — this covers both
        // binary `_ OP _` and ternary-non-assoc `_ OP1 _ OP2 _`.
        // Inside a lenient `( … )` slot, leave the next op-token
        // run for the enclosing pattern to match instead of
        // erroring.
        if !first_slot_is_recursive
            && !last_slot_is_recursive
            && let Some((next_binding, _)) = self.operator_scope.find_at(self, false)
        {
            if self.lenient_depth > 0 {
                return Ok(chain);
            }
            let next_content = leading_op_run(&next_binding.pattern).join("");
            return Err(self.operator_chain_error(&content, &next_content));
        }

        Ok(chain)
    }

    /// The lexer's leading-only trivia before an operator follows its operand.
    /// Append it after any comments already retained from grouping closers.
    fn consume_operator_run_after(&mut self, operand: &mut Expr, count: usize) {
        for _ in 0..count {
            if let Some(token) = self.cursor.peek_token()
                && token
                    .leading_trivia
                    .iter()
                    .any(|trivia| comment_span(trivia).is_some())
            {
                operand
                    .meta_mut()
                    .trailing_trivia
                    .extend(token.leading_trivia.iter().cloned());
            }
            self.advance();
        }
    }

    /// Continuation pinned to a specific operator. Used by the
    /// right-associative recursive-slot path: only the same op
    /// drives further folding. A different in-scope op is a mixing
    /// error unless its tokens belong to the enclosing pattern's
    /// pending closing run; anything else stops.
    fn try_operator_continuation_same_op(
        &mut self,
        op_str: &str,
        binding: &OperatorBinding,
        closing_tokens: &[OpPart],
        lhs: Expr,
    ) -> Result<Expr, Error> {
        match self.operator_scope.find_at(self, false) {
            Some((next_binding, _)) if same_operator_binding(&next_binding, binding) => {
                // Same operator: re-enter the dispatcher, which
                // will parse the rest of the pattern and recurse
                // on the recursive slot again.
                self.try_operator_continuation(lhs)
            }
            Some((next_binding, _)) => {
                let at_own_close = !closing_tokens.is_empty()
                    && closing_tokens.iter().enumerate().all(|(offset, part)| {
                        matches!(part, OpPart::Token { content, .. }
                            if self.peek_at(offset).as_ref().and_then(token_kind_to_op_string).as_deref()
                                == Some(content.as_str()))
                    });
                if at_own_close {
                    return Ok(lhs);
                }
                if self.lenient_depth > 0 {
                    return Ok(lhs);
                }
                let next_content = leading_op_run(&next_binding.pattern).join("");
                Err(self.operator_chain_error(op_str, &next_content))
            }
            None => Ok(lhs),
        }
    }

    /// Delimiters determine this grammar before any operator is resolved.
    fn try_variadic_literal(&mut self) -> Result<Option<Expr>, Error> {
        self.with_block_slot_boundary(true, |parser| parser.try_variadic_literal_body())
    }

    fn try_variadic_literal_body(&mut self) -> Result<Option<Expr>, Error> {
        let Some(TokenKind::SymbolRun(open)) = self.peek() else {
            return Ok(None);
        };
        let Some(close) = mirrored_varop_close(open) else {
            return Ok(None);
        };
        let open = open.clone();
        let start = self.peek_span().start;
        self.advance();
        let mut elements = Vec::new();
        let mut pending_trivia = self.peek_leading_trivia();
        loop {
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if self.peek().is_some_and(|kind| kind.is_sym(&close)) {
                break;
            }
            let mut element = self.expr()?;
            inject_leading_trivia(&mut element, std::mem::take(&mut pending_trivia));
            elements.push(element);
            pending_trivia = self.peek_leading_trivia();
            if !matches!(self.peek(), Some(TokenKind::Comma))
                && !self.peek().is_some_and(|kind| kind.is_sym(&close))
            {
                return Err(
                    self.err_here(format!("expected `,` or mirrored varop CLOSE `{close}`"))
                );
            }
        }
        if let Some(last) = elements.last_mut() {
            set_trailing_trivia(last.meta_mut(), std::mem::take(&mut pending_trivia));
        }
        let end = self
            .expect_kind(TokenKind::sym(&close), &format!("`{close}`"))?
            .end;
        Ok(Some(Expr::OpChain {
            occurrence: Default::default(),
            kind: OpChainKind::Variadic {
                open_tokens: vec![open],
                close_tokens: vec![close],
                elements,
                empty_trivia: pending_trivia,
            },
            meta: Meta::new(Span::new(start, end)),
            ext: (),
        }))
    }

    fn try_prefix_operator(&mut self) -> Result<Option<Expr>, Error> {
        self.operator_cursor(true);
        let Some((binding, n_tokens)) = self.operator_scope.find_at(self, true) else {
            return Ok(None);
        };
        let op_span = self.peek_span();
        // Consume the leading op-token run.
        for _ in 0..n_tokens {
            self.advance();
        }
        let mut slots = Vec::new();
        let mut end = op_span.end;
        let tail = &binding.pattern[n_tokens..];
        for (index, part) in tail.iter().enumerate() {
            match part {
                OpPart::Token {
                    content: expected, ..
                } => {
                    let actual = self.peek().and_then(token_kind_to_op_string);
                    if actual.as_deref() != Some(expected.as_str()) {
                        return Err(self.err_here(format!(
                            "expected `{expected}` to continue the `{}` operator pattern",
                            leading_op_run(&binding.pattern).join(""),
                        )));
                    }
                    end = self.peek_span().end;
                    self.advance();
                }
                OpPart::SlotGreedy { .. }
                | OpPart::SlotPlain { .. }
                | OpPart::SlotRecursive { .. } => {
                    let has_closer = tail
                        .get(index + 1)
                        .is_some_and(|part| matches!(part, OpPart::Token { .. }));
                    let operand = self.with_block_slot_boundary(has_closer, |parser| {
                        if matches!(part, OpPart::SlotGreedy { .. }) {
                            parser.expr()
                        } else if matches!(
                            part,
                            OpPart::SlotPlain { lenient: true, .. }
                                | OpPart::SlotRecursive { lenient: true, .. }
                        ) {
                            parser.lenient_depth += 1;
                            let result = parser.expr();
                            parser.lenient_depth -= 1;
                            result
                        } else {
                            parser.expr_postfix()
                        }
                    })?;
                    end = operand.span().end;
                    slots.push(operand);
                }
            }
        }
        let span = Span::new(op_span.start, end);
        Ok(Some(Expr::OpChain {
            occurrence: Default::default(),
            kind: OpChainKind::Normal {
                pattern: binding.pattern.clone(),
                slots,
            },
            meta: Meta::new(span),
            ext: (),
        }))
    }

    /// Build the diagnostic for chaining operators in one
    /// expression: same-op chains on a non-associative operator
    /// get the "non-associative" wording; different-op chains get
    /// the "different operators" / no-precedence wording.
    fn operator_chain_error(&self, first: &str, next: &str) -> Error {
        let msg = if first == next {
            format!(
                "operator `{first}` is non-associative; chained use \
                 (`a {first} b {first} c`) requires explicit parens"
            )
        } else {
            format!(
                "different operators (`{first}` and `{next}`) in one expression require \
                 explicit parens — operator dispatch is precedence-free"
            )
        };
        self.err_here(msg)
    }

    /// Parses a primary expression, then loops over postfix call
    /// applications (`(args)`) and UFCS-style call splices (`.>f`,
    /// `.>>f(args)`, `f(args).<x`, `f(args).<<x`). The postfix forms
    /// compose freely: chains like `r.>f.>g(x)` or `f(r).>g` fall
    /// out of the loop.
    fn expr_postfix(&mut self) -> Result<Expr, Error> {
        let mut e = self.expr_primary()?;
        loop {
            match self.peek() {
                Some(TokenKind::LParen) => {
                    let start = e.span().start;
                    let list_start = self.peek_span().start;
                    if let Expr::Path { segments, .. } = &e
                        && let Some(callee) = segments.last()
                    {
                        self.record_source_name(callee.span, SourceNameRole::FunctionReference);
                    }
                    let (args, end) = self.call_arg_list(Some(&e))?;
                    if args.is_empty()
                        && self.peek_left_call_splice_flavor().is_some()
                        && matches!(&e, Expr::Path { .. })
                    {
                        return Err(empty_ufcs_arg_list_error(Span::new(list_start, end)));
                    }
                    let call_span = Span::new(start, end);
                    if args.is_empty() && matches!(&e, Expr::Path { .. }) {
                        self.empty_path_call_arg_lists
                            .insert(call_span, Span::new(list_start, end));
                    }
                    e = Expr::Call {
                        occurrence: Default::default(),
                        callee: Box::new(e),
                        args,
                        meta: Meta::new(call_span),
                        ext: (),
                    };
                }
                Some(k) if k.is_sym(".?") && matches!(self.peek_at(1), Some(TokenKind::LBrace)) => {
                    let start = e.span().start;
                    self.advance();
                    let (labels, end) = self.field_access_label_list()?;
                    let id = self.fresh_node_id();
                    e = Expr::Elaborator {
                        occurrence: Default::default(),
                        kind: ElaboratorKind::Access,
                        call: ElaboratorCall::FieldAccess {
                            receiver: Box::new(e),
                            labels,
                        },
                        meta: Meta::new(Span::new(start, end)),
                        ext: id,
                    };
                }
                Some(k) if k.is_sym(".!") && matches!(self.peek_at(1), Some(TokenKind::LBrace)) => {
                    let start = e.span().start;
                    self.advance();
                    let (updates, end) = self.field_update_label_list()?;
                    let id = self.fresh_node_id();
                    e = Expr::Elaborator {
                        occurrence: Default::default(),
                        kind: ElaboratorKind::Filtered,
                        call: ElaboratorCall::FieldUpdate {
                            receiver: Box::new(e),
                            updates,
                        },
                        meta: Meta::new(Span::new(start, end)),
                        ext: id,
                    };
                }
                Some(k) if k.is_sym(".>") || k.is_sym(".>>") => {
                    let flavor = if k.is_sym(".>") {
                        UfcsFlavor::ReceiverFirst
                    } else {
                        UfcsFlavor::ReceiverLast
                    };
                    let start = e.span().start;
                    let arrow_end = self.peek_span().end;
                    // A comment trailing the receiver lands on the
                    // `.>` token's leading trivia (the lexer's
                    // leading-only model). Stash it on the receiver's
                    // trailing slot so `doc_ufcs_chain` can emit it
                    // before this segment instead of dropping it.
                    let mut splice_comments = Vec::new();
                    Self::collect_comments_into(self.peek_leading_trivia(), &mut splice_comments);
                    set_trailing_trivia(e.meta_mut(), splice_comments);
                    self.advance(); // consume `.>` / `.>>`
                    let (segments, callee_span_end) = self.ufcs_callee_path()?;
                    let callee_span = Span::new(arrow_end, callee_span_end);
                    // Elaborator bang-call splice: `r.>iso!(T)`,
                    // `r.>>match!(arms)`, etc. The trailing `!`
                    // after a single-segment UFCS callee records a generic
                    // elaborator-UFCS node. Typechecking later resolves the
                    // elaborator, applies `flavor` to its public call type's
                    // value slots, and records the expansion for substitution.
                    // The peek accepts
                    // a fused `!`-prefix SymbolRun
                    // (`!.`, `!=`, …) too — greedy lexer fusion can
                    // absorb a trailing op-char into the `!` run,
                    // and `expect_bang` peels it back.
                    if self.at_sym_prefix("!") {
                        e = self.elaborator_ufcs_call(e, segments, callee_span, start, flavor)?;
                        continue;
                    }
                    // Optional argument list. A bare `r.>f` (no
                    // parens) builds a zero-arg UFCS, mirroring
                    // projector-style calls.
                    let (args, end) = if matches!(self.peek(), Some(TokenKind::LParen)) {
                        let list_start = self.peek_span().start;
                        let (args, end) = self.call_arg_list(None)?;
                        if args.is_empty() {
                            return Err(empty_ufcs_arg_list_error(Span::new(list_start, end)));
                        }
                        (args, end)
                    } else {
                        (Vec::new(), callee_span_end)
                    };
                    let id = self.fresh_node_id();
                    e = Expr::Ufcs {
                        occurrence: Default::default(),
                        receiver: Box::new(e),
                        callee_segments: segments,
                        callee_span,
                        args,
                        flavor,
                        bang: None,
                        meta: Meta::new(Span::new(start, end)),
                        ext: id,
                    };
                }
                Some(k) if k.is_sym(".<") || k.is_sym(".<<") => {
                    let flavor = if k.is_sym(".<") {
                        UfcsFlavor::ArgumentLast
                    } else {
                        UfcsFlavor::ArgumentFirst
                    };
                    let splice_span = self.peek_span();
                    let start = e.span().start;
                    let (segments, callee_span, args) =
                        self.split_left_call_splice(e, splice_span)?;
                    if let Some(context) = self
                        .tooling
                        .as_mut()
                        .and_then(|tooling| tooling.facts.cursor.as_mut())
                        && context
                            .call
                            .as_ref()
                            .is_some_and(|call| call.callee.span() == callee_span)
                    {
                        context.call = None;
                    }
                    Self::validate_reference_value_path_leaf(&segments)?;
                    self.advance(); // consume `.<` / `.<<`
                    let inserted = self.expr_tight_call_arg()?;
                    let end = inserted.span().end;
                    let id = self.fresh_node_id();
                    e = Expr::Ufcs {
                        occurrence: Default::default(),
                        receiver: Box::new(inserted),
                        callee_segments: segments,
                        callee_span,
                        args,
                        flavor,
                        bang: None,
                        meta: Meta::new(Span::new(start, end)),
                        ext: id,
                    };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn field_access_label_list(&mut self) -> Result<(Vec<FieldAccessLabel>, u32), Error> {
        let start = self.peek_span().start;
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut labels: Vec<FieldAccessLabel> = Vec::new();
        let mut pending_trivia = self.peek_leading_trivia();
        while matches!(self.peek(), Some(TokenKind::Comma)) {
            self.advance();
            pending_trivia.extend(self.peek_leading_trivia());
        }
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let leading = std::mem::take(&mut pending_trivia);
            let LabelPathParts {
                full: label,
                full_span: label_span,
                ..
            } = self.label_path_parts()?;
            let mut item = FieldAccessLabel {
                label,
                label_span,
                label_type: None,
                meta: Meta::new(label_span),
            };
            inject_meta_trivia(&mut item.meta, leading);
            labels.push(item);
            pending_trivia = self.peek_leading_trivia();
            let mut saw_comma = false;
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                saw_comma = true;
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `}`"));
            }
        }
        let end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        let _ = start;
        Ok((labels, end))
    }

    fn field_update_label_list(&mut self) -> Result<(Vec<FieldUpdateLabel>, u32), Error> {
        let start = self.peek_span().start;
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut updates: Vec<FieldUpdateLabel> = Vec::new();
        let mut pending_trivia = self.peek_leading_trivia();
        while matches!(self.peek(), Some(TokenKind::Comma)) {
            self.advance();
            pending_trivia.extend(self.peek_leading_trivia());
        }
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let leading = std::mem::take(&mut pending_trivia);
            let LabelPathParts {
                full: label,
                full_span: label_span,
                last,
                last_span,
            } = self.label_path_parts()?;
            let value = if self.at_sym("=") {
                let eq_span = self.expect_sym("=", "`=`")?;
                if matches!(
                    self.peek(),
                    Some(TokenKind::Comma) | Some(TokenKind::RBrace)
                ) {
                    Expr::Unit {
                        occurrence: Default::default(),
                        meta: Meta::new(eq_span),
                    }
                } else {
                    self.expr()?
                }
            } else {
                Expr::Path {
                    occurrence: Default::default(),
                    segments: vec![PathSegment::new(last, last_span)],
                    meta: Meta::new(last_span),
                    ext: (),
                }
            };
            let value_end = value.span().end;
            let mut item = FieldUpdateLabel {
                label,
                label_span,
                label_type: None,
                value,
                meta: Meta::new(Span::new(label_span.start, value_end)),
            };
            inject_meta_trivia(&mut item.meta, leading);
            updates.push(item);
            pending_trivia = self.peek_leading_trivia();
            let mut saw_comma = false;
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                saw_comma = true;
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `}`"));
            }
        }
        let end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        let _ = start;
        Ok((updates, end))
    }

    fn row_let_entry_list(&mut self) -> Result<(Vec<RowLetEntry>, u32), Error> {
        let start = self.peek_span().start;
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut entries: Vec<RowLetEntry> = Vec::new();
        let mut locals: Vec<(String, Span)> = Vec::new();
        let mut pending_trivia = self.peek_leading_trivia();
        while matches!(self.peek(), Some(TokenKind::Comma)) {
            self.advance();
            pending_trivia.extend(self.peek_leading_trivia());
        }
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let leading = std::mem::take(&mut pending_trivia);
            let LabelPathParts {
                full: label,
                full_span: label_span,
                last,
                last_span,
            } = self.label_path_parts()?;
            self.cursor_choices(CursorSlot::Grammar, &["as"]);
            let (local, local_span, alias_explicit) = if self.peek_ident_named("as") {
                self.expect_ident_named("as")?;
                let (name, span) = self.expect_ident()?;
                Self::validate_value_name(&name, span)?;
                self.record_source_name(span, SourceNameRole::Parameter);
                (name, span, true)
            } else {
                (last, last_span, false)
            };
            if let Some((_, first_span)) = locals.iter().find(|(seen, _)| seen == &local) {
                return Err(self
                    .err(local_span, format!("duplicate row-let local `{local}`"))
                    .with_secondary(*first_span, format!("`{local}` first bound here")));
            }
            locals.push((local.clone(), local_span));
            let mut item = RowLetEntry {
                label,
                label_span,
                local,
                local_span,
                alias_explicit,
                access_ext: self.fresh_node_id(),
                meta: Meta::new(label_span),
            };
            inject_meta_trivia(&mut item.meta, leading);
            entries.push(item);
            pending_trivia = self.peek_leading_trivia();
            let mut saw_comma = false;
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                saw_comma = true;
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `}`"));
            }
        }
        let end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        if entries.is_empty() {
            return Err(self.err(
                Span::new(start, end),
                "row-let `.{...}` must include at least one label",
            ));
        }
        Ok((entries, end))
    }

    /// Parse the right-hand value of `.<` / `.<<`: a primary expression
    /// plus ordinary call applications, stopping before any dot-led
    /// splice so `f.<x.>g` means `f(x).>g`. To pass a UFCS/splice
    /// expression as the inserted value, parenthesize it.
    fn expr_tight_call_arg(&mut self) -> Result<Expr, Error> {
        let mut e = self.expr_primary()?;
        while matches!(self.peek(), Some(TokenKind::LParen)) {
            let start = e.span().start;
            let (args, end) = self.call_arg_list(None)?;
            e = Expr::Call {
                occurrence: Default::default(),
                callee: Box::new(e),
                args,
                meta: Meta::new(Span::new(start, end)),
                ext: (),
            };
        }
        Ok(e)
    }

    /// Split the already-parsed left side of `.<` / `.<<` into a
    /// path-shaped callee and its existing direct-call arguments.
    /// This keeps the splice syntactic: `f.<x` and `f(a).<x` are
    /// accepted, but an arbitrary expression on the left is not
    /// retroactively treated as a function by type.
    fn split_left_call_splice(
        &self,
        lhs: Expr,
        splice_span: Span,
    ) -> Result<(Vec<PathSegment>, Span, Vec<CallArg>), Error> {
        match lhs {
            Expr::Path { segments, meta, .. } => Ok((segments, meta.span, Vec::new())),
            Expr::Call {
                callee, args, meta, ..
            } => match *callee {
                Expr::Path {
                    segments,
                    meta: callee_meta,
                    ..
                } => {
                    if args.is_empty() {
                        let list_span = self
                            .empty_path_call_arg_lists
                            .get(&meta.span)
                            .copied()
                            .unwrap_or_else(|| Span::new(callee_meta.span.end, meta.span.end));
                        return Err(empty_ufcs_arg_list_error(list_span));
                    }
                    Ok((segments, callee_meta.span, args))
                }
                _ => Err(Error::parse(
                    splice_span,
                    "`.<` / `.<<` require a path or direct path call on the left",
                )),
            },
            _ => Err(Error::parse(
                splice_span,
                "`.<` / `.<<` require a path or direct path call on the left",
            )),
        }
    }

    fn peek_left_call_splice_flavor(&self) -> Option<UfcsFlavor> {
        match self.peek() {
            Some(k) if k.is_sym(".<") => Some(UfcsFlavor::ArgumentLast),
            Some(k) if k.is_sym(".<<") => Some(UfcsFlavor::ArgumentFirst),
            _ => None,
        }
    }

    /// Parse a UFCS callee path with the same path syntax as a value
    /// expression path: a single identifier or a dotted path (`m.f`,
    /// `T.member`).
    fn ufcs_callee_path(&mut self) -> Result<(Vec<PathSegment>, u32), Error> {
        // This position is the callee path selected by `.>` / `.>>`, not an
        // operand boundary where a non-prefix operator may begin. A direct FQN
        // is therefore rejected even when `/` is imported in the surrounding
        // expression scope.
        if let Some(error) = self.direct_value_fqn_error() {
            return Err(error);
        }
        let (segments, end) = self.value_path_segments()?;
        Self::validate_reference_value_path_leaf(&segments)?;
        if let Some(callee) = segments.last() {
            self.record_source_name(callee.span, SourceNameRole::FunctionReference);
        }
        Ok((segments, end))
    }

    /// Parse the tail of a right-callee elaborator splice, having
    /// already consumed the inserted value and the `.>` / `.>>`
    /// path segments. A bang-suffix UFCS callee must be a single path segment;
    /// typechecking later requires that name to resolve to an in-scope user
    /// elaborator.
    ///
    /// The result is an [`Expr::Ufcs`] with `bang = Some(bang_span)`
    /// so the pretty-printer can round-trip `r.>iso!(T)` / `r.>>iso!(T)`
    /// faithfully. The post-`!` parentheses use ordinary call-argument parsing.
    /// During typechecking, the elaborator's public call type and `flavor`
    /// determine receiver placement; substitution installs the recorded
    /// expansion before Prime.
    fn elaborator_ufcs_call(
        &mut self,
        receiver: Expr,
        segments: Vec<PathSegment>,
        callee_span: Span,
        start: u32,
        flavor: UfcsFlavor,
    ) -> Result<Expr, Error> {
        // Consume the `!` — peel from the front of any fused
        // SymbolRun (e.g. `!.`) so the trailing op-chars stay
        // available for the next consumer.
        let bang_span = self.expect_bang()?;
        if segments.len() != 1 {
            let callee = segments
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(".");
            return Err(self.err(
                Span::new(callee_span.start, bang_span.end),
                format!(
                    "the `!` suffix after `{}` requires a single elaborator name; \
                     got `{}{callee}!`",
                    flavor.token(),
                    flavor.token(),
                ),
            ));
        }
        let name = &segments[0];
        Self::validate_value_name(&name.name, name.span)?;
        let (args, end_pos) = if matches!(self.peek(), Some(TokenKind::LParen)) {
            let list_start = self.peek_span().start;
            let (args, end) = self.call_arg_list(None)?;
            if args.is_empty() {
                return Err(empty_ufcs_arg_list_error(Span::new(list_start, end)));
            }
            (args, end)
        } else {
            (Vec::new(), bang_span.end)
        };
        let id = self.fresh_node_id();
        Ok(Expr::Ufcs {
            occurrence: Default::default(),
            receiver: Box::new(receiver),
            callee_segments: segments,
            callee_span,
            args,
            flavor,
            bang: Some(bang_span),
            meta: Meta::new(Span::new(start, end_pos)),
            ext: id,
        })
    }

    /// Parse a user elaborator bang-call form.
    fn bang_call_expr(&mut self) -> Result<Expr, Error> {
        let start_span = self.peek_span();
        let start = start_span.start;
        let kind_tok = self.advance();
        let TokenKind::Ident(name) = &kind_tok.kind else {
            unreachable!("expr_primary dispatched with Ident in lookahead");
        };
        let name = name.clone();
        Self::validate_value_name(&name, kind_tok.span)?;
        let bang_span = self.expect_bang()?;
        let explicit_list = self.peek_left_call_splice_flavor().is_none();
        let list_start = self.peek_span().start;
        let (args, mut end) = if explicit_list {
            self.call_arg_list(None)?
        } else {
            (Vec::new(), bang_span.end)
        };
        if let Some(flavor) = self.peek_left_call_splice_flavor() {
            if explicit_list && args.is_empty() {
                return Err(empty_ufcs_arg_list_error(Span::new(list_start, end)));
            }
            self.advance(); // consume `.<` / `.<<`
            let inserted = self.expr_tight_call_arg()?;
            end = inserted.span().end;
            let id = self.fresh_node_id();
            return Ok(Expr::Ufcs {
                occurrence: Default::default(),
                receiver: Box::new(inserted),
                callee_segments: vec![PathSegment::new(name, kind_tok.span)],
                callee_span: kind_tok.span,
                args,
                flavor,
                bang: Some(bang_span),
                meta: Meta::new(Span::new(start, end)),
                ext: id,
            });
        }
        self.user_elaborator_expr_from_args(name, args, start, end)
    }

    fn user_elaborator_expr_from_args(
        &mut self,
        name: String,
        args: Vec<CallArg>,
        start: u32,
        end: u32,
    ) -> Result<Expr, Error> {
        let id = self.fresh_node_id();
        Ok(Expr::UserElaborator {
            occurrence: Default::default(),
            name,
            args,
            form: crate::ast::UserElaboratorCallForm::Ordinary,
            meta: Meta::new(Span::new(start, end)),
            ext: id,
        })
    }

    fn let_value_binder(
        &mut self,
    ) -> Result<(String, Span, Option<Type>, Option<ParamPattern>), Error> {
        Ok(if matches!(self.peek(), Some(TokenKind::LParen)) {
            let pat = self.param_pattern_tuple(false)?;
            self.reject_outer_pattern_annotation()?;
            if let Some((name, name_span, ty)) = Self::unary_bind_from_pattern(&pat) {
                (name, name_span, ty, None)
            } else {
                let pat_span = pat.span;
                let synth = self.synth_pattern_param_name();
                (synth, pat_span, None, Some(pat))
            }
        } else {
            // Wildcard `_` as the bound name is admitted; it lexes
            // as a Slot1 token. Every other name is validated
            // (letterless names are rejected).
            let (n, s) = if matches!(self.peek(), Some(TokenKind::Slot1)) {
                let span = self.peek_span();
                self.advance();
                ("_".to_owned(), span)
            } else {
                self.expect_binder()?
            };
            if n != "_" {
                Self::validate_value_name(&n, s)?;
            }
            (n, s, None, None)
        })
    }

    /// The optional trailing `(Type)` of a `LiteralCall` — the
    /// explicit type annotation on a literal (`100(I32)`, `"hi"(Str)`,
    /// `.t(Bool)`). A literal is never a callee, so `(` immediately
    /// after a literal form unambiguously opens the annotation, not a
    /// call.
    fn parse_literal_annotation(&mut self) -> Result<Option<Type>, Error> {
        if matches!(self.peek(), Some(TokenKind::LParen)) {
            self.expect_kind(TokenKind::LParen, "`(` opening a literal's type annotation")?;
            let ty = self.type_expr()?;
            self.expect_kind(TokenKind::RParen, "`)` closing a literal's type annotation")?;
            Ok(Some(ty))
        } else {
            Ok(None)
        }
    }

    fn with_block_slot_boundary<T>(
        &mut self,
        explicit: bool,
        parse: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let saved = self.block_stop_depth;
        if explicit {
            self.block_stop_depth = None;
        }
        let result = parse(self);
        self.block_stop_depth = saved;
        result
    }

    fn block_prefix_value(&self, value: Expr) -> Result<Expr, Error> {
        let mut head = &value;
        while let Expr::Call { callee, .. } = head {
            head = callee;
        }
        let path = match head {
            Expr::Path { segments, .. } => Some(segments),
            _ => None,
        };
        if path
            .and_then(|segments| segments.last())
            .is_some_and(|segment| crate::naming::starts_like_type_name(&segment.name))
        {
            return Err(self.err(
                value.span(),
                "block prefix requires a value argument; type arguments are inferred",
            ));
        }
        Ok(value)
    }

    fn block_call_starts_here(&self) -> bool {
        if self.block_stop_depth == Some(self.cursor.depth()) {
            return false;
        }
        let Some(TokenKind::Ident(name)) = self.peek() else {
            return false;
        };
        if Self::validate_value_name(name, self.peek_span()).is_err() {
            return false;
        }
        use crate::pass::tree_skeleton::{GroupKind, SkeletonNode};
        let [
            SkeletonNode::Leaf(head),
            SkeletonNode::Leaf(bang),
            next,
            rest @ ..,
        ] = self.cursor.remaining_current_frame()
        else {
            return false;
        };
        if head.span != self.peek_span() || !Self::adjacent_bang_tokens(head, bang) {
            return false;
        }
        // A fused suffix is still owned by ordinary bang/operator parsing;
        // an operator-leading block prefix requires explicit parentheses.
        if !matches!(&bang.kind, TokenKind::SymbolRun(run) if run == "!") {
            return false;
        }
        match next {
            SkeletonNode::Group {
                kind: GroupKind::Brace,
                ..
            } => true,
            SkeletonNode::Group {
                kind: GroupKind::Paren,
                close: Some(_),
                recovered: false,
                ..
            } => matches!(
                rest.first(),
                Some(SkeletonNode::Group {
                    kind: GroupKind::Brace,
                    ..
                })
            ),
            SkeletonNode::Leaf(Token {
                kind:
                    TokenKind::Ident(_)
                    | TokenKind::IntLit { .. }
                    | TokenKind::FloatLit { .. }
                    | TokenKind::StrLit(_)
                    | TokenKind::BoolLit(_),
                ..
            }) => true,
            SkeletonNode::Leaf(Token {
                kind: TokenKind::SymbolRun(run),
                ..
            }) if run == "." => {
                matches!(rest.first(), Some(SkeletonNode::Leaf(Token {
                    kind: TokenKind::Ident(name), ..
                })) if name == "t" || name == "f")
                    && !matches!(rest.get(1), Some(SkeletonNode::Leaf(Token {
                        kind: TokenKind::SymbolRun(run), ..
                    })) if run == ".")
            }
            _ => false,
        }
    }

    fn record_block_cursor(
        &mut self,
        name: &str,
        head: Span,
        region: BlockCursorRegion,
        labels: &[crate::ast::NeutralBlock],
        interval: Span,
    ) {
        let Some(tooling) = &mut self.tooling else {
            return;
        };
        let Some(cursor) = tooling.cursor else { return };
        if cursor < interval.start
            || cursor > interval.end
            || tooling
                .facts
                .block_cursor
                .as_ref()
                .is_some_and(|fact| fact.head.span.start > head.start)
        {
            return;
        }
        tooling.facts.block_cursor = Some(std::sync::Arc::new(BlockCursor {
            head: PathSegment::new(name.to_owned(), head),
            region,
            labels: labels
                .iter()
                .map(|block| block.label.as_ref().map(|label| label.as_str().to_owned()))
                .collect(),
        }));
    }

    fn block_body_cursor(&mut self, name: &str, head: Span, blocks: &[crate::ast::NeutralBlock]) {
        use crate::pass::tree_skeleton::SkeletonNode;
        if self.tooling.is_none() {
            return;
        }
        if let Some(SkeletonNode::Group { open, close, .. }) =
            self.cursor.remaining_current_frame().first()
        {
            let interval = Span::new(
                open.span.start,
                close.as_ref().map_or_else(
                    || {
                        self.cursor
                            .current_group_close_span()
                            .map_or(self.eof, |span| span.start)
                    },
                    |token| token.span.end,
                ),
            );
            self.record_block_cursor(
                name,
                head,
                BlockCursorRegion::Body(blocks.len()),
                blocks,
                interval,
            );
        }
    }

    fn block_label_cursor(&mut self, name: &str, head: Span, blocks: &[crate::ast::NeutralBlock]) {
        if !matches!(
            self.peek(),
            None | Some(TokenKind::Ident(_) | TokenKind::RBrace | TokenKind::Semicolon)
        ) {
            return;
        }
        self.cursor_choices(CursorSlot::BlockLabel, &[]);
        if let Some(cursor) = self
            .tooling
            .as_ref()
            .and_then(|tooling| tooling.facts.cursor.as_ref())
            && cursor.slot == CursorSlot::BlockLabel
            && blocks
                .last()
                .is_some_and(|block| block.close.end <= cursor.atom.prefix.end)
        {
            self.record_block_cursor(
                name,
                head,
                BlockCursorRegion::Label(blocks.len()),
                blocks,
                cursor.span,
            );
        }
    }

    fn block_call(&mut self) -> Result<Expr, Error> {
        let start = self.peek_span().start;
        let (name, head_span) = self.expect_ident()?;
        self.expect_bang()?;
        self.record_source_name(head_span, SourceNameRole::FunctionReference);
        let mut prefix = Vec::new();
        let prefix_explicit = matches!(self.peek(), Some(TokenKind::LParen));
        if prefix_explicit {
            let list_start = self.peek_span().start;
            let mut list_leading = self.peek_leading_trivia();
            let empty_trivia = match self.cursor.remaining_current_frame().first() {
                Some(crate::pass::tree_skeleton::SkeletonNode::Group {
                    children,
                    close: Some(close),
                    ..
                }) if children.iter().all(|node| {
                    matches!(
                        node,
                        crate::pass::tree_skeleton::SkeletonNode::Leaf(Token {
                            kind: TokenKind::Comma,
                            ..
                        })
                    )
                }) =>
                {
                    let mut trivia = Vec::new();
                    for child in children {
                        if let crate::pass::tree_skeleton::SkeletonNode::Leaf(token) = child {
                            trivia.extend(token.leading_trivia.clone());
                        }
                    }
                    trivia.extend(close.leading_trivia.clone());
                    trivia
                }
                _ => Vec::new(),
            };
            let result = self.call_arg_list(None);
            self.record_block_cursor(
                &name,
                head_span,
                BlockCursorRegion::Prefix,
                &[],
                Span::new(head_span.end + 1, self.peek_span().start),
            );
            let (args, end) = result?;
            if args.is_empty() {
                list_leading.extend(empty_trivia);
                prefix.push(Expr::Unit {
                    occurrence: (),
                    meta: Meta::new(Span::new(list_start, end)),
                });
            }
            for arg in args {
                match arg {
                    CallArg::Value(value) => prefix.push(self.block_prefix_value(value)?),
                    CallArg::Type(ty) => {
                        return Err(self.err(
                            ty.span(),
                            "block prefix requires a value argument; type arguments are inferred",
                        ));
                    }
                }
            }
            if let Some(first) = prefix.first_mut() {
                inject_leading_trivia(first, list_leading);
            }
        } else if !matches!(self.peek(), Some(TokenKind::LBrace)) {
            let leading = self.peek_leading_trivia();
            let saved = self.block_stop_depth;
            self.block_stop_depth = Some(self.cursor.depth());
            let result = self.expr();
            self.block_stop_depth = saved;
            self.record_block_cursor(
                &name,
                head_span,
                BlockCursorRegion::Prefix,
                &[],
                Span::new(head_span.end + 1, self.peek_span().start),
            );
            let mut value = self.block_prefix_value(result?)?;
            inject_leading_trivia(&mut value, leading);
            prefix.push(value);
        }
        self.block_body_cursor(&name, head_span, &[]);
        let mut blocks = vec![self.neutral_block(None)?];
        self.block_label_cursor(&name, head_span, &blocks);
        while let Some(TokenKind::Ident(_)) = self.peek() {
            let label_leading = self.peek_leading_trivia();
            let (label, span) = self.expect_ident()?;
            Self::validate_value_name(&label, span)?;
            self.record_keyword(span, KeywordRole::Control);
            let label = Some(PathSegment::new(label, span));
            if matches!(self.peek(), Some(TokenKind::LBrace)) {
                self.block_body_cursor(&name, head_span, &blocks);
                let mut body = self.neutral_block(label)?;
                body.leading.splice(0..0, label_leading);
                blocks.push(body);
                self.block_label_cursor(&name, head_span, &blocks);
            } else if self.block_call_starts_here() {
                let child_leading = self.peek_leading_trivia();
                let mut child = self.block_call()?;
                inject_leading_trivia(&mut child, child_leading);
                let child_span = child.span();
                blocks.push(crate::ast::NeutralBlock {
                    label,
                    elided: true,
                    items: vec![crate::ast::NeutralItem::Expression {
                        value: child,
                        semicolon: None,
                    }],
                    open: span,
                    close: child_span,
                    separators: Vec::new(),
                    leading: label_leading,
                    trailing: Vec::new(),
                });
                break;
            } else {
                return Err(self.err_here("trailing label requires a block or block call"));
            }
        }
        let end = blocks.last().unwrap().close.end;
        Ok(Expr::BlockCall {
            occurrence: (),
            id: self.fresh_node_id(),
            head: PathSegment::new(name, head_span),
            prefix,
            prefix_explicit,
            blocks,
            meta: Meta::new(Span::new(start, end)),
            ext: (),
        })
    }

    fn neutral_block(
        &mut self,
        label: Option<PathSegment>,
    ) -> Result<crate::ast::NeutralBlock, Error> {
        let leading = self.peek_leading_trivia();
        let open = self.expect_kind(TokenKind::LBrace, "`{` starting neutral block")?;
        let scope = self.begin_body_scope(None);
        let result = self.neutral_block_items(label, leading, open);
        self.finish_body_scope(
            scope,
            result
                .as_ref()
                .ok()
                .map(|block| Span::new(block.open.end, block.close.start)),
        );
        result
    }

    fn neutral_block_items(
        &mut self,
        label: Option<PathSegment>,
        leading: Vec<Trivia>,
        open: Span,
    ) -> Result<crate::ast::NeutralBlock, Error> {
        let mut items = Vec::new();
        let mut separators = Vec::new();
        let mut pending = Vec::new();
        loop {
            while matches!(self.peek(), Some(TokenKind::Semicolon)) {
                let leading = self.peek_leading_trivia();
                let span = self.peek_span();
                if let Some(crate::ast::NeutralItem::Expression { semicolon, .. }) =
                    items.last_mut()
                {
                    semicolon.get_or_insert(span);
                }
                separators.push(crate::ast::NeutralSeparator {
                    span,
                    leading: leading.clone(),
                    after_item: items.len().checked_sub(1),
                });
                pending.extend(leading);
                self.advance();
            }
            // A new item may be inserted only at a separated boundary. The
            // final expression needs no trailing separator, but inserting a
            // second expression after it does.
            if !matches!(
                items.last(),
                Some(crate::ast::NeutralItem::Expression {
                    semicolon: None,
                    ..
                })
            ) {
                self.cursor_choices(CursorSlot::Value, &["rec", "let"]);
            }
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                pending.extend(self.peek_leading_trivia());
                let close = self.expect_kind(TokenKind::RBrace, "`}` closing neutral block")?;
                return Ok(crate::ast::NeutralBlock {
                    label,
                    elided: false,
                    items,
                    open,
                    close,
                    separators,
                    leading,
                    trailing: pending,
                });
            }
            if self.let_statement_starts_here() {
                let (item, mut separator) =
                    self.neutral_let_statement(std::mem::take(&mut pending))?;
                separator.after_item = Some(items.len());
                separators.push(separator);
                items.push(item);
                continue;
            }
            pending.extend(self.peek_leading_trivia());
            let mut value = self.expr()?;
            inject_leading_trivia(&mut value, std::mem::take(&mut pending));
            items.push(crate::ast::NeutralItem::Expression {
                value,
                semicolon: None,
            });
            if !matches!(self.peek(), Some(TokenKind::Semicolon | TokenKind::RBrace)) {
                return Err(self.err_here("block items require `;` or closing `}`"));
            }
        }
    }

    fn neutral_let_statement(
        &mut self,
        mut pending: Vec<Trivia>,
    ) -> Result<(crate::ast::NeutralItem, crate::ast::NeutralSeparator), Error> {
        enum Binder {
            Value(String, Span, Option<Type>, Option<ParamPattern>),
            Row(Vec<RowLetEntry>),
            Existential(Vec<TypeParam>, String, Span, Option<ParamPattern>),
        }
        let (leading, start) = self.consume_let_statement_intro()?;
        pending.extend(leading);
        let binder = if matches!(self.peek(), Some(TokenKind::LParen))
            && matches!(self.peek_at(1), Some(TokenKind::LBrace))
        {
            self.advance();
            let (entries, _) = self.row_let_entry_list()?;
            self.expect_kind(TokenKind::RParen, "`)` after the row pattern")?;
            Binder::Row(entries)
        } else if matches!(self.peek(), Some(TokenKind::LParen))
            && matches!(self.peek_at(1), Some(TokenKind::SymbolRun(run)) if run == "<")
        {
            self.advance();
            let (types, name, span, pattern) = self.let_unpack_pattern()?;
            Binder::Existential(types, name, span, pattern)
        } else {
            let (name, span, ty, pattern) = self.let_value_binder()?;
            Binder::Value(name, span, ty, pattern)
        };
        let bind = self.at_sym("<-");
        if bind && !matches!(binder, Binder::Value(..)) {
            return Err(
                self.err_here("row and existential-opening patterns require a pure `=` let")
            );
        }
        let mut value_leading = self.peek_leading_trivia();
        self.expect_sym(
            if bind { "<-" } else { "=" },
            "`=` or `<-` after neutral binder",
        )?;
        value_leading.extend(self.peek_leading_trivia());
        let mut value = self.expr()?;
        inject_leading_trivia(&mut value, value_leading);
        let separator_leading = self.peek_leading_trivia();
        let span = self.expect_kind(TokenKind::Semicolon, "`;` after neutral let")?;
        let meta = Meta {
            span: Span::new(start, span.end),
            leading_trivia: pending,
            trailing_trivia: separator_leading.clone(),
        };
        let item = match binder {
            Binder::Value(name, name_span, ty, pattern) => {
                self.record_scope_prefix(|| ScopeSyntax::Binding {
                    name: Some((name.clone(), name_span)),
                    pattern: pattern.clone(),
                    type_params: Vec::new(),
                });
                crate::ast::NeutralItem::Binding {
                    name,
                    name_span,
                    ty,
                    pattern,
                    value,
                    bind,
                    meta,
                }
            }
            Binder::Row(entries) => {
                self.record_scope_prefix(|| ScopeSyntax::RowLet(entries.clone()));
                crate::ast::NeutralItem::RowBinding {
                    entries,
                    value,
                    meta,
                }
            }
            Binder::Existential(type_params, name, name_span, pattern) => {
                self.record_scope_prefix(|| ScopeSyntax::Binding {
                    name: Some((name.clone(), name_span)),
                    pattern: pattern.clone(),
                    type_params: type_params.clone(),
                });
                crate::ast::NeutralItem::ExistentialBinding {
                    type_params,
                    name,
                    name_span,
                    pattern,
                    value,
                    meta,
                }
            }
        };
        Ok((
            item,
            crate::ast::NeutralSeparator {
                span,
                leading: separator_leading,
                after_item: None,
            },
        ))
    }

    fn expr_primary(&mut self) -> Result<Expr, Error> {
        if self.block_call_starts_here() {
            return self.block_call();
        }
        self.cursor_choices(CursorSlot::Value, &["rec"]);
        if let Some(e) = self.try_variadic_literal()? {
            return Ok(e);
        }
        if self.at_leading_dot_prefix() {
            return self.leading_dot_expr();
        }
        // First, see if a prefix-shaped user-defined operator
        // applies (`! a`, `- a`, `~ a`, etc.). The prefix scope
        // is keyed separately from non-prefix bindings, so a
        // token like `-` can have both a `op - __ { impl neg; };`
        // (prefix, used here) and a `op _ - __ { impl sub; };`
        // (binary, used post-LHS) registered at once.
        if let Some(e) = self.try_prefix_operator()? {
            return Ok(e);
        }
        // A direct module/item FQN starts at this expression-primary
        // position. Value expressions admit only local/imported names and
        // dotted module aliases, so reject the exact path here instead of
        // waiting for an enclosing block/EOF parser to notice its leftover
        // tail. The operator registry gets first claim on the slash token:
        // with an explicitly in-scope non-prefix `/` binding, the same token
        // sequence remains an operator expression with a dotted RHS.
        if let Some(error) = self.unclaimed_direct_value_fqn_error() {
            return Err(error);
        }
        let rec_call = self.peek_ident_named("rec") && self.rec_call_starts_here();
        match self.peek() {
            Some(TokenKind::StrLit(_)) => {
                let (value, span) = self.read_str_literal();
                let annotation = self.parse_literal_annotation()?;
                Ok(Expr::StrLit {
                    occurrence: Default::default(),
                    value,
                    annotation,
                    meta: Meta::new(span),
                })
            }
            Some(TokenKind::IntLit { .. }) => {
                let tok = self.advance();
                let TokenKind::IntLit { digits } = tok.kind else {
                    unreachable!()
                };
                let annotation = self.parse_literal_annotation()?;
                Ok(Expr::IntLit {
                    occurrence: Default::default(),
                    digits,
                    annotation,
                    meta: Meta::new(tok.span),
                })
            }
            Some(TokenKind::FloatLit { .. }) => {
                let tok = self.advance();
                let TokenKind::FloatLit { digits } = tok.kind else {
                    unreachable!()
                };
                let annotation = self.parse_literal_annotation()?;
                Ok(Expr::FloatLit {
                    occurrence: Default::default(),
                    digits,
                    annotation,
                    meta: Meta::new(tok.span),
                })
            }
            Some(TokenKind::BoolLit(_)) => {
                let tok = self.advance();
                let TokenKind::BoolLit(value) = tok.kind else {
                    unreachable!()
                };
                let annotation = self.parse_literal_annotation()?;
                Ok(Expr::BoolLit {
                    occurrence: Default::default(),
                    value,
                    annotation,
                    meta: Meta::new(tok.span),
                })
            }
            // Expression-leading words are contextual keywords — they
            // arrive as Ident tokens and we route them by text. The
            // generic `Ident(_) => expr_path` arm handles every other
            // identifier (paths, var references, etc.).
            // `let` only opens a statement when the surrounding block
            // parser has already recognized a full let-statement
            // shape. In expression position it is an ordinary value
            // identifier, so calls like `let()` remain admissible.
            Some(TokenKind::Ident(s)) if s == "rec" && rec_call => self.rec_call_expr(),
            // Bang-call form: a `name!(args...)` user-elaborator call
            // (`iso!(e)`, `fit!(e, T)`, `match!(...)`, `my_elaborator!(e, T)`
            // — all imported elaborators). The
            // identifier and `!` must be adjacent; `name !` remains an
            // ordinary path followed by a separate token and reports at
            // the stray `!` site.
            Some(TokenKind::Ident(_)) if self.peek_adjacent_bang_after_ident() => {
                self.bang_call_expr()
            }
            Some(TokenKind::Ident(_)) => self.expr_path(),
            Some(TokenKind::LParen) => self.expr_paren(),
            Some(TokenKind::LBrace) => self.label_value_expr(),
            _ => Err(self.err_here("expected expression")),
        }
    }

    fn rec_call_expr(&mut self) -> Result<Expr, Error> {
        let start = self.peek_span().start;
        self.expect_ident_role("rec", KeywordRole::Declaration)?;
        let mut annotation_error = None;
        let parsed = (|| {
            let modes = if matches!(self.peek(), Some(TokenKind::LParen)) {
                self.rec_call_modes(&mut annotation_error)?
            } else {
                Vec::new()
            };
            self.cursor_choices(CursorSlot::RecursiveCallee, &[]);
            let (name, name_span) = self.expect_ident()?;
            Self::validate_value_name(&name, name_span)?;
            if self.at_sym(".") {
                return Err(self.err_here(
                    "`rec` calls target a member of the current recursion group; \
                     write `rec name(...)`, not a dotted path",
                ));
            }
            let (args, end) = if matches!(self.peek(), Some(TokenKind::LParen)) {
                self.call_arg_list(None)?
            } else {
                return Err(self.err_here("expected argument list after `rec` callee"));
            };
            Ok((modes, PathSegment::new(name, name_span), args, end))
        })();
        if let Some(error) = annotation_error {
            return Err(error);
        }
        let (modes, callee, args, end) = parsed?;
        Ok(Expr::RecCall {
            occurrence: Default::default(),
            modes,
            callee,
            args,
            meta: Meta::new(Span::new(start, end)),
            ext: (),
        })
    }

    fn rec_call_modes(
        &mut self,
        annotation_error: &mut Option<Error>,
    ) -> Result<Vec<RecCallMode>, Error> {
        self.expect_kind(TokenKind::LParen, "`(` opening `rec` call annotations")?;
        let annotation_depth = self.cursor.depth();
        self.cursor_choices_from(
            CursorSlot::RecursiveAnnotation,
            [RecCallMode::Poly, RecCallMode::Cont]
                .into_iter()
                .map(RecCallMode::as_str),
        );
        self.skip_commas();
        self.cursor_choices_from(
            CursorSlot::RecursiveAnnotation,
            [RecCallMode::Poly, RecCallMode::Cont]
                .into_iter()
                .map(RecCallMode::as_str),
        );
        if matches!(self.peek(), Some(TokenKind::RParen)) {
            return Err(self.err_here("`rec` call annotation list cannot be empty"));
        }
        let mut modes = Vec::new();
        loop {
            self.cursor_choices_from(
                CursorSlot::RecursiveAnnotation,
                [RecCallMode::Poly, RecCallMode::Cont]
                    .into_iter()
                    .filter(|mode| !modes.contains(mode))
                    .map(RecCallMode::as_str),
            );
            let parsed = (|| {
                let (name, span) = self.expect_ident()?;
                if let Some(mode) = RecCallMode::from_annotation(&name) {
                    if modes.contains(&mode) {
                        annotation_error.get_or_insert_with(|| {
                            Error::parse(
                                span,
                                format!("duplicate `rec` call annotation `{}`", mode.as_str()),
                            )
                        });
                    }
                    self.record_keyword(span, KeywordRole::Control);
                    modes.push(mode);
                } else {
                    annotation_error.get_or_insert_with(|| {
                        Error::parse(span, format!("unknown `rec` call annotation `{name}`"))
                            .with_help(
                                "available annotations are `poly`, `cont`, and reserved `escape`",
                            )
                    });
                }
                if !matches!(self.peek(), Some(TokenKind::Comma | TokenKind::RParen)) {
                    return Err(self.err_here("expected `)` closing `rec` call annotations"));
                }
                Ok(())
            })();
            if let Err(error) = parsed {
                annotation_error.get_or_insert(error);
                // Only the containing list's separator starts another mode;
                // nested items and unseparated suffixes own no annotation roles.
                let mut varop_closers = Vec::new();
                while self.peek().is_some() {
                    if self.cursor.depth() == annotation_depth {
                        if matches!(self.peek(), Some(TokenKind::RParen))
                            || (varop_closers.is_empty()
                                && matches!(self.peek(), Some(TokenKind::Comma)))
                        {
                            break;
                        }
                        if let Some(TokenKind::SymbolRun(run)) = self.peek() {
                            if varop_closers.last() == Some(run) {
                                varop_closers.pop();
                            } else if let Some(close) = mirrored_varop_close(run) {
                                varop_closers.push(close);
                            }
                        }
                    }
                    self.advance();
                }
            }
            if self.skip_commas() {
                self.cursor_choices_from(
                    CursorSlot::RecursiveAnnotation,
                    [RecCallMode::Poly, RecCallMode::Cont]
                        .into_iter()
                        .filter(|mode| !modes.contains(mode))
                        .map(RecCallMode::as_str),
                );
                if matches!(self.peek(), Some(TokenKind::RParen)) {
                    break;
                }
                continue;
            }
            break;
        }
        self.expect_kind(TokenKind::RParen, "`)` closing `rec` call annotations")?;
        Ok(modes)
    }

    fn leading_dot_expr(&mut self) -> Result<Expr, Error> {
        let dot_span = self.expect_leading_dot()?;
        let starts_polymorphic_lambda = self.peek_starts_polymorphic_lambda_signature();
        match self.peek() {
            Some(TokenKind::Ident(stem)) if matches!(self.peek_at(1), Some(TokenKind::SymbolRun(run)) if run == ".") =>
            {
                let stem = stem.clone();
                let stem_span = self.advance().span;
                Self::validate_value_name(&stem, stem_span)?;
                if !stem
                    .bytes()
                    .last()
                    .is_some_and(|byte| byte.is_ascii_alphabetic())
                {
                    return Err(self.err(stem_span, "a placeholder stem must end in a letter"));
                }
                let close = self.expect_sym(".", "`.` after the placeholder stem")?;
                if dot_span.end != stem_span.start || stem_span.end != close.start {
                    return Err(self.err(
                        Span::new(dot_span.start, close.end),
                        "the dots and stem of a placeholder intro must be adjacent",
                    ));
                }
                self.record_source_name(stem_span, SourceNameRole::Parameter);
                self.fn_placeholder_expr(dot_span.start, PathSegment::new(stem, stem_span))
            }
            Some(TokenKind::Ident(s)) if s == "t" || s == "f" => {
                let tok = self.advance();
                let TokenKind::Ident(name) = tok.kind else {
                    unreachable!("just matched Ident")
                };
                let annotation = self.parse_literal_annotation()?;
                Ok(Expr::BoolLit {
                    occurrence: Default::default(),
                    value: name == "t",
                    annotation,
                    meta: Meta::new(Span::new(dot_span.start, tok.span.end)),
                })
            }
            Some(TokenKind::LParen) => self.fn_expr_after_intro(dot_span.start),
            Some(_) if starts_polymorphic_lambda => self.fn_expr_after_intro(dot_span.start),
            _ => Err(self.err_here("expected `.t`, `.f`, `.(...) { ... }`, or `.stem. { ... }`")),
        }
    }

    fn peek_starts_polymorphic_lambda_signature(&mut self) -> bool {
        if !self.peek_starts_type_param() {
            return false;
        }
        let saved_cursor = self.cursor.clone();
        let checkpoint = self.tooling_checkpoint();
        let mut complete = false;
        let mut failed = false;
        while self.peek_starts_type_param() {
            if self.type_param_group().is_err() {
                failed = true;
                break;
            }
            complete = true;
        }
        let selected = !failed && complete && matches!(self.peek(), Some(TokenKind::LParen));
        if selected {
            self.advance();
        }
        let facts = self.take_tooling_suffix(&checkpoint);
        self.cursor = saved_cursor;
        if !selected && facts.as_ref().is_some_and(|facts| facts.unclosed) {
            self.restore_tooling_suffix(&checkpoint, facts);
        }
        selected
    }

    fn expr_path(&mut self) -> Result<Expr, Error> {
        let (segments, span_end) = self.value_path_segments()?;
        let start = segments
            .first()
            .map(|segment| segment.span.start)
            .unwrap_or(span_end);
        Ok(Expr::Path {
            occurrence: Default::default(),
            segments,
            meta: Meta::new(Span::new(start, span_end)),
            ext: (),
        })
    }

    fn value_path_segments(&mut self) -> Result<(Vec<PathSegment>, u32), Error> {
        self.cursor_choices(CursorSlot::Value, &[]);
        let (first, first_span) = self.expect_ident()?;
        Self::validate_path_segment_name(&first, first_span)?;
        let mut segments = vec![PathSegment::new(first, first_span)];
        let mut span_end = first_span.end;
        while self.at_sym(".") && !matches!(self.peek_at(1), Some(TokenKind::LBrace)) {
            let separator = self.advance().span;
            self.cursor_path_choices(
                CursorSlot::Value,
                &segments,
                separator,
                PathSeparator::Dot,
                0,
            );
            let (next, next_span) = self.expect_ident()?;
            Self::validate_path_segment_name(&next, next_span)?;
            segments.push(PathSegment::new(next, next_span));
            span_end = next_span.end;
        }
        Ok((segments, span_end))
    }

    /// Parse the callable-target syntax shared by `op`, `varop`, and `elab`.
    /// A target is an ordinary lexical value path: a local or selectively
    /// imported name, or a dotted path through a qualified import/newtype.
    fn parse_lexical_callable_path(
        &mut self,
        expectation: &str,
        expression_help: &str,
    ) -> Result<(crate::ast::LexicalCallablePath, u32), Error> {
        if let Some(span) = self.direct_value_fqn_ahead() {
            return Err(self
                .err(
                    span,
                    "slash-qualified item paths are not lexical callable targets",
                )
                .with_help(
                    "import the item or its module, then use the local name or dotted module alias",
                ));
        }
        if !matches!(self.peek(), Some(TokenKind::Ident(_))) {
            return Err(self.err_here(expectation).with_help(expression_help));
        }
        let (segments, end) = self.value_path_segments()?;
        Self::validate_reference_value_path_leaf(&segments)?;
        Ok((crate::ast::LexicalCallablePath::new(segments), end))
    }

    /// Return the span of an exact `module/path.item` prefix at the current
    /// value-path position. The lookahead is deliberately anchored at the
    /// leading identifier: a slash after a completed expression such as
    /// `left() / right.item` is operator-shaped syntax, not a direct FQN.
    fn direct_value_fqn_ahead(&self) -> Option<Span> {
        let first = self.peek_token_at(0)?;
        if !matches!(first.kind, TokenKind::Ident(_)) {
            return None;
        }

        let mut offset = 1usize;
        let mut saw_slash = false;
        while self.peek_at(offset).is_some_and(|kind| kind.is_sym("/"))
            && matches!(self.peek_at(offset + 1), Some(TokenKind::Ident(_)))
        {
            saw_slash = true;
            offset += 2;
        }
        if !saw_slash || !self.peek_at(offset).is_some_and(|kind| kind.is_sym(".")) {
            return None;
        }
        let item = self.peek_token_at(offset + 1)?;
        if !matches!(item.kind, TokenKind::Ident(_)) {
            return None;
        }
        Some(Span::new(first.span.start, item.span.end))
    }

    /// Diagnose a direct value FQN only when no in-scope non-prefix operator
    /// claims its leading slash. Callers invoke this at positions where an
    /// ordinary value path can begin; a slash following some other completed
    /// expression is therefore outside this lookahead by construction.
    fn unclaimed_direct_value_fqn_error(&self) -> Option<Error> {
        let error = self.direct_value_fqn_error()?;
        if self
            .operator_scope
            .find_non_prefix_at_offset(self, 1)
            .is_some()
        {
            return None;
        }
        Some(error)
    }

    fn direct_value_fqn_error(&self) -> Option<Error> {
        let span = self.direct_value_fqn_ahead()?;
        Some(self.err(
            span,
            "slash-qualified item paths are not value expressions; import the item or its \
             module and use the local name or module alias",
        ))
    }

    fn expr_paren(&mut self) -> Result<Expr, Error> {
        let start = self.peek_span().start;
        self.expect_kind(TokenKind::LParen, "`(`")?;
        // Trivia between `(` and the first element (or its leading
        // comma); merged forward across commas, mirroring
        // `call_arg_list`.
        let mut pending_trivia = self.peek_leading_trivia();
        let leading_comma = self.skip_commas();
        if leading_comma {
            pending_trivia.extend(self.peek_leading_trivia());
        }
        self.cursor_choices(CursorSlot::Value, &["rec"]);
        if matches!(self.peek(), Some(TokenKind::RParen)) {
            let end = self.peek_span().end;
            self.advance();
            return Ok(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(Span::new(start, end)),
            });
        }
        let first = self.expr()?;
        let first_trivia = std::mem::take(&mut pending_trivia);
        pending_trivia = self.peek_leading_trivia();
        let saw_comma_after_first = self.skip_commas();
        if saw_comma_after_first {
            pending_trivia.extend(self.peek_leading_trivia());
            self.cursor_choices(CursorSlot::Value, &["rec"]);
        }
        // `(e)` — grouping; reduces to the inner expression. The
        // same collapse applies when comma-runs frame one item:
        // `(, e)`, `(e,)`, and `(,,, e ,,,)` are still grouping,
        // not unary tuples.
        if matches!(self.peek(), Some(TokenKind::RParen))
            && !leading_comma
            && !saw_comma_after_first
        {
            // A `(e /* c */)` grouping: the comment between `e` and the
            // closing `)` lives in `pending_trivia`. Carry it on `e`'s
            // trailing slot so it survives the grouping collapse.
            let mut first = first;
            let closing_trivia = std::mem::take(&mut pending_trivia);
            set_trailing_trivia(first.meta_mut(), closing_trivia);
            self.advance();
            return Ok(first);
        }
        // Otherwise we're in a tuple. Collect items until we hit `)`.
        // The first item's pre-token trivia rides on its own
        // `meta.leading_trivia` so the layout matches the parallel
        // arrays-per-element invariant the pretty-printer expects.
        let mut first = first;
        inject_leading_trivia(&mut first, first_trivia);
        let mut items = vec![first];
        while !matches!(self.peek(), Some(TokenKind::RParen)) {
            // We're here because we saw at least one comma — either
            // the leading run, or after the previous item.
            let leading = std::mem::take(&mut pending_trivia);
            let mut item = self.expr()?;
            inject_leading_trivia(&mut item, leading);
            items.push(item);
            pending_trivia = self.peek_leading_trivia();
            let mut saw_comma = false;
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                saw_comma = true;
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if matches!(self.peek(), Some(TokenKind::RParen)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `)`"));
            }
        }
        // Last tuple item's trailing / dangling comment run before the
        // `)` — stash on the last item's trailing slot. A grouping
        // `(e)` collapses to `e` below; its closing-paren trivia rides
        // along on `e`'s trailing slot so a `(x /* c */)` round-trips.
        let closing_trivia = std::mem::take(&mut pending_trivia);
        if let Some(last) = items.last_mut() {
            set_trailing_trivia(last.meta_mut(), closing_trivia);
        }
        let end = self.expect_kind(TokenKind::RParen, "`)`")?.end;
        if items.len() < 2 {
            return Ok(items.pop().expect("single item exists"));
        }
        Ok(Expr::Tuple {
            occurrence: Default::default(),
            items,
            meta: Meta::new(Span::new(start, end)),
            ext: (),
        })
    }

    fn fn_expr_after_intro(&mut self, start: u32) -> Result<Expr, Error> {
        let owner = self.peek_span();
        let sig = self.fn_signature()?;
        // Optional `.> R` return-type annotation. Stored on the
        // node and verified by the typer against bidirectional
        // flow. `.> _` parses as `Type::Infer` and is treated
        // identically to no annotation (the typer infers the
        // return type from the body and the surrounding context).
        let ret_ty = if self.at_sym_prefix("->") {
            self.expect_fn_arrow()?;
            Some(self.with_type_parameters(
                owner,
                ScopeOwnerKind::Signature,
                sig.params.iter().filter_map(|param| match param {
                    SignatureParam::Type(param) => Some(param),
                    SignatureParam::Value(_) => None,
                }),
                Self::type_expr,
            )?)
        } else {
            None
        };
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let (body, end) = self.lambda_body(Some(&sig), None)?;
        Ok(Expr::FnExpr {
            occurrence: Default::default(),
            sig,
            ret_ty,
            body: Box::new(body),
            meta: Meta::new(Span::new(start, end)),
            caps: (),
        })
    }

    /// `{f = e, g = e'}` — label-value construction sugar. Each
    /// label's `f` must resolve to a label in scope; label-elab
    /// validates this against the per-module label table. The label
    /// may be qualified `m.f` for cross-module labels. The parser
    /// distinguishes declaration-position `{f: X}` from
    /// value-position `{f = e}` via the leading separator:
    /// `:` for label declarations, `=` for value sugar. An empty
    /// `{}` is the unit value.
    fn label_value_expr(&mut self) -> Result<Expr, Error> {
        let start = self.peek_span().start;
        self.expect_kind(TokenKind::LBrace, "`{`")?;
        let mut labels: Vec<LabelValueLabel> = Vec::new();
        // Pre-loop trivia: comments between `{` and the first label
        // (or its leading comma). Merged forward across commas, same
        // pattern as `call_arg_list`.
        let mut pending_trivia = self.peek_leading_trivia();
        while matches!(self.peek(), Some(TokenKind::Comma)) {
            self.advance();
            pending_trivia.extend(self.peek_leading_trivia());
        }
        while !matches!(self.peek(), Some(TokenKind::RBrace)) {
            let leading = std::mem::take(&mut pending_trivia);
            let mut label = self.label_value_label()?;
            inject_meta_trivia(&mut label.meta, leading);
            labels.push(label);
            pending_trivia = self.peek_leading_trivia();
            let mut saw_comma = false;
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                saw_comma = true;
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `}`"));
            }
        }
        // Last label's trailing / dangling comment run before the `}` —
        // stash on the last label's trailing slot so the formatter
        // keeps it across the round-trip.
        let closing_trivia = std::mem::take(&mut pending_trivia);
        if let Some(last) = labels.last_mut() {
            set_trailing_trivia(&mut last.meta, closing_trivia);
        }
        let end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        if labels.is_empty() {
            return Ok(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(Span::new(start, end)),
            });
        }
        let id = self.fresh_node_id();
        Ok(Expr::LabelValue {
            occurrence: Default::default(),
            labels,
            meta: Meta::new(Span::new(start, end)),
            ext: id,
        })
    }

    fn label_path_parts(&mut self) -> Result<LabelPathParts, Error> {
        let (first, first_span) = self.expect_ident()?;
        Self::validate_value_name(&first, first_span)?;
        let mut full_label = first.clone();
        let mut label_end = first_span.end;
        let mut last = first;
        let mut last_span = first_span;
        while self.at_sym(".") {
            self.advance();
            let (next, next_span) = self.expect_ident()?;
            Self::validate_value_name(&next, next_span)?;
            full_label.push('.');
            full_label.push_str(&next);
            label_end = next_span.end;
            last = next;
            last_span = next_span;
        }
        self.record_source_name(
            last_span,
            if first_span == last_span {
                SourceNameRole::LabelReference
            } else {
                SourceNameRole::QualifiedLabelReference
            },
        );
        Ok(LabelPathParts {
            full: full_label,
            full_span: Span::new(first_span.start, label_end),
            last,
            last_span,
        })
    }

    fn label_value_label(&mut self) -> Result<LabelValueLabel, Error> {
        let LabelPathParts {
            full: full_label,
            full_span: label_span,
            last,
            last_span,
        } = self.label_path_parts()?;
        let value = if self.at_sym("=") {
            let eq_span = self.expect_sym("=", "`=`")?;
            if matches!(
                self.peek(),
                Some(TokenKind::Comma) | Some(TokenKind::RBrace)
            ) {
                Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(eq_span),
                }
            } else {
                self.expr()?
            }
        } else {
            Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new(last, last_span)],
                meta: Meta::new(last_span),
                ext: (),
            }
        };
        let value_end = value.span().end;
        Ok(LabelValueLabel {
            label: full_label,
            label_span,
            value,
            meta: Meta::new(Span::new(label_span.start, value_end)),
        })
    }

    fn fn_placeholder_expr(&mut self, start: u32, stem: PathSegment) -> Result<Expr, Error> {
        self.expect_kind(TokenKind::LBrace, "`{` after placeholder-lambda intro")?;
        let (mut body, end) = self.lambda_body(None, Some(&stem))?;
        let span = Span::new(start, end);
        let slot_count =
            crate::pass::placeholder::classify(&stem.name, &mut body, span)?.slot_count;
        if let Some(tooling) = &mut self.tooling {
            for prefix in &mut tooling.facts.scope_prefix {
                if let ScopeSyntax::Placeholder {
                    stem: recorded,
                    slot_count: count,
                } = &mut prefix.syntax
                    && recorded.span == stem.span
                {
                    *count = slot_count;
                }
            }
        }
        Ok(Expr::FnPlaceholder {
            occurrence: Default::default(),
            stem,
            state: crate::ast::PlaceholderState::Source { slot_count },
            body: Box::new(body),
            meta: Meta::new(span),
            ext: (),
        })
    }

    /// Parse a **block body**: a sequence of `;`-terminated
    /// statements followed by a final expression, inside a function.
    ///
    /// A `let X = E;` statement adds a binding whose scope is the
    /// remainder of the block; a bare `e;`
    /// statement discards `e` (the typer requires `e : .`).
    ///
    /// The opening `{` is consumed by the caller; this function
    /// consumes everything up through the closing `}` and returns
    /// the body as a single `Expr` (a chain of `Expr::Let` /
    /// `Expr::Seq` ending in the final expression). The closing
    /// `}`'s end position is returned alongside.
    ///
    /// A block with no final expression (statement-only or empty) is
    /// a parse error per `specs/language.md` § Blocks and local
    /// bindings. A trailing `;` after a real final expression is
    /// accepted and ignored.
    fn block_body_with_signature(
        &mut self,
        signature: Option<&Signature>,
    ) -> Result<(Expr, u32), Error> {
        let scope = self.begin_body_scope(signature);
        let result = self.block_body_inner();
        self.finish_body_scope(scope, result.as_ref().ok().map(|(body, _)| body.span()));
        result
    }

    fn lambda_body(
        &mut self,
        signature: Option<&Signature>,
        placeholder: Option<&PathSegment>,
    ) -> Result<(Expr, u32), Error> {
        let scope = self.begin_body_scope(signature);
        if let Some(stem) = placeholder {
            self.record_scope_prefix(|| ScopeSyntax::Placeholder {
                stem: stem.clone(),
                slot_count: 0,
            });
        }
        if let Some((owner, _)) = scope
            && let Some(tooling) = &mut self.tooling
            && let Some(region) = tooling
                .facts
                .regions
                .iter_mut()
                .find(|region| region.open == owner)
        {
            region.lambda = true;
        }
        let result = self.block_body_inner();
        self.finish_body_scope(scope, result.as_ref().ok().map(|(body, _)| body.span()));
        result
    }

    fn block_body_inner(&mut self) -> Result<(Expr, u32), Error> {
        let block_start = self.peek_span().start;
        let mut body = self.block_body_continuation(block_start)?;
        // Comments dangling between the body's last token and the
        // closing `}` live in the `}`'s leading trivia; stash them on
        // the body's trailing slot so the formatter emits them before
        // the closer instead of dropping them.
        let closing_trivia = self.peek_leading_trivia();
        let end = self.expect_kind(TokenKind::RBrace, "`}`")?.end;
        set_trailing_trivia(body.meta_mut(), closing_trivia);
        Ok((body, end))
    }

    /// Recursive helper for [`block_body`]: parse the next
    /// statement / final-expression at the current position. On
    /// each call, the parser is sitting at the start of a
    /// statement or the final expression; the loop ends when the
    /// next token is `}`.
    ///
    /// `block_start` is the offset of the opening `{` — used to
    /// produce spans that cover the whole emptied range when the
    /// block has no final expression.
    fn block_body_continuation(&mut self, block_start: u32) -> Result<Expr, Error> {
        let leading_separator_trivia = self.skip_semicolon_separators();
        // Empty block (no statements, no final expression) — error.
        if matches!(self.peek(), Some(TokenKind::RBrace)) {
            self.cursor_choices(CursorSlot::Value, &["rec", "let"]);
            return Err(self.err(
                Span::new(block_start, self.peek_span().end),
                "block has no final expression; a block must end with an expression \
                 (statements alone are not enough)",
            ));
        }
        if self.let_statement_starts_here() {
            let mut body = self.let_statement(block_start)?;
            inject_leading_trivia(&mut body, leading_separator_trivia);
            return Ok(body);
        }
        // Otherwise: parse an expression. If it's followed by `;`,
        // it's an expression statement; the block must continue. If
        // it's followed by `}`, it's the final expression.
        let mut value_leading_trivia = leading_separator_trivia;
        value_leading_trivia.extend(self.peek_leading_trivia());
        let mut value = self.expr()?;
        if matches!(self.peek(), Some(TokenKind::Semicolon)) {
            self.advance();
            let next_clause_trivia = self.skip_semicolon_separators();
            // `e;` at end of block is a final expression with an
            // optional trailing separator.
            if matches!(self.peek(), Some(TokenKind::RBrace)) {
                self.cursor_choices(CursorSlot::Value, &["rec", "let"]);
                inject_leading_trivia(&mut value, value_leading_trivia);
                return Ok(value);
            }
            let mut body = self.block_body_continuation(block_start)?;
            inject_leading_trivia(&mut body, next_clause_trivia);
            let span = Span::new(value.span().start, body.span().end);
            // The Seq's own `meta.leading_trivia` carries the
            // trivia that preceded the value expression — that
            // surfaces above the Seq when the surrounding block
            // pretty-prints it.
            Ok(Expr::Seq {
                occurrence: Default::default(),
                value: Box::new(value),
                body: Box::new(body),
                meta: Meta {
                    span,
                    leading_trivia: value_leading_trivia,
                    trailing_trivia: Vec::new(),
                },
            })
        } else {
            inject_leading_trivia(&mut value, value_leading_trivia);
            Ok(value)
        }
    }

    fn consume_let_statement_intro(&mut self) -> Result<(Vec<Trivia>, u32), Error> {
        let leading_trivia = self.peek_leading_trivia();
        let start = self.peek_span().start;
        self.expect_ident_role("let", KeywordRole::Declaration)?;
        if self.at_sym(".") {
            self.advance();
        }
        Ok((leading_trivia, start))
    }

    /// Parse a `let X = E;` statement followed by the rest of the
    /// surrounding block. Builds `Expr::Let { body: <rest of
    /// block>, … }`.
    ///
    /// The grouped existential pattern `let .(<U_1> … <U_n> x) = E;` is sugar
    ///   over a curried CPS projector call. The parser rewrites it
    ///   as `E(_, .[U_1](…)[U_n](X) { rest })`. The RHS `E`
    ///   must evaluate to a polymorphic function of CPS shape — in
    ///   practice the call to an existential-bearing newtype's
    ///   projector, which returns exactly that shape.
    fn let_statement(&mut self, block_start: u32) -> Result<Expr, Error> {
        // Capture leading trivia of the `let` keyword token before
        // consuming it — survives the round-trip via `Expr::Let`'s
        // `leading_trivia` field, which the pretty-printer emits
        // above the `let` and uses as a hint to break the
        // surrounding block body to multi-line.
        let (leading_trivia, start) = self.consume_let_statement_intro()?;
        if matches!(self.peek(), Some(TokenKind::LParen))
            && matches!(self.peek_at(1), Some(TokenKind::LBrace))
        {
            self.advance();
            return self.row_let_statement(block_start, start, leading_trivia);
        }
        if matches!(self.peek(), Some(TokenKind::LParen))
            && matches!(self.peek_at(1), Some(TokenKind::SymbolRun(run)) if run == "<")
        {
            self.advance();
            return self.let_unpack_statement(block_start, start, leading_trivia);
        }
        if matches!(self.peek(), Some(TokenKind::LParen)) {
            return self.let_destructuring_statement(block_start, start, leading_trivia);
        }
        // Wildcard binder `let _ = …;` — `_` lexes as a slot
        // token rather than an identifier, and is admitted as the
        // wildcard-discard binder. Every other name is validated
        // (letterless names are rejected).
        let (name, name_span) = if matches!(self.peek(), Some(TokenKind::Slot1)) {
            let span = self.peek_span();
            self.advance();
            ("_".to_owned(), span)
        } else {
            self.expect_binder()?
        };
        if name != "_" {
            Self::validate_value_name(&name, name_span)?;
        }
        let ty = None;
        self.expect_sym("=", "`=`")?;
        // Capture trivia between `=` and the value expression so any
        // comments authors place above the value survive the round-
        // trip via the value's `meta.leading_trivia`.
        let value_leading_trivia = self.peek_leading_trivia();
        let mut value = self.expr()?;
        inject_leading_trivia(&mut value, value_leading_trivia);
        // `let X = E;` requires a terminating `;`. If the user
        // wrote `let X = E in …`, produce a pointed error rather
        // than a generic "expected `;`".
        if self.peek_ident_named("in") {
            return Err(self.err_here(
                "expected `;` after let binding (use a block statement: `let X = E;` \
                 followed by more statements)",
            ));
        }
        self.expect_kind(TokenKind::Semicolon, "`;` after the let binding")?;
        self.record_scope_prefix(|| ScopeSyntax::Binding {
            name: Some((name.clone(), name_span)),
            pattern: None,
            type_params: Vec::new(),
        });
        let body = self.block_body_continuation(block_start)?;
        let end = body.span().end;
        Ok(Expr::Let {
            occurrence: Default::default(),
            name,
            name_span,
            ty,
            pattern: None,
            value: Box::new(value),
            body: Box::new(body),
            meta: Meta {
                span: Span::new(start, end),
                leading_trivia,
                trailing_trivia: Vec::new(),
            },
        })
    }

    fn row_let_statement(
        &mut self,
        block_start: u32,
        start: u32,
        leading_trivia: Vec<Trivia>,
    ) -> Result<Expr, Error> {
        let (entries, _) = self.row_let_entry_list()?;
        self.expect_kind(TokenKind::RParen, "`)` after the row pattern")?;
        self.expect_sym("=", "`=`")?;
        let value_leading_trivia = self.peek_leading_trivia();
        let mut value = self.expr()?;
        inject_leading_trivia(&mut value, value_leading_trivia);
        self.expect_kind(TokenKind::Semicolon, "`;` after the row-let binding")?;
        self.record_scope_prefix(|| ScopeSyntax::RowLet(entries.clone()));
        let body = self.block_body_continuation(block_start)?;
        let end = body.span().end;
        Ok(Expr::RowLet {
            occurrence: Default::default(),
            entries,
            value: Box::new(value),
            body: Box::new(body),
            temp_name: self.synth_row_let_temp_name(),
            meta: Meta {
                span: Span::new(start, end),
                leading_trivia,
                trailing_trivia: Vec::new(),
            },
            ext: (),
        })
    }

    /// Preserve product/as-patterns for projection lowering; a single ordinary
    /// binder shares the core let representation, including its annotation.
    fn let_destructuring_statement(
        &mut self,
        block_start: u32,
        start: u32,
        leading_trivia: Vec<crate::pass::lexer::Trivia>,
    ) -> Result<Expr, Error> {
        let pattern = self.param_pattern_tuple(false)?;
        self.reject_outer_pattern_annotation()?;
        self.expect_sym("=", "`=`")?;
        let value_leading_trivia = self.peek_leading_trivia();
        let mut value = self.expr()?;
        inject_leading_trivia(&mut value, value_leading_trivia);
        self.expect_kind(
            TokenKind::Semicolon,
            "`;` after the destructuring let binding",
        )?;
        self.record_scope_prefix(|| ScopeSyntax::Binding {
            name: None,
            pattern: Some(pattern.clone()),
            type_params: Vec::new(),
        });
        let rest = self.block_body_continuation(block_start)?;
        let end = rest.span().end;
        if let Some((name, name_span, ty)) = Self::unary_bind_from_pattern(&pattern) {
            return Ok(Expr::Let {
                occurrence: Default::default(),
                name,
                name_span,
                ty,
                pattern: None,
                value: Box::new(value),
                body: Box::new(rest),
                meta: Meta {
                    span: Span::new(start, end),
                    leading_trivia,
                    trailing_trivia: Vec::new(),
                },
            });
        }
        let outer = format!("__pat_let_n{}__", pattern.match_id.0);
        let name_span = pattern.span;
        Ok(Expr::Let {
            occurrence: Default::default(),
            name: outer,
            name_span,
            ty: None,
            pattern: Some(pattern),
            value: Box::new(value),
            body: Box::new(rest),
            meta: Meta {
                span: Span::new(start, end),
                leading_trivia,
                trailing_trivia: Vec::new(),
            },
        })
    }

    /// Parse the existential-opening `let .(<U_1> … <U_n> x) = E;`
    /// followed by the rest of the surrounding block. Desugars at
    /// parse time to `E(_, .[U_1](…)[U_n](X) { rest })`. The
    /// caller has already consumed `let .(` and is positioned
    /// at the first `<`. `start` is the source offset of the `let`
    /// keyword.
    ///
    /// The RHS `E` must evaluate to a CPS-shaped polymorphic function
    /// `[R](k: [U_1]…[U_n] P -> R) -> R` — typically a
    /// call to the projector of an existential-bearing newtype, which
    /// returns exactly this shape under the curried CPS scheme.
    fn let_unpack_pattern(
        &mut self,
    ) -> Result<(Vec<TypeParam>, String, Span, Option<ParamPattern>), Error> {
        let mut binders: Vec<TypeParam> = Vec::new();
        while self.at_sym("<") {
            binders.push(self.angle_binder()?);
        }
        // Binder position admits the destructuring-pattern form
        // `let .(<U> (a: A, b: B)) = E;` — the continuation's value
        // parameter is just a pattern-bearing slot, which the
        // existing `fn`-param desugar handles for free.
        let (binder_name, binder_span, binder_pattern) =
            if matches!(self.peek(), Some(TokenKind::LParen))
                && self.peek_paren_starts_param_pattern()
            {
                let pat = self.param_pattern_tuple(false)?;
                self.reject_outer_pattern_annotation()?;
                let span = pat.span;
                let name = self.synth_pattern_param_name();
                (name, span, Some(pat))
            } else {
                let (name, name_span) = self.expect_binder()?;
                // `_` is the wildcard-discard binder, admitted here; every
                // other name is validated (letterless names are rejected).
                if name != "_" {
                    Self::validate_value_name(&name, name_span)?;
                }
                if self.at_sym(":") {
                    return Err(self.err_here(
                        "type annotations on existential-opening `let` binders are not admissible; \
                     open the existential first, then annotate a separate local binding if needed",
                    ));
                }
                (name, name_span, None)
            };
        self.expect_kind(TokenKind::RParen, "`)` after the existential pattern")?;
        Ok((binders, binder_name, binder_span, binder_pattern))
    }

    fn let_unpack_statement(
        &mut self,
        block_start: u32,
        start: u32,
        leading_trivia: Vec<Trivia>,
    ) -> Result<Expr, Error> {
        let (binders, binder_name, binder_span, binder_pattern) = self.let_unpack_pattern()?;
        self.expect_sym("=", "`=`")?;
        let value_leading_trivia = self.peek_leading_trivia();
        let mut value = self.expr()?;
        inject_leading_trivia(&mut value, value_leading_trivia);
        self.expect_kind(
            TokenKind::Semicolon,
            "`;` after the existential-opening let binding",
        )?;
        self.record_scope_prefix(|| ScopeSyntax::Binding {
            name: Some((binder_name.clone(), binder_span)),
            pattern: binder_pattern.clone(),
            type_params: binders.clone(),
        });
        let rest = self.block_body_continuation(block_start)?;
        let end = rest.span().end;
        let span = Span::new(start, end);
        let value_span = value.span();

        // Build the continuation `.[U_1](…)[U_n](X) { rest }`.
        let mut fn_params: Vec<SignatureParam> =
            binders.into_iter().map(SignatureParam::Type).collect();
        fn_params.push(SignatureParam::Value(Param {
            name: binder_name,
            ty: None,
            pattern: binder_pattern,
            meta: Meta::new(binder_span),
        }));
        let fn_expr = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(fn_params),
            ret_ty: None,
            body: Box::new(rest),
            meta: Meta::new(span),
            caps: (),
        };

        // Apply `E` to `(_, .[U_1](…)[U_n](X) { rest })`. `E` is
        // expected to evaluate to the curried CPS function returned by
        // an existential-bearing newtype's projector. The leading `_`
        // lets the typer infer the result type `r` from the
        // continuation's body.
        Ok(Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(value),
            args: vec![
                CallArg::Type(Type::Infer {
                    meta: Meta::new(value_span),
                    ext: (),
                }),
                CallArg::Value(fn_expr),
            ],
            meta: Meta {
                span,
                leading_trivia,
                trailing_trivia: Vec::new(),
            },
            ext: (),
        })
    }

    /// `(arg, arg, ...)` at a call site. Each arg is dispatched to either
    /// `type_expr` or `expr` based on syntactic shape. Each arg's
    /// captured leading trivia is injected onto the arg's
    /// `meta.leading_trivia`; the pretty-printer reads it from there.
    ///
    /// Trivia capture spans the *full* gap between the previous
    /// arg (or `(`) and this arg's first meaningful token —
    /// commas in between are discarded but their leading trivia is
    /// merged forward, so the leading-comma multi-line layout
    /// (`( , a , b )`) and the inline form (`(a, b)`) both yield
    /// the same arg-trivia AST.
    fn call_arg_list(&mut self, callee: Option<&Expr>) -> Result<(Vec<CallArg>, u32), Error> {
        self.expect_kind(TokenKind::LParen, "`(`")?;
        let mut args = Vec::new();
        // Pre-loop trivia run: comments between `(` and the first
        // arg (or its leading comma).
        let mut pending_trivia = self.peek_leading_trivia();
        while matches!(self.peek(), Some(TokenKind::Comma)) {
            self.advance();
            pending_trivia.extend(self.peek_leading_trivia());
        }
        if matches!(self.peek(), Some(TokenKind::RParen)) {
            self.call_argument_cursor(callee, false);
        }
        while !matches!(self.peek(), Some(TokenKind::RParen)) {
            let leading = std::mem::take(&mut pending_trivia);
            self.call_argument_cursor(callee, !args.is_empty());
            let mut arg = self.call_arg()?;
            inject_meta_trivia(arg.meta_mut(), leading);
            args.push(arg);
            // Capture trivia between this arg and the next arg /
            // closer, eating any number of `,` separators on the way.
            let mut saw_comma = false;
            pending_trivia = self.peek_leading_trivia();
            while matches!(self.peek(), Some(TokenKind::Comma)) {
                self.advance();
                saw_comma = true;
                pending_trivia.extend(self.peek_leading_trivia());
            }
            if matches!(self.peek(), Some(TokenKind::RParen)) {
                if saw_comma {
                    self.call_argument_cursor(callee, true);
                }
                break;
            }
            if !saw_comma {
                return Err(self.err_here("expected `,` or `)`"));
            }
        }
        // Trivia between the last arg (or trailing `,`) and the `)` is
        // the last element's trailing / dangling comment run — stash it
        // on the last arg's trailing slot so the formatter emits it
        // before the closer rather than dropping it. An empty list's
        // dangling comment has no element to carry it; the formatter's
        // interstitial-hoist fallback recovers it from the enclosing
        // construct, so it is not lost.
        let closing_trivia = std::mem::take(&mut pending_trivia);
        if let Some(last) = args.last_mut() {
            set_trailing_trivia(last.meta_mut(), closing_trivia);
        }
        let end = self.expect_kind(TokenKind::RParen, "`)`")?.end;
        Ok((args, end))
    }

    fn call_argument_cursor(&mut self, callee: Option<&Expr>, has_prior_argument: bool) {
        let unclaimed = self
            .tooling
            .as_ref()
            .is_some_and(|tooling| tooling.facts.cursor.is_none());
        self.cursor_choices(CursorSlot::Argument, &["rec"]);
        if unclaimed
            && let Some(callee) = callee
            && let Some(context) = self
                .tooling
                .as_mut()
                .and_then(|tooling| tooling.facts.cursor.as_mut())
        {
            context.call = Some(CallCursor {
                callee: callee.clone(),
                has_prior_argument,
            });
        }
    }

    /// Dispatch one positional call argument to either `expr` or
    /// `type_expr` based on its syntactic shape. Forms that are
    /// unambiguously one or the other route directly; the few
    /// genuinely ambiguous shapes (type-shaped paths, parens,
    /// braces, and symbol-led forms) use **backtracking** — try as
    /// expression first, fall back to type on failure, restoring lexer
    /// state and parser-confirmed structural facts in between. The cost is
    /// bounded at one alternative per slot (so 2× worst-case work, not
    /// exponential).
    ///
    /// Backtracking replaces the older type-head heuristic that
    /// used to route `List.un_list(a, xs)` (a value-position member
    /// call whose head spells like a type) to `type_expr` and then
    /// surface a kind error. The expr-first attempt now succeeds for
    /// such forms. Type-only shapes like `(A) -> B` fall back when
    /// the expression parse leaves unconsumed tokens, while structural
    /// type shapes like `A & B` are re-parsed as types when the type
    /// parse consumes the same range.
    fn call_arg(&mut self) -> Result<CallArg, Error> {
        self.cursor_choices(CursorSlot::Argument, &["rec"]);
        self.validate_call_arg_path_names()?;
        let parsed = match self.peek() {
            // `_` is the type-inference placeholder; in call-arg
            // position it is always a `CallArg::Type` (Kio has no
            // value-level wildcard syntax).
            Some(TokenKind::Slot1) => Ok(CallArg::Type(self.type_expr()?)),
            // Unambiguous value forms — route directly. Literals
            // and the surface-keyword expressions can't be types.
            Some(TokenKind::StrLit(_))
            | Some(TokenKind::IntLit { .. })
            | Some(TokenKind::FloatLit { .. })
            | Some(TokenKind::BoolLit(_)) => Ok(CallArg::Value(self.expr()?)),
            Some(TokenKind::Ident(s)) if s == "fn" || s == "let" || s == "rec" => {
                Ok(CallArg::Value(self.expr()?))
            }
            // Ambiguous symbol runs — try expression first for
            // leading-dot forms, variadic bracket-fold literals, and
            // prefix operators, then fall back to type syntax for
            // leading `& A & B` / `| A | B` chains.
            Some(TokenKind::SymbolRun(_)) => self.try_call_arg_alts(),
            // Ambiguous — try expr first, fall back to type.
            Some(TokenKind::Ident(_)) | Some(TokenKind::LParen) | Some(TokenKind::LBrace) => {
                self.try_call_arg_alts()
            }
            _ => Err(self.err_here("expected call argument")),
        };
        if let Ok(CallArg::Type(ty)) = &parsed
            && !matches!(ty, Type::Infer { .. })
            && let Some(tooling) = &mut self.tooling
            && let Some(cursor) = tooling.cursor
            && ty.span().start <= cursor
            && cursor <= ty.span().end
            && let Some(context) = &mut tooling.facts.cursor
            && context.slot == CursorSlot::Argument
        {
            context.slot = CursorSlot::Type;
            context.keywords.clear();
        }
        parsed
    }

    /// Backtracking dispatch: try parsing as a value expression
    /// first; if that fails — *or* if it succeeds but leaves the
    /// parser at a token the surrounding call-arg list can't
    /// continue from — restore parser state and try as a type
    /// expression. If both fail, return whichever attempt consumed
    /// more tokens (the "deeper failure" is usually the more
    /// informative diagnostic).
    ///
    /// The post-state check matters for forms like `(String) -> .`:
    /// `expr_paren` parses `(String)` as a successful single-element
    /// grouping and leaves `.> ()` unconsumed, which would break the
    /// surrounding `call_arg_list` ("expected `,` or `)`" at the
    /// arrow). The type-side parser handles the whole function-type
    /// shape in one go, so falling back gets the right structure.
    fn try_call_arg_alts(&mut self) -> Result<CallArg, Error> {
        let saved_cursor = self.cursor.clone();
        let checkpoint = self.tooling_checkpoint();
        let expr_attempt = self.expr();
        if let Ok(e) = &expr_attempt
            && self.call_arg_list_continues()
        {
            if Self::expr_can_be_type_arg_candidate(e) {
                let expr_end_cursor = self.cursor.clone();
                let expr_end_consumed = self.cursor.consumed();
                let value_attempt = e.clone();
                if let Some(t) = self.try_type_arg_same_span(
                    saved_cursor.clone(),
                    expr_end_cursor.clone(),
                    expr_end_consumed,
                    &checkpoint,
                ) {
                    return Ok(CallArg::Type(t));
                }
                return Ok(CallArg::Value(value_attempt));
            }
            // Special case: a top-level UFCS expression is
            // syntactically indistinguishable from a function-type
            // (`(A) -> B` vs. `A.>B`). When both parses consume the
            // same range and the type-side parses cleanly, prefer
            // the type interpretation — UFCS in argument position
            // most commonly means the user wrote a function type
            // like `(I32) -> Strbox` for a polymorphic call's
            // type-argument slot. The typer's UFCS dispatch is
            // unforgiving on type-shaped receivers, so this also
            // produces clearer diagnostics than letting the typer
            // try and fail.
            if matches!(e, Expr::Ufcs { .. }) {
                let expr_end_cursor = self.cursor.clone();
                let expr_end_consumed = self.cursor.consumed();
                let value_attempt = e.clone();
                if let Some(t) = self.try_type_arg_same_span(
                    saved_cursor,
                    expr_end_cursor.clone(),
                    expr_end_consumed,
                    &checkpoint,
                ) {
                    return Ok(CallArg::Type(t));
                }
                return Ok(CallArg::Value(value_attempt));
            }
            return Ok(CallArg::Value(e.clone()));
        }
        // At EOF an otherwise complete value argument is authoritative even
        // though the surrounding call still lacks its closing `)`. The
        // missing outer delimiter does not change a parsed value into a type.
        if let Ok(e) = &expr_attempt
            && self.peek().is_none()
        {
            return Ok(CallArg::Value(e.clone()));
        }
        // Either the expr attempt failed outright, or it succeeded
        // but left the parser at a token that can't continue the
        // surrounding call-arg list. Restore and try as a type.
        let expr_err = expr_attempt.err();
        let expr_facts = self.take_tooling_suffix(&checkpoint);
        self.cursor = saved_cursor;
        match self.type_expr() {
            Ok(t) => Ok(CallArg::Type(t)),
            Err(type_err) => {
                let type_reached_unclosed_forall = self
                    .tooling
                    .as_ref()
                    .is_some_and(|tooling| tooling.facts.structural_forall.unclosed_at_eof);
                match expr_err {
                    // Entering an actual structural binder and reaching EOF
                    // is the deeper call-argument branch even when the two
                    // diagnostics happen to end at the same byte.
                    Some(_) if type_reached_unclosed_forall => Err(type_err),
                    Some(expr_err) => {
                        let expr_end = expr_err.diag().0.end;
                        let type_end = type_err.diag().0.end;
                        if expr_end >= type_end {
                            self.restore_tooling_suffix(&checkpoint, expr_facts);
                            Err(expr_err)
                        } else {
                            Err(type_err)
                        }
                    }
                    None => Err(type_err),
                }
            }
        }
    }

    fn try_type_arg_same_span(
        &mut self,
        saved_cursor: SkeletonCursor<'a>,
        expr_end_cursor: SkeletonCursor<'a>,
        expr_end_consumed: u64,
        checkpoint: &Option<ToolingCheckpoint>,
    ) -> Option<Type> {
        let expr_facts = self.take_tooling_suffix(checkpoint);
        self.cursor = saved_cursor;
        if let Ok(t) = self.type_expr()
            && self.call_arg_list_continues()
            && self.cursor.consumed() == expr_end_consumed
        {
            return Some(t);
        }
        self.cursor = expr_end_cursor;
        self.restore_tooling_suffix(checkpoint, expr_facts);
        None
    }

    /// True if the next token can validly continue a call-arg list —
    /// i.e., either `,` (next arg) or `)` (end of args). Used by the
    /// expr-first backtracking step in [`try_call_arg_alts`] to detect
    /// successful-but-stuck parses like `(String)` followed by `->`.
    fn call_arg_list_continues(&self) -> bool {
        matches!(
            self.peek(),
            Some(TokenKind::Comma) | Some(TokenKind::RParen)
        )
    }

    fn expr_can_be_type_arg_candidate(expr: &Expr) -> bool {
        fn walk(expr: &Expr) -> (bool, bool) {
            match expr {
                Expr::Tuple { items, .. } => {
                    let mut saw_type_path = false;
                    for item in items {
                        let (ok, has_type_path) = walk(item);
                        if !ok {
                            return (false, false);
                        }
                        saw_type_path |= has_type_path;
                    }
                    (true, saw_type_path)
                }
                Expr::Unit { .. } => (true, false),
                Expr::Path { segments, .. } => {
                    let is_type_path = segments
                        .last()
                        .is_some_and(|segment| crate::naming::starts_like_type_name(&segment.name));
                    (is_type_path, is_type_path)
                }
                Expr::OpChain {
                    kind: OpChainKind::Normal { pattern, slots, .. },
                    ..
                } if op_chain_pattern_is_type_chain(pattern) => {
                    let mut saw_type_path = false;
                    for slot in slots {
                        let (ok, has_type_path) = walk(slot);
                        if !ok {
                            return (false, false);
                        }
                        saw_type_path |= has_type_path;
                    }
                    (true, saw_type_path)
                }
                _ => (false, false),
            }
        }

        matches!(expr, Expr::Tuple { .. } | Expr::OpChain { .. }) && {
            let (ok, saw_type_path) = walk(expr);
            ok && saw_type_path
        }
    }
}

fn op_chain_pattern_is_type_chain(pattern: &[OpPart]) -> bool {
    let mut kind: Option<ChainKind> = None;
    for part in pattern {
        let OpPart::Token { content, .. } = part else {
            continue;
        };
        let token_kind = if !content.is_empty() && content.chars().all(|c| c == '&') {
            ChainKind::Amp
        } else if !content.is_empty() && content.chars().all(|c| c == '|') {
            ChainKind::Pipe
        } else {
            return false;
        };
        match kind {
            Some(existing) if existing != token_kind => return false,
            Some(_) => {}
            None => kind = Some(token_kind),
        }
    }
    kind.is_some()
}

/// Stride between per-item parallel-task `NodeId` ranges. Each
/// rayon task starts at `(idx + 1) * NODE_ID_TASK_STRIDE` so the
/// IDs across tasks don't collide. The sequential op pre-scan
/// uses the main parser's `next_node_id` starting at 0, which
/// stays below the first task's offset for any plausible op
/// count (a module declaring ten million `if`/`else`-bearing
/// ops would still leave room).
const NODE_ID_TASK_STRIDE: u64 = 10_000_000;

fn parse_item_from_children(
    children: &[SkeletonNode],
    eof: usize,
    operator_scope: OpRegistry,
    node_id_base: u64,
    mode: ItemParseMode,
) -> Result<ParsedLazyItem, Error> {
    let mut sub = Parser::new_at(children, eof);
    sub.operator_scope = operator_scope.clone();
    sub.next_node_id = node_id_base;
    let mut parsed = sub.documented_declaration(
        |parser| parser.item_with_mode(mode),
        |parsed, doc, leading| {
            set_item_doc(&mut parsed.item, doc, &leading);
            inject_meta_trivia(parsed.item.meta_mut(), leading);
        },
    )?;
    retain_compact_declaration_comments(&mut parsed.item, children);
    sub.expect_eof()?;
    if mode == ItemParseMode::Lazy
        && matches!(
            parsed.item,
            Item::FnDef(_)
                | Item::RecGroup(_, _)
                | Item::Equiv(_, _)
                | Item::Elaborator(_, _)
                | Item::VariadicOperator(_, _)
        )
    {
        parsed.deferred = Some(DeferredItem {
            item_index: 0,
            children: children.to_vec(),
            operator_scope,
            node_id_base,
            eof,
        });
    }
    Ok(parsed)
}

/// One top-level item's skeleton subtree, ready to feed to a
/// per-item sub-parser. `children` is a sub-slice of the top-
/// level frame's children that together form one item;
/// `is_op` flags operator declarations, which the parallel
/// driver routes through the sequential operator-scope pass.
#[derive(Debug)]
struct ItemSlice<'a> {
    children: &'a [crate::pass::tree_skeleton::SkeletonNode],
    is_op: bool,
}

fn skeleton_node_start(node: &SkeletonNode) -> u32 {
    match node {
        SkeletonNode::Leaf(token) => token.span.start,
        SkeletonNode::Group { open, .. } => open.span.start,
    }
}

/// Partition the remaining top-level skeleton children into
/// per-item subslices. Each item runs from the start of its
/// declaring keyword (or `pub` modifier) to the start of the
/// next item or the end of input. Item kinds are detected by
/// peeking the first identifier (after an optional `pub`):
/// Fixed and variadic `op` items trigger the sequential operator-scope pre-scan
/// in [`parse_items_in_parallel`]; all other kinds parse in
/// parallel.
///
/// At the top-level skeleton frame we never see item-keyword
/// identifiers (`fn` / `op` / …) *inside* an item — those
/// appear inside Brace / Paren Groups, which are opaque to the
/// top-level walk. So scanning the top-level children for the
/// next item-keyword Leaf reliably finds item boundaries
/// regardless of how many `{ … }` groups the current item
/// contains (e.g., a `labels` declaration has a top-level Brace
/// Group for its arm) and we walk past it.
fn partition_top_level_items(
    children: &[crate::pass::tree_skeleton::SkeletonNode],
) -> Vec<ItemSlice<'_>> {
    use crate::pass::tree_skeleton::SkeletonNode;
    let mut slices: Vec<ItemSlice<'_>> = Vec::new();
    let mut start = 0;
    while start < children.len() {
        let keyword = peek_item_keyword(children, start);
        let is_op = matches!(keyword, Some("op" | "varop"));
        // Skip past the current item's modifier set + keyword
        // before scanning for the next item start, so the scan
        // doesn't immediately re-find this item's own keyword.
        let mut scan_from = start;
        while let Some(next) = skip_one_item_modifier(children, scan_from) {
            scan_from = next;
        }
        scan_from += 1;
        if matches!(keyword, Some("rec")) {
            if matches!(
                children.get(scan_from),
                Some(SkeletonNode::Group {
                    kind: crate::pass::tree_skeleton::GroupKind::Paren,
                    ..
                })
            ) {
                scan_from += 1;
            }
            while let Some(next) = skip_one_item_modifier(children, scan_from) {
                scan_from = next;
            }
            if let Some(SkeletonNode::Leaf(t)) = children.get(scan_from)
                && let TokenKind::Ident(name) = &t.kind
                && matches!(name.as_str(), "fn" | "type" | "newtype" | "labels")
            {
                scan_from += 1;
            }
        }
        let end = find_next_item_start(children, scan_from);
        slices.push(ItemSlice {
            children: &children[start..end],
            is_op,
        });
        start = end;
    }
    slices
}

/// Identify the item-keyword starting at `start` in `children`.
/// Returns the keyword string (`"fn"`, `"op"`, …) or
/// `None` if no full item-start shape is present at that position.
/// Skips leading `pub` / `pure` modifier leaves transparently —
/// `pub pure fn` and `pure pub fn` both read as `fn`.
fn peek_item_keyword(
    children: &[crate::pass::tree_skeleton::SkeletonNode],
    start: usize,
) -> Option<&str> {
    use crate::pass::tree_skeleton::SkeletonNode;
    let mut peek = start;
    while let Some(next) = skip_one_item_modifier(children, peek) {
        peek = next;
    }
    let Some(SkeletonNode::Leaf(t)) = children.get(peek) else {
        return None;
    };
    let TokenKind::Ident(s) = &t.kind else {
        return None;
    };
    if item_keyword_starts_here(children, peek, s) {
        Some(s.as_str())
    } else {
        None
    }
}

/// Index of the next top-level item's first child, or
/// `children.len()` if no further item starts. A new item begins
/// at any full item-start shape (with the `pub` prefix absorbed
/// transparently).
fn find_next_item_start(
    children: &[crate::pass::tree_skeleton::SkeletonNode],
    from: usize,
) -> usize {
    use crate::pass::tree_skeleton::SkeletonNode;
    let mut i = from;
    while i < children.len() {
        if matches!(
            children.get(i.wrapping_sub(1)),
            Some(SkeletonNode::Leaf(prev))
                if matches!(&prev.kind, TokenKind::Ident(prev_name) if prev_name == "fn")
        ) {
            i += 1;
            continue;
        }
        if peek_item_keyword(children, i).is_some() {
            return i;
        }
        i += 1;
    }
    children.len()
}

/// Does the identifier `s` at `idx` have the token shape of a
/// top-level item start? `find_next_item_start` excludes contextual
/// names immediately after `fn`; parenthesized operator patterns
/// remain ordinary declaration heads here.
fn item_keyword_starts_here(
    children: &[crate::pass::tree_skeleton::SkeletonNode],
    idx: usize,
    s: &str,
) -> bool {
    use crate::pass::tree_skeleton::{GroupKind, SkeletonNode};

    let next_is = |offset: usize, expected: &str| -> bool {
        matches!(
            children.get(idx + offset),
            Some(SkeletonNode::Leaf(t)) if t.kind.is_sym(expected)
        )
    };
    let next_is_semicolon = |offset: usize| -> bool {
        matches!(
            children.get(idx + offset),
            Some(SkeletonNode::Leaf(t)) if matches!(t.kind, TokenKind::Semicolon)
        )
    };
    let paren_group = |offset: usize| -> bool {
        matches!(
            children.get(idx + offset),
            Some(SkeletonNode::Group {
                kind: GroupKind::Paren,
                ..
            })
        )
    };
    let brace_group = |offset: usize| -> bool {
        matches!(
            children.get(idx + offset),
            Some(SkeletonNode::Group {
                kind: GroupKind::Brace,
                ..
            })
        )
    };
    let leaf_ident = |offset: usize| -> bool {
        matches!(
            children.get(idx + offset),
            Some(SkeletonNode::Leaf(t)) if matches!(t.kind, TokenKind::Ident(_))
        )
    };
    let leaf_value_name = |offset: usize| -> bool {
        matches!(
            children.get(idx + offset),
            Some(SkeletonNode::Leaf(t))
                if matches!(t.kind, TokenKind::Ident(_) | TokenKind::Slot1)
        )
    };
    let op_pattern_lead = |offset: usize| -> bool {
        matches!(
            children.get(idx + offset),
            Some(SkeletonNode::Leaf(t))
                if matches!(
                    t.kind,
                    TokenKind::Slot1
                        | TokenKind::Slot2
                        | TokenKind::Slot3
                        | TokenKind::SymbolRun(_)
                )
        ) || paren_group(offset)
    };

    // The `host` modifier (with `host type` / `host fn`) is absorbed by
    // `is_item_modifier_leaf` before this lookup, so when control
    // reaches here `s` is the `type` / `fn` keyword that follows `host`
    // — no dedicated `host` arm is needed.
    match s {
        "fn" if paren_group(1) || next_is(1, "=") || next_is_semicolon(1) => false,
        "fn" => leaf_value_name(1),
        "type" => leaf_ident(1) || brace_group(1),
        "literal" | "newtype" | "equiv" | "elab" => leaf_ident(1),
        "labels" => leaf_ident(1) || brace_group(1),
        "op" | "varop" => op_pattern_lead(1),
        "rec" => {
            paren_group(1)
                || brace_group(1)
                || matches!(
                    children.get(idx + 1),
                    Some(SkeletonNode::Leaf(t))
                        if matches!(
                            &t.kind,
                            TokenKind::Ident(name)
                                if matches!(name.as_str(), "type" | "newtype" | "labels")
                        )
                )
        }
        _ => false,
    }
}

fn is_item_modifier_leaf(
    children: &[crate::pass::tree_skeleton::SkeletonNode],
    idx: usize,
) -> bool {
    use crate::pass::tree_skeleton::SkeletonNode;
    let leaf_named = |offset: usize, expected: &str| -> bool {
        matches!(
            children.get(idx + offset),
            Some(SkeletonNode::Leaf(t))
                if matches!(&t.kind, TokenKind::Ident(name) if name == expected)
        )
    };
    if leaf_named(0, "pub") || leaf_named(0, "pure") {
        return true;
    }
    // `host` is a contextual keyword: it is the host-declaration
    // modifier only when followed by `type` / `fn` (optionally with a
    // `pub` in between, accepting `host pub type`). In any other
    // position it stays an ordinary identifier, so it is not absorbed
    // as a modifier here. Mirrors `Parser::host_decl_follows`.
    leaf_named(0, "host")
        && (leaf_named(1, "type")
            || leaf_named(1, "fn")
            || (leaf_named(1, "pub") && (leaf_named(2, "type") || leaf_named(2, "fn"))))
}

/// Advance past one item modifier at `idx`, returning the next index —
/// or `None` if `idx` is not at a modifier. A `pub` modifier may be
/// followed by a `(<path>)` visibility-restriction group; that group
/// is consumed as part of the same modifier so the item splitter keeps
/// `pub(P)` attached to the item it qualifies (otherwise the group is
/// mistaken for the item keyword and the item is split in two).
fn skip_one_item_modifier(
    children: &[crate::pass::tree_skeleton::SkeletonNode],
    idx: usize,
) -> Option<usize> {
    use crate::pass::tree_skeleton::{GroupKind, SkeletonNode};
    if !is_item_modifier_leaf(children, idx) {
        return None;
    }
    let is_pub = matches!(
        children.get(idx),
        Some(SkeletonNode::Leaf(t)) if matches!(&t.kind, TokenKind::Ident(n) if n == "pub")
    );
    let mut next = idx + 1;
    if is_pub
        && matches!(
            children.get(next),
            Some(SkeletonNode::Group {
                kind: GroupKind::Paren,
                ..
            })
        )
    {
        next += 1;
    }
    Some(next)
}

fn op_part_span(p: &OpPart) -> Span {
    match p {
        OpPart::SlotPlain { span, .. }
        | OpPart::SlotRecursive { span, .. }
        | OpPart::SlotGreedy { span }
        | OpPart::Token { span, .. } => *span,
    }
}

/// Parse the complete tagged grammar used by imports and operator queries.
#[cfg(feature = "repl-core")]
pub(crate) fn parse_operator_grammar(source: &str) -> Result<crate::ast::OperatorGrammar, Error> {
    let tokens = crate::pass::lexer::lex(source)?;
    let skeleton = crate::pass::tree_skeleton::build(tokens);
    let mut parser = Parser::new(&skeleton, source.len());
    let item = parser.parse_import_item()?;
    parser.expect_eof()?;
    match item {
        ImportItem::OperatorPattern { grammar, .. } => Ok(grammar),
        _ => Err(Error::parse(
            Span::new(0, source.len() as u32),
            "expected a complete tagged operator grammar",
        )),
    }
}

/// Build the operator-scope trie key for a validated `op`
/// pattern: the contiguous **leading op-token run** that the parser
/// can read forward at a use site before hitting a slot.
///
/// For non-prefix patterns the leading run sits between the first
/// slot (the LHS, already parsed at the use site) and the next slot
/// (or the pattern's end). For prefix patterns the leading run is
/// the run of tokens before the first slot. Multi-slot patterns
/// (`_ ? _ : __`) supply *only* their leading run as the key (e.g.
/// `["?"]`) — the inner tokens are matched by the existing pattern
/// walk after the trie has identified which op is firing.
///
/// Prefix-shaped operators are stored in a separate trie, so the
/// same op-token spelling can be both prefix and non-prefix (e.g.
/// unary `-` plus binary `-`).
fn leading_op_run(pattern: &[OpPart]) -> Vec<String> {
    let starts_with_token = matches!(pattern.first(), Some(OpPart::Token { .. }));
    let start = if starts_with_token {
        0
    } else {
        pattern
            .iter()
            .position(|p| matches!(p, OpPart::Token { .. }))
            .expect("validated patterns include >= 1 token")
    };
    let mut run = Vec::new();
    for p in &pattern[start..] {
        match p {
            OpPart::Token { content, .. } => run.push(content.clone()),
            OpPart::SlotPlain { .. } | OpPart::SlotRecursive { .. } | OpPart::SlotGreedy { .. } => {
                break;
            }
        }
    }
    run
}

#[cfg(any(feature = "surface", feature = "cli"))]
impl OperatorDispatchKey {
    /// Render the internal collision key for structural comparisons.
    /// Run boundaries remain explicit so distinct keys cannot alias.
    pub(crate) fn render(&self) -> String {
        let mut out = CanonicalWriter::default();
        if self.expr_start {
            // Expression-start (prefix-unary / bracket-unary /
            // variadic): `SYMBOLS _`. A space separates the run from
            // the slot for readability and to keep the slot a distinct
            // token on re-parse.
            push_op_token_run(&self.leading_run, &mut out);
            out.push_space_then("_");
        } else {
            // Non-expression-start (binary / postfix / ternary):
            // `_ SYMBOLS`.
            out.push_unit("_");
            if let Some(first) = self.leading_run.first() {
                out.push_space_then(first);
                push_op_token_run(&self.leading_run[1..], &mut out);
            }
        }
        out.into_string()
    }
}

/// Render a fixed operator's complete tagged public grammar.
#[cfg(any(feature = "repl", feature = "repl-core"))]
pub(crate) fn op_name(body: &OpBody) -> String {
    let OpBody::Normal { pattern, .. } = body;
    crate::ast::OperatorGrammar::fixed(pattern).render()
}

#[cfg(any(feature = "repl", feature = "repl-core"))]
pub(crate) fn variadic_name(operator: &VariadicOperator) -> String {
    crate::ast::OperatorGrammar::variadic(&operator.open, &operator.spec).render()
}

/// True iff `c` is an op-char that the lexer fuses into a `SymbolRun`.
/// Only the dedicated structural punctuators are absent; square brackets
/// are ordinary op-chars and therefore need the same load-bearing boundary
/// spaces as the rest. Mirrors the op-char allow-list in
/// `specs/language.md` § Operators (Operator-character set).
#[cfg(any(feature = "surface", feature = "cli"))]
fn is_fusable_op_char(c: char) -> bool {
    matches!(
        c,
        '+' | '-'
            | '*'
            | '/'
            | '%'
            | '^'
            | '~'
            | '?'
            | '@'
            | '#'
            | '$'
            | '\\'
            | '\''
            | '`'
            | '<'
            | '>'
            | '='
            | '!'
            | '&'
            | '|'
            | ':'
            | '.'
            | '['
            | ']'
    )
}

/// Accumulates the canonical name one **lexical unit** at a time (a
/// slot `_` / `__` / `___`, an op-token run, the `...` ellipsis, a
/// bracket / paren, or a `,` / `;` separator), inserting a
/// load-bearing space between two consecutive units only when the
/// previous unit's last char and the next unit's first char are
/// **both** fusable op-chars — otherwise the lexer would re-fuse two
/// separately-declared op-token runs into one (`&& ++` → `&&++`).
/// A unit is never split internally (`...` stays `...`, never `. . .`);
/// the boundary rule applies only *between* whole units.
#[cfg(any(feature = "surface", feature = "cli"))]
#[derive(Default)]
struct CanonicalWriter {
    buf: String,
    last: Option<char>,
}

#[cfg(any(feature = "surface", feature = "cli"))]
impl CanonicalWriter {
    /// Append one lexical unit, inserting a boundary space if the
    /// previous unit could fuse with this one.
    fn push_unit(&mut self, unit: &str) {
        let Some(first) = unit.chars().next() else {
            return;
        };
        if let Some(prev) = self.last
            && is_fusable_op_char(prev)
            && is_fusable_op_char(first)
        {
            self.buf.push(' ');
        }
        self.buf.push_str(unit);
        self.last = unit.chars().next_back();
    }

    /// Append a unit, always preceded by exactly one space (used at the
    /// slot↔run boundary of an operator name, where a space is part of
    /// the canonical spelling regardless of fusion).
    fn push_space_then(&mut self, unit: &str) {
        if !self.buf.is_empty() {
            self.buf.push(' ');
        }
        self.buf.push_str(unit);
        self.last = unit.chars().next_back();
    }

    fn into_string(self) -> String {
        self.buf
    }
}

/// Append a run of op-tokens (`["&&", "++"]`) to `out`, each as its
/// own unit. The writer's boundary rule inserts a load-bearing space
/// between two fusable op-token units (`&&` `++` → `&& ++`).
#[cfg(any(feature = "surface", feature = "cli"))]
fn push_op_token_run(tokens: &[String], out: &mut CanonicalWriter) {
    for tok in tokens {
        out.push_unit(tok);
    }
}

/// Validate that a `op` pattern is one of the supported
/// shapes. See [`Parser::op`] for the supported set.
/// Two reservations apply:
///
/// - **Leading-dot reservation** (any pattern): an operator component
///   whose spelling starts with `.` must contain at least two dots.
///   Dot-led one-dot runs (`.`, `.>`, `.+`, `.###$`, …) are held back
///   for member access, UFCS, lambdas, placeholder lambdas, and future
///   dot-led syntax. This is a per-token spelling rule, quoted or
///   unquoted: `..`, `.+.`, `<.>`, and `+.` stay valid.
/// - **`//`-prefix reservation** (any pattern): an operator component
///   whose spelling **starts with** `//` is reserved for the future
///   `//…` syntax family. This is a `starts_with("//")` **prefix**
///   reject, reserving the whole family (`//!`, `//?`, `//=`, …), not
///   a single spelling.
/// - **Standalone `=` reservation**: `=` is admissible only through
///   operator-token quotation, so the fixed-arity parser rejects it
///   when recorded as an unquoted token.
///
/// See `specs/language.md` § Operators.
fn validate_op_pattern(pattern: &[OpPart], span: Span) -> Result<(), Error> {
    // Adjacent slots (`_ _` or `_ __`) are rejected — there's no
    // delimiter between them. Adjacent op-tokens (`_ & & _` for
    // the `_ && _` pattern) are accepted: each token is consumed
    // in sequence during operator-usage parsing.
    let mut prev_was_slot = false;
    for (i, part) in pattern.iter().enumerate() {
        let is_slot = matches!(
            part,
            OpPart::SlotPlain { .. } | OpPart::SlotRecursive { .. }
        );
        if i > 0 && prev_was_slot && is_slot {
            return Err(Error::parse(
                span,
                "adjacent slots (`_` / `__`) in a `op` pattern are ambiguous".to_owned(),
            ));
        }
        prev_was_slot = is_slot;
    }
    let op_tokens: Vec<(&str, Span)> = pattern
        .iter()
        .filter_map(|p| match p {
            OpPart::Token { content, span, .. } => Some((content.as_str(), *span)),
            _ => None,
        })
        .collect();
    for (content, tok_span) in &op_tokens {
        if content.contains(['[', ']']) {
            return Err(Error::parse(
                *tok_span,
                "fixed operator tokens cannot contain square brackets; use a mirrored `varop` delimiter",
            ));
        }
        if is_reserved_leading_dot_op_token(content) {
            return Err(reserved_leading_dot_op_token_error(content, *tok_span));
        }
    }
    for part in pattern {
        if let OpPart::Token {
            content,
            span,
            quoted,
            ..
        } = part
            && is_reserved_standalone_equals_op_token(content)
            && !quoted
        {
            return Err(reserved_standalone_equals_op_token_error(*span));
        }
    }
    // `//`-prefix reservation: any operator component whose spelling
    // starts with `//` is reserved for future syntax — the whole
    // `//…` family (`//!`, `//?`, `//=`, …). The lexer caps a single
    // `SymbolRun` at one `/`, so a `//` prefix is only reachable as
    // two adjacent `/` op-tokens; check each maximal run of adjacent
    // op-tokens. Unlike the leading-dot rule above, this checks the
    // maximal adjacent run because the lexer splits before a second
    // `/`; it is a
    // `starts_with("//")` prefix reject, not a narrow spelling reject:
    // it reserves the entire family, not a single spelling. The `//`
    // sequence already lexes as a line comment outside `op` patterns,
    // so reserving it here keeps the surface coherent if `//…` later
    // takes on operator meaning.
    let mut run = String::new();
    let mut run_span: Option<Span> = None;
    for part in pattern {
        match part {
            OpPart::Token { content, span, .. } => {
                run.push_str(content);
                run_span = Some(match run_span {
                    None => *span,
                    Some(s) => Span::new(s.start, span.end),
                });
                if run.starts_with("//") {
                    return Err(Error::parse(
                        run_span.expect("run is non-empty when it has a `//` prefix"),
                        "an `op` operator component starting with `//` is reserved for \
                             future syntax — the whole `//…` family (`//!`, `//?`, `//=`, …) \
                             is held back. Use a different operator spelling."
                            .to_owned(),
                    ));
                }
            }
            // A slot breaks the adjacent-token run; reset.
            OpPart::SlotPlain { .. } | OpPart::SlotRecursive { .. } | OpPart::SlotGreedy { .. } => {
                run.clear();
                run_span = None;
            }
        }
    }
    Ok(())
}

fn item_span(item: &Item) -> Span {
    item.span()
}

/// Fold a product-shaped list into its identity / unary / binary AST
/// form: `[] -> .`, `[A] -> A`, and `[A, B, C] -> A & (B & C)`.
fn fold_product_chain(chain: Vec<Type>, outer_span: Span) -> Type {
    if chain.is_empty() {
        return Type::Unit {
            meta: Meta::new(outer_span),
        };
    }
    if chain.len() == 1 {
        return chain.into_iter().next().expect("one item");
    }
    let mut iter = chain.into_iter().rev();
    let last = iter.next().expect("non-empty");
    let mut acc = last;
    for ty in iter {
        let span = Span::new(ty.span().start, acc.span().end);
        acc = Type::Product {
            left: Box::new(ty),
            right: Box::new(acc),
            meta: Meta::new(span),
        };
    }
    // Promote the outer node's span to cover the parens for nicer
    // diagnostics.
    match acc {
        Type::Product { left, right, .. } => Type::Product {
            left,
            right,
            meta: Meta::new(outer_span),
        },
        _ => unreachable!("right-fold of ≥2 elements yields Product at the head"),
    }
}

/// Fold a sum-shaped list into its identity / unary / binary AST form:
/// `[] -> !`, `[A] -> A`, and `[A, B, C] -> A | (B | C)`.
fn fold_sum_chain(chain: Vec<Type>, outer_span: Span) -> Type {
    if chain.is_empty() {
        return Type::Bottom {
            meta: Meta::new(outer_span),
        };
    }
    if chain.len() == 1 {
        return chain.into_iter().next().expect("one item");
    }
    let mut iter = chain.into_iter().rev();
    let last = iter.next().expect("non-empty");
    let mut acc = last;
    for ty in iter {
        let span = Span::new(ty.span().start, acc.span().end);
        acc = Type::Sum {
            left: Box::new(ty),
            right: Box::new(acc),
            meta: Meta::new(span),
        };
    }
    match acc {
        Type::Sum { left, right, .. } => Type::Sum {
            left,
            right,
            meta: Meta::new(outer_span),
        },
        _ => unreachable!("right-fold of ≥2 elements yields Sum at the head"),
    }
}
