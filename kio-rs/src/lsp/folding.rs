//! LSP `textDocument/foldingRange` handler.
//!
//! Walks the CST tree-skeleton of a source file and emits one
//! `FoldingRange` per balanced `{ … }` brace group. Paren groups
//! (`( … )`) are intentionally excluded — they are typically short
//! argument lists and type signatures that editors don't collapse.
//! Recovered (unbalanced) brace groups still produce a folding
//! range, using the close span synthesized by the skeleton builder.
//!
//! ## Algorithm
//!
//! 1. Lex and build the `SkeletonFile` from the source text.
//! 2. Walk every `SkeletonNode` recursively.
//! 3. For each `Group { kind: Brace, … }`: record a `FoldingRange`
//!    from the open-brace line through the close-brace line.
//! 4. Return the sorted, deduplicated list (source order).
//!
//! Ranges where `start_line == end_line` are dropped — a one-line
//! brace group offers no useful collapse point.
//!
//! ## Stale-snapshot behaviour
//!
//! `foldingRange` is a pure structural query: it parses on demand
//! from the overlay text (or any caller-supplied source string) so
//! even a mid-edit document yields correct ranges. The caller is
//! responsible for supplying the right text; `handle_folding_range`
//! takes `source: &str` directly.

use lsp_types::{FoldingRange, FoldingRangeKind};

use crate::lsp::positions::LineIndex;
use crate::pass::parser::parse_tree_skeleton;
use crate::pass::tree_skeleton::{GroupKind, SkeletonNode};

/// Parse `source` (a regular `.kio` module file) and return all
/// folding ranges found in its brace groups.
///
/// Lexing / skeleton build errors are treated as an empty result —
/// a partially-broken file still folds the ranges it can.
pub fn folding_ranges(source: &str, line_index: &LineIndex) -> Vec<FoldingRange> {
    let skeleton = match parse_tree_skeleton(source) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut ranges: Vec<FoldingRange> = Vec::new();
    for node in &skeleton.children {
        collect_ranges(node, line_index, &mut ranges);
    }
    ranges
}

/// Recursively walk a `SkeletonNode` and push a `FoldingRange` for
/// every brace group (including recovered groups). Recurses into
/// every group's children regardless of bracket kind.
fn collect_ranges(node: &SkeletonNode, line_index: &LineIndex, out: &mut Vec<FoldingRange>) {
    match node {
        SkeletonNode::Leaf(_) => {}
        SkeletonNode::Group {
            open,
            kind: GroupKind::Brace,
            children,
            close,
            ..
        } => {
            // Compute the close span: use the recorded close token
            // for balanced groups; for recovered groups the close
            // token is synthesized at the children's end.
            let close_span = close
                .as_ref()
                .map(|t| t.span)
                .unwrap_or_else(|| children.last().map(end_span_of).unwrap_or(open.span));
            let start_line = line_index.to_position(open.span.start).line;
            let end_line = line_index
                .to_position(close_span.end.saturating_sub(1))
                .line;
            // Only emit ranges that span more than one line.
            if end_line > start_line {
                out.push(FoldingRange {
                    start_line,
                    start_character: None,
                    end_line,
                    end_character: None,
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: None,
                });
            }
            // Recurse into children.
            for child in children {
                collect_ranges(child, line_index, out);
            }
        }
        SkeletonNode::Group {
            children,
            kind: GroupKind::Paren,
            ..
        } => {
            // Paren groups: recurse into children only (no top-level
            // fold range for paren groups — they're too small /
            // argument-list-shaped to be useful collapse points).
            for child in children {
                collect_ranges(child, line_index, out);
            }
        }
    }
}

/// The end span of a skeleton node. For leaves it's the leaf token's
/// span; for groups it's the close span (balanced) or the last
/// child's end (recovered).
fn end_span_of(node: &SkeletonNode) -> crate::span::Span {
    match node {
        SkeletonNode::Leaf(t) => t.span,
        SkeletonNode::Group {
            open,
            children,
            close,
            ..
        } => close
            .as_ref()
            .map(|t| t.span)
            .unwrap_or_else(|| children.last().map(end_span_of).unwrap_or(open.span)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges_for(source: &str) -> Vec<FoldingRange> {
        let li = LineIndex::new(source);
        folding_ranges(source, &li)
    }

    #[test]
    fn empty_module_yields_no_ranges() {
        assert!(ranges_for("module pkg/main;\n").is_empty());
    }

    #[test]
    fn single_fn_body_yields_one_range() {
        let source = concat!(
            "module pkg/main;\n",
            "pub fn run() -> . {\n",
            "    ()\n",
            "}\n",
        );
        let ranges = ranges_for(source);
        assert_eq!(ranges.len(), 1, "expected one range for the fn body brace");
        // The brace group spans lines 1..3 (0-indexed).
        assert_eq!(ranges[0].start_line, 1);
        assert_eq!(ranges[0].end_line, 3);
        assert_eq!(ranges[0].kind, Some(FoldingRangeKind::Region));
    }

    #[test]
    fn nested_braces_yield_multiple_ranges() {
        // A function body with an inner conditional brace.
        let source = concat!(
            "module pkg/main;\n",
            "pub fn f(x: Bool) -> . {\n",
            "    let r = if true {\n",
            "        ()\n",
            "    } else {\n",
            "        ()\n",
            "    };\n",
            "    r\n",
            "}\n",
        );
        let ranges = ranges_for(source);
        // Outer fn body + inner match body = 2 brace groups.
        assert!(
            ranges.len() >= 2,
            "expected at least 2 ranges for nested braces; got {:?}",
            ranges
        );
    }

    #[test]
    fn single_line_brace_not_emitted() {
        // A one-line fn body `{ () }` doesn't fold usefully.
        let source = "module pkg/main;\npub fn run() -> . { () }\n";
        let ranges = ranges_for(source);
        assert!(
            ranges.is_empty(),
            "single-line brace groups must not produce a folding range; got {:?}",
            ranges
        );
    }

    #[test]
    fn recovered_brace_still_produces_range() {
        // Unclosed brace: `{` with no matching `}`. The skeleton
        // recovers it, synthesizing a close at EOF. If the content
        // spans multiple lines the range must still appear.
        let source = "module pkg/main;\npub fn run() -> . {\n    ()\n";
        let ranges = ranges_for(source);
        // Recovered brace: skeleton marks it but we still emit the
        // range because the start and end lines differ.
        assert!(
            !ranges.is_empty(),
            "recovered brace group must still yield a folding range"
        );
    }
}
