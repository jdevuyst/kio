//! LSP `textDocument/documentSymbol` handler.
//!
//! Walks a parsed `Module<Surface>` and returns a flat
//! `DocumentSymbol` list — one entry per top-level declaration in source
//! order. A recursive group contributes one function symbol per member. The
//! result carries:
//!
//! - `name`: the declared identifier.
//! - `kind`: an LSP `SymbolKind` mapped from the item variant; see
//!   the constant table [`KIND_TABLE`] for the mapping rationale.
//! - `range`: the full item span (from the leading keyword through
//!   the trailing `;` / `}`).
//! - `selectionRange`: the name identifier's span (the range an
//!   editor reveals when the user picks the symbol from the outline).
//!   When a precise `name_span` is not stored in the AST node (e.g.
//!   `FnDef`, `TypeAlias`, `Newtype`), [`find_name_in_source`] searches
//!   the source text within the item span to locate the name.
//! - `children`: empty for v1 (flat outline).
//!
//! Both `range` and `selectionRange` are clamped so `selectionRange`
//! is always contained by `range`, as the LSP spec requires.
//!
//! ## SymbolKind mapping table
//!
//! The mapping is recorded here so the kio-kio LSP reimplementation
//! can mirror it consistently:
//!
//! | Kio item    | LSP `SymbolKind`  | Rationale                              |
//! |-------------|-------------------|----------------------------------------|
//! | `fn`        | `FUNCTION`        | Direct equivalent.                     |
//! | `rec fn`    | `FUNCTION`        | Each recursive member is a function.   |
//! | `type` | `INTERFACE`       | Type alias is closest to an abstract   |
//! |             |                   | shape (not a value, not a concrete     |
//! |             |                   | type). `TYPE_PARAMETER` is too narrow; |
//! |             |                   | `CLASS` is too concrete.               |
//! | `literal`   | `CONSTANT`        | A named compile-time literal value.    |
//! | `newtype`   | `STRUCT`          | Newtype is a nominal wrapper with a    |
//! |             |                   | constructor and projector — structurally|
//! |             |                   | analogous to a struct.                 |
//! | `labels`    | `ENUM`            | A `labels` block declares a family of  |
//! |             |                   | named injections, the closest          |
//! |             |                   | LSP analogue being an enum.            |
//! | `type {x}`  | `ENUM_MEMBER`     | A label within a nominal family.       |
//! | `equiv`     | `OPERATOR`        | An equivalence claim is a proof        |
//! |             |                   | obligation / property, which has no    |
//! |             |                   | perfect LSP match; `OPERATOR` signals  |
//! |             |                   | "not a value, not a type, a relation". |
//! | `op`        | `OPERATOR`        | Direct equivalent.                     |

use lsp_types::{DocumentSymbol, Range, SymbolKind};

use crate::ast::{Item, Module, Surface};
use crate::lsp::positions::LineIndex;
use crate::pass::lexer::{self, Token, TokenKind};
use crate::span::Span;

/// Walk `module`'s items and return a `DocumentSymbol` list in
/// source order. `source` is the text the module was parsed from;
/// `line_index` maps byte offsets to LSP positions.
///
/// Items whose name cannot be located in the source (malformed /
/// recovered parse) fall back to the item's full span for both
/// `range` and `selectionRange`.
pub fn document_symbols(
    module: &Module<Surface>,
    source: &str,
    line_index: &LineIndex,
) -> Vec<DocumentSymbol> {
    let tokens = lexer::lex(source).ok();
    let mut symbols = Vec::new();
    for item in &module.items {
        if let Item::RecGroup(group, _) = item {
            symbols.extend(group.members.iter().map(|member| {
                function_symbol(
                    &member.name,
                    member.meta.span,
                    source,
                    line_index,
                    tokens.as_deref(),
                )
            }));
        } else if let Item::TypeRecGroup(group) = item {
            for member in &group.members {
                let symbol = match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => {
                        let selection = find_name_in_source(source, &alias.name, alias.meta.span);
                        named_symbol(
                            alias.name.clone(),
                            SymbolKind::INTERFACE,
                            alias.meta.span,
                            selection,
                            line_index,
                        )
                    }
                    crate::ast::TypeRecMember::Newtype(newtype) => {
                        let selection =
                            find_name_in_source(source, &newtype.name, newtype.meta.span);
                        named_symbol(
                            newtype.name.clone(),
                            SymbolKind::STRUCT,
                            newtype.meta.span,
                            selection,
                            line_index,
                        )
                    }
                    crate::ast::TypeRecMember::Labels(labels, _) => {
                        let Some(name) = &labels.type_alias_name else {
                            continue;
                        };
                        let selection = labels
                            .type_alias_span
                            .unwrap_or_else(|| find_name_in_source(source, name, labels.meta.span));
                        named_symbol(
                            name.clone(),
                            SymbolKind::ENUM,
                            labels.meta.span,
                            selection,
                            line_index,
                        )
                    }
                };
                symbols.push(symbol);
            }
        } else if let Some(symbol) = item_to_symbol(item, source, line_index, tokens.as_deref()) {
            symbols.push(symbol);
        }
    }
    symbols
}

