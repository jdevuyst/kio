//! Semantic-token classification for `textDocument/semanticTokens/full`.
//!
//! Produces a per-token semantic classification so editors can colour
//! identifiers by their semantic kind (function vs. type vs. variable
//! etc.), going beyond what a static grammar can know.
//!
//! ## Legend
//!
//! The legend is the contract between the server and the client.
//! Every type and modifier is listed by *index*; the numeric values
//! in the encoded token array refer to positions in these lists.
//! The ordering is therefore a **breaking change** — modify only by
//! appending to the end.
//!
//! Token types (indices 0..):
//!   0  `namespace`        — module path segments
//!   1  `type`             — type definition names (`newtype`, type alias)
//!   2  `typeParameter`    — type-parameter binders `[A]`
//!   3  `parameter`        — value-parameter binders
//!   4  `variable`         — let-bound and other value binders
//!   5  `function`         — function definition names and call-site callees
//!   6  `enumMember`       — label entry names (`foo:` inside `labels { … }`)
//!   7  `keyword`          — language keywords
//!   8  `comment`          — line comments
//!   9  `string`           — string literals
//!  10  `number`           — numeric literals
//!  11  `operator`         — operators (builtin and user-defined)
//!
//! Token modifiers (bit indices 0..):
//!   0  `declaration`      — the definition site of a binding; omitted from
//!                           `foo: _` label-reuse references
//!   1  `defaultLibrary`   — items supplied by the host
//!   2  `readonly`         — every Kio binder is immutable (applied
//!                           to parameters, let-bindings, and fn defs)
//!
//! ## Stage A — source-token classification
//!
//! Walk the classified token stream produced by [`crate::tokens`]
//! and map each [`TokenKind`] to a `(token_type, token_modifiers)` pair.
//! The lexer classifies non-identifier tokens; a surface AST supplies
//! identifier roles when parsing succeeds, including context-aware parses of
//! imported operator syntax. Generic identifiers remain `variable`.
//!
//! Comments are handled specially: the token stream already
//! lifts comment trivia into first-class tokens, so no extra pass is
//! needed.
//!
//! ## Encoder
//!
//! LSP semantic tokens are delta-encoded: each token's `(line, col)`
//! is expressed as a delta from the *previous* token's position, not
//! from the file start. [`encode`] does a single linear pass over a
//! sorted `(span, type_idx, modifier_mask)` triple list and produces
//! the flat `u32` array the LSP `SemanticTokens.data` field expects.

use crate::ast::{Module, Surface};
use crate::lsp::positions::LineIndex;
use crate::tokens::{TokenKind, dump, dump_with_module};
use lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensServerCapabilities,
};

// ── Legend ───────────────────────────────────────────────────────────────────

/// The ordered list of token types the server uses.
///
/// **Do not reorder.** Append-only: existing indices are part of the
/// client/server contract. Each position matches the numeric
/// `tokenType` in the encoded array.
pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::NAMESPACE,      // 0
    SemanticTokenType::TYPE,           // 1
    SemanticTokenType::TYPE_PARAMETER, // 2
    SemanticTokenType::PARAMETER,      // 3
    SemanticTokenType::VARIABLE,       // 4
    SemanticTokenType::FUNCTION,       // 5
    SemanticTokenType::ENUM_MEMBER,    // 6
    SemanticTokenType::KEYWORD,        // 7
    SemanticTokenType::COMMENT,        // 8
    SemanticTokenType::STRING,         // 9
    SemanticTokenType::NUMBER,         // 10
    SemanticTokenType::OPERATOR,       // 11
];

/// The ordered list of token modifiers the server uses.
///
/// **Do not reorder.** Append-only. Each position corresponds to one
/// bit in the `tokenModifiers` bitset.
pub const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::DECLARATION,     // bit 0
    SemanticTokenModifier::DEFAULT_LIBRARY, // bit 1
    SemanticTokenModifier::READONLY,        // bit 2
];

// ── Type-index constants ─────────────────────────────────────────────────────

const TT_NAMESPACE: u32 = 0;
const TT_TYPE: u32 = 1;
const TT_PARAMETER: u32 = 3;
const TT_VARIABLE: u32 = 4;
const TT_FUNCTION: u32 = 5;
const TT_ENUM_MEMBER: u32 = 6;
const TT_KEYWORD: u32 = 7;
const TT_COMMENT: u32 = 8;
const TT_STRING: u32 = 9;
const TT_NUMBER: u32 = 10;
const TT_OPERATOR: u32 = 11;

