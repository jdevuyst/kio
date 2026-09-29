//! LSP `textDocument/inlayHint` handler.
//!
//! Inlay hints are read from the typechecker's position index. The
//! handler does not perform its own AST walk: request frequency is high,
//! so it answers from the latest typed snapshot and returns `None` when
//! no snapshot is available.

use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Position, Range, Uri};

use crate::cmd::check::LspAnalysis;
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::pass::typecheck_core::write_type;

pub fn handle_inlay_hints(
    uri: &Uri,
    range: &Range,
    analysis: &LspAnalysis,
    overlay_text: Option<&str>,
) -> Option<Vec<InlayHint>> {
    let canonical = crate::lsp::util::uri_to_canonical(uri)?;
    let module_path = analysis.file_to_module.get(canonical.as_path())?;
    let source = overlay_text
        .or_else(|| {
            analysis
                .sources
                .get(canonical.as_path())
                .map(|s| s.as_str())
        })
        .or_else(|| analysis.sources.get(&canonical).map(|s| s.as_str()))?;
    let line_index = LineIndex::new(source);
    let requested = RequestedRange::from_lsp(range, &line_index);

    let mut hints = Vec::new();
    for ((entry_module_path, name_span), ty) in analysis.position_index.inlay_let_types_iter() {
        if entry_module_path != module_path || !requested.contains(name_span.end) {
            continue;
        }
        let mut rendered = String::from(": ");
        write_type(
            &analysis
                .position_index
                .display_let_type(module_path, *name_span, ty),
            &mut rendered,
        );
        hints.push(type_hint(name_span.end, rendered, &line_index));
    }
    for (callee_end, tys) in analysis
        .position_index
        .display_call_type_arguments(module_path)
    {
        if !requested.contains(callee_end.start) {
            continue;
        }
        let rendered = render_type_args(&tys);
        if !rendered.is_empty() {
            hints.push(type_hint(callee_end.start, rendered, &line_index));
        }
    }
    hints.sort_by_key(|hint| {
        (
            hint.position.line,
            hint.position.character,
            hint.label_string(),
        )
    });
    Some(hints)
}

fn render_type_args<P>(tys: &[crate::ast::Type<P>]) -> String
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    if tys.is_empty() {
        return String::new();
    }
    let mut out = String::from("[");
    for (index, ty) in tys.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write_type(ty, &mut out);
    }
    out.push(']');
    out
}

fn type_hint(byte_offset: u32, label: String, line_index: &LineIndex) -> InlayHint {
    let pos = line_index.to_position(byte_offset);
    InlayHint {
        position: Position {
            line: pos.line,
            character: pos.character,
        },
        label: InlayHintLabel::String(label),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: Some(false),
        padding_right: Some(false),
        data: None,
    }
}

trait InlayHintLabelKey {
    fn label_string(&self) -> String;
}

impl InlayHintLabelKey for InlayHint {
    fn label_string(&self) -> String {
        match &self.label {
            InlayHintLabel::String(s) => s.clone(),
            InlayHintLabel::LabelParts(parts) => {
                let mut out = String::new();
                for part in parts {
                    out.push_str(&part.value);
                }
                out
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RequestedRange {
    start: u32,
    end: u32,
}

impl RequestedRange {
    fn from_lsp(range: &Range, line_index: &LineIndex) -> Self {
        let start = line_index.position_to_offset(LspPosition {
            line: range.start.line,
            character: range.start.character,
        });
        let end = line_index.position_to_offset(LspPosition {
            line: range.end.line,
            character: range.end.character,
        });
        Self { start, end }
    }

    fn contains(self, offset: u32) -> bool {
        self.start <= offset && offset <= self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Lowered, Meta, Type};
    use crate::span::Span;

    fn unit_ty() -> Type<Lowered> {
        Type::Unit {
            meta: Meta::new(Span::new(0, 0)),
        }
    }

    #[test]
    fn render_type_arg_group() {
        assert_eq!(render_type_args(&[unit_ty(), unit_ty()]), "[., .]");
    }

    #[test]
    fn requested_range_filters_offsets() {
        let source = "abc\ndef\n";
        let line_index = LineIndex::new(source);
        let range = Range {
            start: Position {
                line: 1,
                character: 0,
            },
            end: Position {
                line: 1,
                character: 3,
            },
        };
        let requested = RequestedRange::from_lsp(&range, &line_index);
        assert!(!requested.contains(2));
        assert!(requested.contains(4));
        assert!(requested.contains(7));
    }
}