/// Convert one top-level item to a `DocumentSymbol`. Returns `None`
/// for items that parse OK but carry no user-visible name (should
/// not happen in practice — every `Item` variant has a name).
fn item_to_symbol(
    item: &Item<Surface>,
    source: &str,
    line_index: &LineIndex,
    tokens: Option<&[Token]>,
) -> Option<DocumentSymbol> {
    let (name, kind, full_span, sel_span) = match item {
        Item::FnDef(f) => {
            return Some(function_symbol(
                &f.name,
                f.meta.span,
                source,
                line_index,
                tokens,
            ));
        }
        Item::TypeAlias(a) => {
            let sel = find_name_in_source(source, &a.name, a.meta.span);
            (a.name.clone(), SymbolKind::INTERFACE, a.meta.span, sel)
        }
        Item::LiteralAlias(l, _) => {
            let sel = find_name_in_source(source, &l.name, l.meta.span);
            (l.name.clone(), SymbolKind::CONSTANT, l.meta.span, sel)
        }
        Item::Newtype(n) => {
            let sel = find_name_in_source(source, &n.name, n.meta.span);
            (n.name.clone(), SymbolKind::STRUCT, n.meta.span, sel)
        }
        Item::Labels(t, _) => {
            // Named form carries a `type_alias_span`; anonymous form
            // uses the whole item's span as a fallback.
            if let Some(name) = &t.type_alias_name {
                let sel = t
                    .type_alias_span
                    .unwrap_or_else(|| find_name_in_source(source, name, t.meta.span));
                (name.clone(), SymbolKind::ENUM, t.meta.span, sel)
            } else {
                // Anonymous `labels { … };` — no declared name, skip.
                return None;
            }
        }
        Item::LabelForward(forward, _) => (
            format!("{{{}}}", forward.name),
            SymbolKind::ENUM_MEMBER,
            forward.meta.span,
            forward.name_span,
        ),
        Item::Equiv(e, _) => (
            e.name.clone(),
            SymbolKind::OPERATOR,
            e.meta.span,
            e.name_span,
        ),
        Item::Elaborator(s, _) => (
            s.name.clone(),
            SymbolKind::FUNCTION,
            s.meta.span,
            s.name_span,
        ),
        Item::RecGroup(..) => return None,
        Item::TypeRecGroup(_) => return None,
        Item::Op(op, _) => {
            // `op` items name their bound function, not a Kio name.
            // Use the operator pattern as a display name.
            let name = op_display_name(op);
            let sel = find_name_in_source(source, &name, op.meta.span);
            (name, SymbolKind::OPERATOR, op.meta.span, sel)
        }
        Item::VariadicOperator(operator, _) => {
            let name = variadic_display_name(operator);
            let sel = find_name_in_source(source, "varop", operator.meta.span);
            (name, SymbolKind::OPERATOR, operator.meta.span, sel)
        }
        Item::HostType(h) => {
            let sel = find_name_in_source(source, &h.name, h.meta.span);
            (h.name.clone(), SymbolKind::INTERFACE, h.meta.span, sel)
        }
        Item::HostFn(h) => {
            let sel = find_name_in_source(source, &h.name, h.meta.span);
            (h.name.clone(), SymbolKind::FUNCTION, h.meta.span, sel)
        }
    };

    let range = span_to_range(full_span, line_index);
    // `selectionRange` must be contained by `range`.
    let sel_clamped = clamp_span(sel_span, full_span);
    let selection_range = span_to_range(sel_clamped, line_index);

    #[allow(deprecated)]
    Some(DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    })
}