// ── Modifier-bit constants ───────────────────────────────────────────────────

const MOD_DECLARATION: u32 = 1 << 0;
const MOD_READONLY: u32 = 1 << 2;

// ── Capability advertisement ─────────────────────────────────────────────────

/// Build the `SemanticTokensServerCapabilities` value to embed in the
/// `initialize` response.
///
/// Advertises:
/// - The legend (token types + modifiers).
/// - `full: true` — the server supports `textDocument/semanticTokens/full`.
/// - `range: false` — partial-range requests are not implemented in v1.
pub fn server_capabilities() -> SemanticTokensServerCapabilities {
    SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
        legend: SemanticTokensLegend {
            token_types: TOKEN_TYPES.to_vec(),
            token_modifiers: TOKEN_MODIFIERS.to_vec(),
        },
        full: Some(SemanticTokensFullOptions::Bool(true)),
        range: Some(false),
        ..Default::default()
    })
}

// ── Classification helper ────────────────────────────────────────────────────

/// Map one [`TokenKind`] to `(token_type_index, modifier_bitset)`.
///
/// Returns `None` for token kinds that LSP clients don't expect to
/// receive as semantic tokens (e.g. punctuation, brackets, separators,
/// slots — these are already handled by the grammar-level
/// highlighter and adding them here would just produce duplicate
/// coverage with no semantic gain).
fn classify_token_kind(kind: TokenKind) -> Option<(u32, u32)> {
    match kind {
        // Keywords — control flow and declarations both map to `keyword`.
        TokenKind::KeywordControl | TokenKind::KeywordDeclaration => Some((TT_KEYWORD, 0)),
        TokenKind::KeywordElaborator => Some((TT_KEYWORD, 0)),

        // Comments
        TokenKind::CommentLine | TokenKind::CommentDoc => Some((TT_COMMENT, 0)),

        // Literals
        TokenKind::LiteralString => Some((TT_STRING, 0)),
        TokenKind::LiteralNumber => Some((TT_NUMBER, 0)),
        TokenKind::LiteralBool => {
            // Booleans are keyword-like in most editors' colour schemes.
            Some((TT_KEYWORD, 0))
        }

        // Operators
        TokenKind::OperatorBuiltin | TokenKind::OperatorUser => Some((TT_OPERATOR, 0)),

        // Entity names — distinguish definitions from call-site references so
        // declaration modifiers never leak onto uses.
        TokenKind::EntityNameFunction => Some((TT_FUNCTION, MOD_DECLARATION | MOD_READONLY)),
        TokenKind::EntityNameFunctionReference => Some((TT_FUNCTION, MOD_READONLY)),

        // Type definition names.
        TokenKind::EntityNameType => Some((TT_TYPE, MOD_DECLARATION | MOD_READONLY)),

        // Module path segments.
        TokenKind::EntityNameModule => Some((TT_NAMESPACE, 0)),

        // Label entry names (Kio's analogue of enum members).
        TokenKind::EntityNameLabel => Some((TT_ENUM_MEMBER, MOD_DECLARATION)),
        TokenKind::EntityNameLabelReference => Some((TT_ENUM_MEMBER, 0)),
        TokenKind::EntityNameQualifiedLabelReference => Some((TT_ENUM_MEMBER, 0)),

        // Parameter binders (value params and type params).
        TokenKind::VariableParameter => Some((TT_PARAMETER, MOD_DECLARATION | MOD_READONLY)),

        // Type parameters inside `[A]` brackets.  The token
        // stream re-uses `VariableParameter` for type-param binders; no
        // separate TokenKind exists today. If a distinct kind is ever
        // added we can promote them to slot 2 (`typeParameter`).
        // Purposely exhaustive fallthrough so the compiler enforces the
        // full match:

        // Plain identifiers — generic variables, let-bindings etc.
        // We emit them as `variable`; without Stage B resolution they
        // are all we can say.
        TokenKind::Identifier => Some((TT_VARIABLE, MOD_READONLY)),

        // Punctuation, brackets, separators, and slots
        // are not semantic tokens — they are handled by the grammar
        // highlighter and don't benefit from semantic override.
        TokenKind::PunctuationBracket | TokenKind::PunctuationSeparator | TokenKind::Slot => None,
    }
}

// ── Encoder ──────────────────────────────────────────────────────────────────

/// Encode a sorted sequence of `(start_byte, end_byte, type_index,
/// modifier_mask)` triples into the flat LSP delta-encoded `u32` array.
///
/// The input **must** be sorted by `start_byte` ascending — if not,
/// the delta values can underflow and produce garbage. The caller
/// (`semantic_tokens`) guarantees this because the token classifier
/// returns tokens in source order.
///
/// Each triple becomes five `u32`s in the output:
/// `[delta_line, delta_start, length, token_type, modifiers]`.
fn encode(
    triples: &[(u32, u32, u32, u32)], // (start, end, type_idx, modifiers)
    line_index: &LineIndex,
) -> Vec<SemanticToken> {
    let mut tokens = Vec::with_capacity(triples.len());
    let mut prev_line: u32 = 0;
    let mut prev_col: u32 = 0;

    for &(start, end, token_type, token_modifiers_bitset) in triples {
        let start_pos = line_index.to_position(start);
        let length = {
            // Compute the UTF-16 length of the token.
            // For tokens that don't span a newline (the vast majority),
            // the length is `end_pos.character - start_pos.character`.
            // For tokens that do span a newline (shouldn't happen for
            // any Kio token — no multi-line keywords or strings), we
            // clamp to a reasonable value rather than producing a
            // negative delta_start.
            let end_pos = line_index.to_position(end);
            if end_pos.line == start_pos.line {
                end_pos.character.saturating_sub(start_pos.character)
            } else {
                // Multi-line token: rare / impossible for Kio, but
                // don't panic — emit from start to end of the start
                // line as a best-effort length.
                let source = line_index.source();
                let line_slice = {
                    let line_start = start as usize;
                    // Advance to the end of the line.
                    let line_end = source[line_start..]
                        .find('\n')
                        .map(|i| line_start + i)
                        .unwrap_or(source.len());
                    &source[line_start..line_end]
                };
                line_slice.chars().map(|c| c.len_utf16()).sum::<usize>() as u32
            }
        };
        if length == 0 {
            // Zero-length tokens would confuse clients; skip them.
            continue;
        }

        let delta_line = start_pos.line - prev_line;
        let delta_start = if delta_line == 0 {
            start_pos.character - prev_col
        } else {
            start_pos.character
        };

        tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset,
        });

        prev_line = start_pos.line;
        prev_col = start_pos.character;
    }

    tokens
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Build the semantic tokens response for `source`.
///
/// Runs the token classifier over `source` (Stage A, lexer-based)
/// and returns a [`SemanticTokens`] value ready to serialize into the
/// LSP response body.
///
/// Returns `None` if lexing fails (the source contains a byte sequence
/// the lexer rejects entirely — extremely rare in practice, as the
/// lexer is total over ASCII and the source is user-typed Kio).
pub fn semantic_tokens(source: &str) -> Option<SemanticTokens> {
    let classified = dump(source).ok()?;
    semantic_tokens_from_classified(source, &classified)
}

/// Build semantic tokens from a provider-aware surface parse.
pub fn semantic_tokens_with_module(
    source: &str,
    module: &Module<Surface>,
) -> Option<SemanticTokens> {
    let classified = dump_with_module(source, module).ok()?;
    semantic_tokens_from_classified(source, &classified)
}

fn semantic_tokens_from_classified(
    source: &str,
    classified: &[crate::tokens::ClassifiedToken],
) -> Option<SemanticTokens> {
    let line_index = LineIndex::new(source);

    // Collect (start, end, type_idx, modifier_mask) triples for tokens
    // that have a semantic classification.
    let triples: Vec<(u32, u32, u32, u32)> = classified
        .iter()
        .filter_map(|t| {
            if let Some(triple) = elaborator_head_token(source, t) {
                return Some(triple);
            }
            let (type_idx, modifiers) = classify_token_kind(t.kind)?;
            Some((t.span.start, t.span.end, type_idx, modifiers))
        })
        .collect();

    let data = encode(&triples, &line_index);
    Some(SemanticTokens {
        result_id: None,
        data,
    })
}