fn named_symbol(
    name: String,
    kind: SymbolKind,
    full_span: Span,
    selection_span: Span,
    line_index: &LineIndex,
) -> DocumentSymbol {
    #[allow(deprecated)]
    DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: span_to_range(full_span, line_index),
        selection_range: span_to_range(clamp_span(selection_span, full_span), line_index),
        children: None,
    }
}

fn function_symbol(
    name: &str,
    span: Span,
    source: &str,
    line_index: &LineIndex,
    tokens: Option<&[Token]>,
) -> DocumentSymbol {
    let selection_span = tokens
        .and_then(|tokens| find_function_name_in_tokens(tokens, name, span))
        .unwrap_or_else(|| find_name_in_source(source, name, span));
    let range = span_to_range(span, line_index);
    let selection_range = span_to_range(clamp_span(selection_span, span), line_index);

    #[allow(deprecated)]
    DocumentSymbol {
        name: name.to_owned(),
        detail: None,
        kind: SymbolKind::FUNCTION,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    }
}

fn find_function_name_in_tokens(tokens: &[Token], name: &str, item_span: Span) -> Option<Span> {
    let start = tokens.partition_point(|token| token.span.end <= item_span.start);
    let end = start + tokens[start..].partition_point(|token| token.span.start < item_span.end);
    tokens[start..end]
        .windows(2)
        .find_map(|pair| match (&pair[0].kind, &pair[1].kind) {
            (TokenKind::Ident(keyword), TokenKind::Ident(candidate))
                if keyword == "fn" && candidate == name =>
            {
                Some(pair[1].span)
            }
            _ => None,
        })
}

/// Compute a rough display name for an `op` item. The operator's
/// pattern tokens are joined with spaces; the bound function name
/// is omitted (it's captured by the `FnDef` symbol separately).
///
/// Example: `op _ + _ { impl add }` → `"op _ + _"`.
fn op_display_name(op: &crate::ast::Op) -> String {
    use crate::ast::{OpBody, OpPart};
    match &op.body {
        OpBody::Normal { pattern, .. } => {
            let parts: Vec<&str> = pattern
                .iter()
                .map(|p| match p {
                    OpPart::SlotPlain { .. } | OpPart::SlotRecursive { .. } => "_",
                    OpPart::SlotGreedy { .. } => "___",
                    OpPart::Token { content, .. } => content.as_str(),
                })
                .collect();
            format!("op {}", parts.join(" "))
        }
    }
}

fn variadic_display_name(operator: &crate::ast::VariadicOperator) -> String {
    let open = operator.open.join("");
    let close = operator.spec.close.join("");
    format!("varop {open} {close}")
}

/// Search for `name` in `source` starting from `item_span.start`.
/// The search scans for an exact word-boundary match of `name`
/// within the item's span. Returns the name's span if found;
/// falls back to `item_span` otherwise (so `selectionRange` is
/// at least `range`-containable).
///
/// This heuristic covers the common case (name follows the keyword
/// and any `pub` modifier) without needing dedicated `name_span`
/// fields in AST nodes that don't store one.
fn find_name_in_source(source: &str, name: &str, item_span: Span) -> Span {
    let start = item_span.start as usize;
    let end = item_span.end as usize;
    let slice = &source[start.min(source.len())..end.min(source.len())];
    // Walk the slice looking for `name` bounded by non-identifier chars.
    let name_bytes = name.as_bytes();
    let slice_bytes = slice.as_bytes();
    let mut i = 0;
    while i + name.len() <= slice.len() {
        if &slice_bytes[i..i + name.len()] == name_bytes {
            // Check left boundary: either at start of slice or
            // preceded by a non-identifier character.
            let left_ok = i == 0 || !is_ident_char(slice_bytes[i - 1]);
            // Check right boundary: either at end of slice or
            // followed by a non-identifier character.
            let right_pos = i + name.len();
            let right_ok = right_pos >= slice.len() || !is_ident_char(slice_bytes[right_pos]);
            if left_ok && right_ok {
                let abs_start = (start + i) as u32;
                let abs_end = (start + i + name.len()) as u32;
                return Span::new(abs_start, abs_end);
            }
        }
        i += 1;
    }
    // Fallback: use the item span itself.
    item_span
}