fn elaborator_head_token(
    source: &str,
    token: &crate::tokens::ClassifiedToken,
) -> Option<(u32, u32, u32, u32)> {
    match token.kind {
        TokenKind::KeywordElaborator => {
            let text = source.get(token.span.start as usize..token.span.end as usize)?;
            let end = text
                .strip_suffix('!')
                .map(|head| token.span.start + head.len() as u32)
                .unwrap_or(token.span.end);
            if end > token.span.start {
                Some((token.span.start, end, TT_FUNCTION, MOD_READONLY))
            } else {
                None
            }
        }
        TokenKind::Identifier
        | TokenKind::EntityNameFunction
        | TokenKind::EntityNameFunctionReference
            if is_user_elaborator_head(source, token.span.end as usize) =>
        {
            Some((token.span.start, token.span.end, TT_FUNCTION, MOD_READONLY))
        }
        _ => None,
    }
}

fn is_user_elaborator_head(source: &str, head_end: usize) -> bool {
    if !source
        .get(head_end..)
        .is_some_and(|suffix| suffix.starts_with('!'))
    {
        return false;
    }

    let after_bang = head_end + 1;
    let Some(next) = source.get(after_bang..) else {
        return false;
    };
    let mut chars = next.chars().skip_while(|c| c.is_whitespace());
    matches!(chars.next(), Some('('))
}

// ── Type-parameter refinement note ───────────────────────────────────────────
//
// The token stream classifies type-parameter binders (`[A]`) as
// `VariableParameter`, the same kind it uses for value parameters.  The
// `classify_token_kind` function maps them both to `parameter` with
// `declaration | readonly` modifiers — indistinguishable at this layer.
//
// If / when the `TokenKind` taxonomy grows a `TypeParameter` variant, the
// `VariableParameter` arm above can be split and the type-param binders
// promoted to the `typeParameter` slot.

#[cfg(test)]
mod tests {
    use super::*;

    // ── Legend stability ──────────────────────────────────────────────────────

    #[test]
    fn legend_token_types_order_is_stable() {
        // Spot-check index positions against the table in the module doc.
        assert_eq!(TOKEN_TYPES[0], SemanticTokenType::NAMESPACE);
        assert_eq!(TOKEN_TYPES[1], SemanticTokenType::TYPE);
        assert_eq!(TOKEN_TYPES[2], SemanticTokenType::TYPE_PARAMETER);
        assert_eq!(TOKEN_TYPES[3], SemanticTokenType::PARAMETER);
        assert_eq!(TOKEN_TYPES[4], SemanticTokenType::VARIABLE);
        assert_eq!(TOKEN_TYPES[5], SemanticTokenType::FUNCTION);
        assert_eq!(TOKEN_TYPES[6], SemanticTokenType::ENUM_MEMBER);
        assert_eq!(TOKEN_TYPES[7], SemanticTokenType::KEYWORD);
        assert_eq!(TOKEN_TYPES[8], SemanticTokenType::COMMENT);
        assert_eq!(TOKEN_TYPES[9], SemanticTokenType::STRING);
        assert_eq!(TOKEN_TYPES[10], SemanticTokenType::NUMBER);
        assert_eq!(TOKEN_TYPES[11], SemanticTokenType::OPERATOR);
    }

    #[test]
    fn legend_token_modifiers_order_is_stable() {
        assert_eq!(TOKEN_MODIFIERS[0], SemanticTokenModifier::DECLARATION);
        assert_eq!(TOKEN_MODIFIERS[1], SemanticTokenModifier::DEFAULT_LIBRARY);
        assert_eq!(TOKEN_MODIFIERS[2], SemanticTokenModifier::READONLY);
    }

    // ── Classification ────────────────────────────────────────────────────────

    #[test]
    fn keywords_classify_as_keyword() {
        assert_eq!(
            classify_token_kind(TokenKind::KeywordControl),
            Some((TT_KEYWORD, 0))
        );
        assert_eq!(
            classify_token_kind(TokenKind::KeywordDeclaration),
            Some((TT_KEYWORD, 0))
        );
        assert_eq!(
            classify_token_kind(TokenKind::KeywordElaborator),
            Some((TT_KEYWORD, 0))
        );
    }

    #[test]
    fn comment_classifies_as_comment() {
        assert_eq!(
            classify_token_kind(TokenKind::CommentLine),
            Some((TT_COMMENT, 0))
        );
    }

    #[test]
    fn string_classifies_as_string() {
        assert_eq!(
            classify_token_kind(TokenKind::LiteralString),
            Some((TT_STRING, 0))
        );
    }

    #[test]
    fn number_classifies_as_number() {
        assert_eq!(
            classify_token_kind(TokenKind::LiteralNumber),
            Some((TT_NUMBER, 0))
        );
    }

    #[test]
    fn bool_classifies_as_keyword() {
        // Booleans are keyword-styled.
        assert_eq!(
            classify_token_kind(TokenKind::LiteralBool),
            Some((TT_KEYWORD, 0))
        );
    }

    #[test]
    fn operators_classify_as_operator() {
        assert_eq!(
            classify_token_kind(TokenKind::OperatorBuiltin),
            Some((TT_OPERATOR, 0))
        );
        assert_eq!(
            classify_token_kind(TokenKind::OperatorUser),
            Some((TT_OPERATOR, 0))
        );
    }

    #[test]
    fn entity_name_function_classifies_with_declaration_readonly() {
        let (ty, mods) = classify_token_kind(TokenKind::EntityNameFunction).unwrap();
        assert_eq!(ty, TT_FUNCTION);
        assert_ne!(
            mods & MOD_DECLARATION,
            0,
            "declaration modifier must be set"
        );
        assert_ne!(mods & MOD_READONLY, 0, "readonly modifier must be set");
    }

    #[test]
    fn entity_name_function_reference_classifies_readonly_without_declaration() {
        assert_eq!(
            classify_token_kind(TokenKind::EntityNameFunctionReference),
            Some((TT_FUNCTION, MOD_READONLY))
        );
    }

    #[test]
    fn entity_name_type_classifies_with_declaration_readonly() {
        let (ty, mods) = classify_token_kind(TokenKind::EntityNameType).unwrap();
        assert_eq!(ty, TT_TYPE);
        assert_ne!(mods & MOD_DECLARATION, 0);
        assert_ne!(mods & MOD_READONLY, 0);
    }

    #[test]
    fn entity_name_module_classifies_as_namespace() {
        let (ty, mods) = classify_token_kind(TokenKind::EntityNameModule).unwrap();
        assert_eq!(ty, TT_NAMESPACE);
        assert_eq!(mods, 0);
    }

    #[test]
    fn entity_name_label_classifies_as_enum_member_with_declaration() {
        let (ty, mods) = classify_token_kind(TokenKind::EntityNameLabel).unwrap();
        assert_eq!(ty, TT_ENUM_MEMBER);
        assert_ne!(mods & MOD_DECLARATION, 0);
    }

    #[test]
    fn entity_name_label_reference_has_no_declaration_modifier() {
        let (ty, mods) = classify_token_kind(TokenKind::EntityNameLabelReference).unwrap();
        assert_eq!(ty, TT_ENUM_MEMBER);
        assert_eq!(mods & MOD_DECLARATION, 0);
    }

    #[test]
    fn label_reuse_marker_is_a_reference_semantic_token() {
        let source = "module x; labels { field: . }; labels Row = { field: _, other: . };";
        let tokens = semantic_tokens(source).expect("semantic tokens");
        let labels: Vec<_> = tokens
            .data
            .iter()
            .filter(|token| token.token_type == TT_ENUM_MEMBER)
            .collect();
        assert_eq!(labels.len(), 3);
        assert_ne!(labels[0].token_modifiers_bitset & MOD_DECLARATION, 0);
        assert_eq!(labels[1].token_modifiers_bitset & MOD_DECLARATION, 0);
        assert_ne!(labels[2].token_modifiers_bitset & MOD_DECLARATION, 0);
    }

    #[test]
    fn selective_label_import_is_a_reference_semantic_token() {
        let source = "module x; import origin({field});";
        let tokens = semantic_tokens(source).expect("semantic tokens");
        let field = semantic_token_at(source, &tokens.data, "field").expect("field token");
        assert_eq!(field.token_type, TT_ENUM_MEMBER);
        assert_eq!(field.token_modifiers_bitset & MOD_DECLARATION, 0);
    }

    #[test]
    fn variable_parameter_classifies_as_parameter_with_declaration_readonly() {
        let (ty, mods) = classify_token_kind(TokenKind::VariableParameter).unwrap();
        assert_eq!(ty, TT_PARAMETER);
        assert_ne!(mods & MOD_DECLARATION, 0);
        assert_ne!(mods & MOD_READONLY, 0);
    }

    #[test]
    fn punctuation_and_structural_tokens_are_filtered_out() {
        assert_eq!(classify_token_kind(TokenKind::PunctuationBracket), None);
        assert_eq!(classify_token_kind(TokenKind::PunctuationSeparator), None);
        assert_eq!(classify_token_kind(TokenKind::Slot), None);
    }

    // ── Encoder ───────────────────────────────────────────────────────────────