/// Whether a byte is an ASCII identifier character (`[a-zA-Z0-9_]`).
fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Clamp `sel` so it is contained by `full`. If `sel` is already
/// inside `full` this is a no-op; if not (e.g. the search fell back
/// to the item span), return `full`.
fn clamp_span(sel: Span, full: Span) -> Span {
    if sel.start >= full.start && sel.end <= full.end {
        sel
    } else {
        full
    }
}

/// Convert a byte-offset `Span` to an LSP `Range`.
fn span_to_range(span: Span, line_index: &LineIndex) -> Range {
    let r = line_index.to_range(span);
    Range {
        start: lsp_types::Position {
            line: r.start.line,
            character: r.start.character,
        },
        end: lsp_types::Position {
            line: r.end.line,
            character: r.end.character,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::parser::{parse, parse_lazy};

    fn symbols_for(source: &str) -> Vec<DocumentSymbol> {
        let module = parse(source).expect("parse");
        let line_index = LineIndex::new(source);
        document_symbols(&module, source, &line_index)
    }

    fn lazy_symbols_for(source: &str) -> Vec<DocumentSymbol> {
        let module = parse_lazy(source).expect("lazy parse");
        let line_index = LineIndex::new(source);
        document_symbols(module.module(), source, &line_index)
    }

    #[test]
    fn empty_module_yields_no_symbols() {
        let syms = symbols_for("module pkg/main;\n");
        assert!(syms.is_empty());
    }

    #[test]
    fn variadic_symbol_uses_current_syntax_and_exact_keyword_span() {
        let source = "module pkg;\nvarop [* *] { foldr step empty }\n";
        for symbols in [symbols_for(source), lazy_symbols_for(source)] {
            assert_eq!(symbols.len(), 1);
            assert_eq!(symbols[0].name, "varop [* *]");
            assert_eq!(symbols[0].kind, SymbolKind::OPERATOR);
            assert_eq!(
                symbols[0].selection_range.start,
                lsp_types::Position::new(1, 0)
            );
            assert_eq!(
                symbols[0].selection_range.end,
                lsp_types::Position::new(1, 5)
            );
        }
    }

    #[test]
    fn forwarded_label_symbol_keeps_its_namespace_and_exact_name_span() {
        let source = "module pkg;\nfn field() -> . { () }\ntype {field} = {original};\n";
        for symbols in [symbols_for(source), lazy_symbols_for(source)] {
            assert_eq!(symbols.len(), 2);
            assert_eq!(symbols[0].name, "field");
            assert_eq!(symbols[0].kind, SymbolKind::FUNCTION);
            assert_eq!(symbols[1].name, "{field}");
            assert_eq!(symbols[1].kind, SymbolKind::ENUM_MEMBER);
            assert_eq!(
                symbols[1].selection_range.start,
                lsp_types::Position::new(2, 6)
            );
            assert_eq!(
                symbols[1].selection_range.end,
                lsp_types::Position::new(2, 11)
            );
        }
    }

    #[test]
    fn fn_item_maps_to_function_kind() {
        let source = "module pkg/main;\npub fn run() -> . { () }\n";
        let syms = symbols_for(source);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "run");
        assert_eq!(syms[0].kind, SymbolKind::FUNCTION);
        // selectionRange must be within range.
        assert!(syms[0].selection_range.start >= syms[0].range.start);
        assert!(syms[0].selection_range.end <= syms[0].range.end);
    }

    #[test]
    fn rec_group_contributes_one_function_symbol_per_member() {
        let source = concat!(
            "module pkg/main;\n",
            "rec(loop) {\n",
            "  fn first(value: .) -> . { rec second(value) };\n",
            "  pub fn second(value: .) -> . { rec first(value) }\n",
            "}\n",
        );
        let symbols = symbols_for(source);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name, "first");
        assert_eq!(symbols[0].kind, SymbolKind::FUNCTION);
        assert_eq!(symbols[1].name, "second");
        assert_eq!(symbols[1].kind, SymbolKind::FUNCTION);
        assert!(symbols.iter().all(|symbol| {
            symbol.selection_range.start >= symbol.range.start
                && symbol.selection_range.end <= symbol.range.end
        }));
        assert_ne!(symbols[0].range, symbols[1].range);
    }

    #[test]
    fn singleton_rec_shorthand_and_braced_form_have_the_same_symbol_shape() {
        let shorthand = symbols_for(concat!(
            "module pkg/main;\n",
            "rec(loop) fn visit(value: .) -> . { rec visit(value) }\n",
        ));
        let braced = symbols_for(concat!(
            "module pkg/main;\n",
            "rec(loop) {\n",
            "  fn visit(value: .) -> . { rec visit(value) }\n",
            "}\n",
        ));

        assert_eq!(shorthand.len(), 1);
        assert_eq!(braced.len(), 1);
        assert_eq!(shorthand[0].name, braced[0].name);
        assert_eq!(shorthand[0].kind, braced[0].kind);
    }

    #[test]
    fn lazy_rec_member_selection_ignores_matching_scoped_visibility_path() {
        let source = concat!(
            "module pkg/main;\n",
            "rec(loop) {\n",
            "  pub(pkg) fn pkg(value: .) -> . { let broken = ; rec pkg(value) }\n",
            "}\n",
        );

        let symbols = lazy_symbols_for(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "pkg");
        assert_eq!(symbols[0].selection_range.start.line, 2);
        assert_eq!(symbols[0].selection_range.start.character, 14);
        assert_eq!(symbols[0].selection_range.end.line, 2);
        assert_eq!(symbols[0].selection_range.end.character, 17);
    }

    #[test]
    fn multiple_items_in_source_order() {
        let source = concat!(
            "module pkg/main;\n",
            "pub fn foo() -> . { () }\n",
            "type Bar[A] = A;\n",
            "literal answer = 42;\n",
            "newtype Baz[A] : A { constructor mk_baz; projector un_baz }\n",
        );
        let syms = symbols_for(source);
        assert_eq!(syms.len(), 4);
        assert_eq!(syms[0].name, "foo");
        assert_eq!(syms[0].kind, SymbolKind::FUNCTION);
        assert_eq!(syms[1].name, "Bar");
        assert_eq!(syms[1].kind, SymbolKind::INTERFACE);
        assert_eq!(syms[2].name, "answer");
        assert_eq!(syms[2].kind, SymbolKind::CONSTANT);
        assert_eq!(syms[3].name, "Baz");
        assert_eq!(syms[3].kind, SymbolKind::STRUCT);
    }

    #[test]
    fn find_name_in_source_locates_fn_name() {
        let source = "pub fn run() -> . { () }";
        let span = Span::new(0, source.len() as u32);
        let name_span = find_name_in_source(source, "run", span);
        assert_ne!(name_span, span, "should have found the exact name span");
        let name = &source[name_span.start as usize..name_span.end as usize];
        assert_eq!(name, "run");
    }

    #[test]
    fn find_name_falls_back_for_missing_name() {
        // Gibberish source: name "xyz" not present → fallback = item_span.
        let source = "pub fn foo() -> . { () }";
        let span = Span::new(0, source.len() as u32);
        let name_span = find_name_in_source(source, "xyz", span);
        assert_eq!(name_span, span);
    }

    #[test]
    fn selection_range_is_contained_by_range() {
        let source = concat!(
            "module pkg/main;\n",
            "pub fn run() -> . { () }\n",
            "type Same[A] = A;\n",
        );
        let syms = symbols_for(source);
        for sym in &syms {
            assert!(
                sym.selection_range.start >= sym.range.start,
                "selectionRange.start must be >= range.start for {}",
                sym.name
            );
            assert!(
                sym.selection_range.end <= sym.range.end,
                "selectionRange.end must be <= range.end for {}",
                sym.name
            );
        }
    }
}