    #[test]
    fn encoder_single_token_on_first_line() {
        // "fn" at bytes 0..2 — keyword on line 0, col 0.
        // classify_token_kind(KeywordDeclaration) → (TT_KEYWORD, 0).
        let line_index = LineIndex::new("fn foo");
        let triples = vec![(0u32, 2u32, TT_KEYWORD, 0u32)];
        let tokens = encode(&triples, &line_index);
        assert_eq!(tokens.len(), 1);
        let t = &tokens[0];
        assert_eq!(t.delta_line, 0);
        assert_eq!(t.delta_start, 0);
        assert_eq!(t.length, 2); // "fn" = 2 UTF-16 code units
        assert_eq!(t.token_type, TT_KEYWORD);
        assert_eq!(t.token_modifiers_bitset, 0);
    }

    #[test]
    fn encoder_two_tokens_same_line() {
        // "fn foo" — "fn" at 0..2 and "foo" at 3..6.
        let line_index = LineIndex::new("fn foo");
        let triples = vec![(0, 2, TT_KEYWORD, 0), (3, 6, TT_FUNCTION, MOD_DECLARATION)];
        let tokens = encode(&triples, &line_index);
        assert_eq!(tokens.len(), 2);
        let a = &tokens[0];
        assert_eq!(a.delta_line, 0);
        assert_eq!(a.delta_start, 0);
        assert_eq!(a.length, 2);
        let b = &tokens[1];
        assert_eq!(b.delta_line, 0);
        assert_eq!(b.delta_start, 3); // delta from col 0 to col 3
        assert_eq!(b.length, 3); // "foo"
        assert_eq!(b.token_type, TT_FUNCTION);
        assert_eq!(b.token_modifiers_bitset, MOD_DECLARATION);
    }

    #[test]
    fn encoder_tokens_on_different_lines() {
        // "fn\nbar" — "fn" on line 0, "bar" on line 1.
        let src = "fn\nbar";
        let line_index = LineIndex::new(src);
        let triples = vec![(0, 2, TT_KEYWORD, 0), (3, 6, TT_FUNCTION, 0)];
        let tokens = encode(&triples, &line_index);
        assert_eq!(tokens.len(), 2);
        let a = &tokens[0];
        assert_eq!(a.delta_line, 0);
        assert_eq!(a.delta_start, 0);
        let b = &tokens[1];
        assert_eq!(b.delta_line, 1); // one line below
        assert_eq!(b.delta_start, 0); // start of the new line
        assert_eq!(b.length, 3); // "bar"
    }

    #[test]
    fn encoder_zero_length_tokens_are_skipped() {
        // A token with start == end should not appear in the output.
        let line_index = LineIndex::new("fn");
        let triples = vec![(0, 0, TT_KEYWORD, 0), (0, 2, TT_KEYWORD, 0)];
        let tokens = encode(&triples, &line_index);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].length, 2);
    }

    // ── Full pipeline ─────────────────────────────────────────────────────────

    #[test]
    fn semantic_tokens_empty_source_yields_empty_data() {
        let result = semantic_tokens("").unwrap();
        assert!(result.data.is_empty());
    }

    #[test]
    fn semantic_tokens_keyword_is_classified() {
        let result = semantic_tokens("module demo;").unwrap();
        assert_eq!(result.data[0].token_type, TT_KEYWORD);
        let ordinary = semantic_tokens("fn").unwrap();
        assert_eq!(ordinary.data.len(), 1);
        assert_eq!(ordinary.data[0].token_type, TT_VARIABLE);
    }

    #[test]
    fn semantic_tokens_classify_marked_type_names_as_types() {
        let source =
            "module m; type Alias = .; type _Alias = Alias; fn id[T][_T](x: T) -> _Alias { x }";
        let tokens = semantic_tokens(source).expect("semantic tokens");
        for name in ["Alias", "_Alias"] {
            let alias = semantic_token_at(source, &tokens.data, name).expect("type token");
            assert_eq!(alias.token_type, TT_TYPE, "{name}");
        }
        for name in ["T", "_T"] {
            let type_param =
                semantic_token_at(source, &tokens.data, name).expect("type-parameter token");
            assert_eq!(type_param.token_type, TT_PARAMETER, "{name}");
        }
    }

    #[test]
    fn semantic_tokens_classify_raw_call_type_arguments_by_spelling() {
        let source = "module m; fn id[A](x: A) -> A { x } \
                      fn use_types[A][_A](x: A, y: _A) -> _A { \
                        let _ = id(A, x); id(_A, y) \
                      }";
        let tokens = semantic_tokens(source).expect("semantic tokens");
        for call in ["id(A, x)", "id(_A, y)"] {
            let byte = source.find(call).unwrap() + "id(".len();
            let token = semantic_token_at_byte(source, &tokens.data, byte as u32)
                .expect("raw call type-argument token");
            assert_eq!(token.token_type, TT_TYPE, "{call}");
        }
    }

    #[test]
    fn semantic_tokens_comment_is_classified() {
        let result = semantic_tokens("// hello").unwrap();
        // Should be one token, the comment, token_type = comment.
        let comment_tokens: Vec<_> = result
            .data
            .iter()
            .filter(|t| t.token_type == TT_COMMENT)
            .collect();
        assert!(
            !comment_tokens.is_empty(),
            "expected at least one comment token; got {:?}",
            result.data
        );
    }

    #[test]
    fn semantic_tokens_string_is_classified() {
        let result = semantic_tokens(r#""hello""#).unwrap();
        assert_eq!(result.data.len(), 1);
        assert_eq!(result.data[0].token_type, TT_STRING);
    }

    #[test]
    fn semantic_tokens_number_is_classified() {
        let result = semantic_tokens("42").unwrap();
        assert_eq!(result.data.len(), 1);
        assert_eq!(result.data[0].token_type, TT_NUMBER);
    }

    #[test]
    fn semantic_tokens_operator_is_classified() {
        // `+` is a user operator.
        let result = semantic_tokens("+").unwrap();
        assert_eq!(result.data.len(), 1);
        assert_eq!(result.data[0].token_type, TT_OPERATOR);
    }

    #[test]
    fn semantic_tokens_keep_bracket_operator_runs_maximal() {
        let result = semantic_tokens("[! ]] ]- ][*").unwrap();
        assert_eq!(result.data.len(), 4);
        assert!(
            result
                .data
                .iter()
                .all(|token| token.token_type == TT_OPERATOR)
        );
        assert_eq!(
            result
                .data
                .iter()
                .map(|token| token.length)
                .collect::<Vec<_>>(),
            vec![2, 2, 2, 3]
        );
    }

    #[test]
    fn semantic_tokens_elaborator_call_heads_are_uniform_functions() {
        let src = "module m;\nfn f(x: .) -> . { fit!(x, .); custom_elaborator!(x, .) }\n";
        let result = semantic_tokens(src).unwrap();
        let fit = semantic_token_at(src, &result.data, "fit").expect("fit token");
        let custom = semantic_token_at(src, &result.data, "custom_elaborator")
            .expect("custom_elaborator token");

        assert_eq!(fit.token_type, TT_FUNCTION);
        assert_eq!(custom.token_type, TT_FUNCTION);
        assert_eq!(fit.token_modifiers_bitset, MOD_READONLY);
        assert_eq!(custom.token_modifiers_bitset, MOD_READONLY);
        assert_eq!(fit.length, 3);
        assert_eq!(custom.length, "custom_elaborator".len() as u32);
    }

    #[test]
    fn semantic_tokens_contextual_pure_calls_are_functions() {
        for src in [
            "module m; fn caller(x: A) -> A { pure(x) }",
            "module m; fn caller(x: A) -> A { helpers.pure(x) }",
        ] {
            let tokens = semantic_tokens(src).expect("semantic tokens");
            let pure = semantic_token_at(src, &tokens.data, "pure").expect("pure call token");
            assert_eq!(pure.token_type, TT_FUNCTION, "source: {src}");
        }
    }

    #[test]
    fn semantic_tokens_callable_declaration_targets_are_readonly_references() {
        let src = "module m;\n\
            fn declared() -> . { () }\n\
            op _ + _ { impl op_target; };\n\
            varop [* *] {\n\
              foldr step_target base_target; finalize finish_target;\n\
            };\n\
            elab demo : . -> . { impl elab_target; };\n";
        let tokens = semantic_tokens(src).expect("semantic tokens");

        let declaration =
            semantic_token_at(src, &tokens.data, "declared").expect("function declaration");
        assert_eq!(declaration.token_type, TT_FUNCTION);
        assert_eq!(
            declaration.token_modifiers_bitset,
            MOD_DECLARATION | MOD_READONLY
        );

        for target in [
            "op_target",
            "base_target",
            "step_target",
            "finish_target",
            "elab_target",
        ] {
            let token = semantic_token_at(src, &tokens.data, target)
                .unwrap_or_else(|| panic!("missing callable target `{target}`"));
            assert_eq!(token.token_type, TT_FUNCTION, "target `{target}`");
            assert_eq!(
                token.token_modifiers_bitset, MOD_READONLY,
                "target `{target}` must be readonly but not a declaration"
            );
        }
    }

    #[test]
    fn semantic_tokens_punctuation_is_not_emitted() {
        // Parens, brackets, commas and semicolons are not semantic
        // tokens — they are handled by the grammar highlighter.
        let result = semantic_tokens("( ) { } , ;").unwrap();
        assert!(
            result.data.is_empty(),
            "punctuation must not produce semantic tokens; got {:?}",
            result.data
        );
    }

    #[test]
    fn semantic_tokens_fn_def_name_is_function_with_declaration() {
        // `fn foo() -> . { () }` — `foo` is `EntityNameFunction`.
        let src = "module m;\nfn foo() -> . { () }\n";
        let result = semantic_tokens(src).unwrap();
        // Find the token at the byte offset of `foo` (byte 13 in src).
        // Since tokens are delta-encoded, decode to absolute positions
        // by summing.
        let tokens = &result.data;
        let mut abs_line = 0u32;
        let mut abs_col = 0u32;
        let mut found_fn = false;
        let mut found_foo_function = false;
        for t in tokens {
            abs_line += t.delta_line;
            abs_col = if t.delta_line == 0 {
                abs_col + t.delta_start
            } else {
                t.delta_start
            };
            // `fn` keyword on line 1, col 0.
            if abs_line == 1 && abs_col == 0 && t.token_type == TT_KEYWORD {
                found_fn = true;
            }
            // `foo` function name on line 1, col 3.
            if abs_line == 1 && abs_col == 3 && t.token_type == TT_FUNCTION {
                assert_ne!(
                    t.token_modifiers_bitset & MOD_DECLARATION,
                    0,
                    "fn name must carry the declaration modifier"
                );
                found_foo_function = true;
            }
        }
        assert!(found_fn, "expected `fn` keyword token");
        assert!(found_foo_function, "expected `foo` as function token");
    }

    #[test]
    fn semantic_tokens_result_id_is_none() {
        // v1 does not issue delta-update result ids.
        let result = semantic_tokens("fn").unwrap();
        assert_eq!(result.result_id, None);
    }

    #[test]
    fn server_capabilities_full_true_range_false() {
        let caps = server_capabilities();
        let SemanticTokensServerCapabilities::SemanticTokensOptions(opts) = caps else {
            panic!("expected SemanticTokensOptions variant");
        };
        assert_eq!(
            opts.full,
            Some(SemanticTokensFullOptions::Bool(true)),
            "full must be true"
        );
        assert_eq!(opts.range, Some(false), "range must be false");
        // Legend must be non-empty.
        assert!(
            !opts.legend.token_types.is_empty(),
            "token_types must be non-empty"
        );
        assert!(
            !opts.legend.token_modifiers.is_empty(),
            "token_modifiers must be non-empty"
        );
    }

    #[test]
    fn server_capabilities_legend_matches_constants() {
        let caps = server_capabilities();
        let SemanticTokensServerCapabilities::SemanticTokensOptions(opts) = caps else {
            panic!("expected SemanticTokensOptions");
        };
        assert_eq!(&opts.legend.token_types[..], TOKEN_TYPES);
        assert_eq!(&opts.legend.token_modifiers[..], TOKEN_MODIFIERS);
    }

    fn semantic_token_at<'a>(
        source: &str,
        tokens: &'a [SemanticToken],
        needle: &str,
    ) -> Option<&'a SemanticToken> {
        let byte = source.find(needle)? as u32;
        semantic_token_at_byte(source, tokens, byte)
    }

    fn semantic_token_at_byte<'a>(
        source: &str,
        tokens: &'a [SemanticToken],
        byte: u32,
    ) -> Option<&'a SemanticToken> {
        let pos = LineIndex::new(source).to_position(byte);
        let mut abs_line = 0u32;
        let mut abs_col = 0u32;
        for token in tokens {
            abs_line += token.delta_line;
            abs_col = if token.delta_line == 0 {
                abs_col + token.delta_start
            } else {
                token.delta_start
            };
            if abs_line == pos.line && abs_col == pos.character {
                return Some(token);
            }
        }
        None
    }
}
