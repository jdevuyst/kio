//! Lexer for the Kio' regular-module grammar.
//!
//! Whitespace and `// …` line comments are trivia and dropped during
//! lexing. Every keyword-shaped word — `module`, `import`, `as`,
//! `pub`, `pure`, `type`, `host`, `bridge`, `package`, `fn`, `newtype`, `let`,
//! `constructor`, `projector`, and the magic `__...__` import targets —
//! is emitted as `Ident(text)` and disambiguated by the parser based on
//! grammatical position.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Pure-structural punctuators keep dedicated kinds.
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Semicolon,
    /// Every operator character lexes into a **greedy** `SymbolRun`
    /// carrying the run's spelling: adjacent op-chars fuse (`->`,
    /// `.>`, and — like the main kio-rs lexer — `=.`), while
    /// whitespace, identifiers, digits, and the structural punctuators
    /// break the run. Kio' pins each fixed-role symbol
    /// (`< > : = | & ! . /`, the arrow `->`, the kind-marker run
    /// `*`/`**`/`***`) to a grammatical role; `[` and `]` otherwise join
    /// ordinary maximal runs like every other op-char. A parser site matches on
    /// the run's content and peels a structural delimiter (`->`, `>`)
    /// off the front of a longer fused run where the grammar needs it.
    /// This mirrors the main lexer so the two implementations agree on
    /// token vocabulary (see `specs/grammar.md` § Lexical structure
    /// (Kio')).
    SymbolRun(String),
    Ident(String),
    IntLit,
    FloatLit,
    StrLit,
    /// A `///` doc-comment line. Regular `//` comments are dropped as
    /// trivia, but doc-comment lines are surfaced as a token so the
    /// parser can attach them to the following declaration and reject
    /// a doc-comment immediately before an `import` clause (per
    /// `specs/grammar.md` § Doc-comment lines). The payload is not
    /// needed for grammar validation, so none is carried.
    DocLine,
}

impl TokenKind {
    /// Construct a [`TokenKind::SymbolRun`] from its spelling.
    pub fn sym(s: &str) -> TokenKind {
        TokenKind::SymbolRun(s.to_owned())
    }

    /// True iff this token is the [`TokenKind::SymbolRun`] spelled
    /// exactly `s`.
    pub fn is_sym(&self, s: &str) -> bool {
        matches!(self, TokenKind::SymbolRun(run) if run == s)
    }
}

#[derive(Debug)]
pub struct LexError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

pub fn lex(src: &str) -> Result<Vec<Token>, LexError> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];

        // Whitespace.
        if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
            continue;
        }

        // Comment marker — `//` (regular line comment, dropped) or
        // `///` (doc-comment line, surfaced as a `DocLine` token). The
        // marker must be followed immediately by whitespace,
        // end-of-line, or end-of-file; a non-whitespace character
        // flush against the marker — including a fourth `/` (`////`) —
        // is a lex error. This mirrors the main lexer
        // (`kio-rs/src/pass/lexer.rs`) so the two implementations agree
        // on what is a comment (see `specs/grammar.md` § Comment
        // markers require trailing whitespace).
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let is_doc = i + 2 < bytes.len() && bytes[i + 2] == b'/';
            let marker_len = if is_doc { 3 } else { 2 };
            let after = bytes.get(i + marker_len).copied();
            let rejects = matches!(after, Some(c) if !matches!(c, b' ' | b'\t' | b'\r' | b'\n'));
            if rejects {
                let marker = if is_doc { "///" } else { "//" };
                return Err(LexError {
                    offset: i,
                    message: format!(
                        "`{marker}` comment marker must be followed by whitespace or \
                         end-of-line (a non-whitespace character flush against `{marker}`, \
                         including a fourth `/`, is rejected)"
                    ),
                });
            }
            let marker_start = i;
            i += marker_len;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if is_doc {
                tokens.push(Token {
                    kind: TokenKind::DocLine,
                    span: Span {
                        start: marker_start,
                        end: i,
                    },
                });
            }
            continue;
        }

        let start = i;

        // Leading-minus carve-out (specs/grammar.md § "Notes (Kio
        // surface)"): a `-` flush against a digit fuses into the
        // following numeric literal when the prior token does not
        // end an expression. After an expression-ending token
        // (identifier, literal, `)`, or `}`), the `-` stays as a
        // separate op-token. A SymbolRun, including a bracket-bearing
        // run, instead leaves the lexer in an expression-starting
        // position. The Kio'-grammar verifier mirrors the
        // main lexer's contextual decision so the two implementations
        // agree on token vocabulary. The check happens before the
        // `->` arrow rule so `-40 -> ...` (negative literal then
        // arrow) parses correctly when context-starting.
        if b == b'-'
            && !prior_ends_expression(tokens.last())
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
        {
            i += 1; // consume the leading `-`
            // Read the rest of the literal; the produced token's
            // span starts at the original `-` position, so the
            // span covers the full `-NNN` range. The kio-prime-check
            // lexer doesn't store the digit string (its IntLit /
            // FloatLit kinds carry no payload), so there's no need
            // to remember the sign separately — the parser sees one
            // `IntLit` / `FloatLit` token and validates against the
            // Literal production.
            let lit_token = lex_numeric_literal(bytes, &mut i, start);
            tokens.push(lit_token);
            continue;
        }

        // Structural punctuators keep dedicated token kinds. Every operator
        // character lexes as a **greedy** `SymbolRun`: the arrow `->`,
        // the dot-splice family (`.>` / `.>>` / `.<` / `.<<`), the
        // kind-marker run (`*` / `**` / `***`), and fused runs such as
        // `=.` all emerge from ordinary greedy fusion rather than
        // fixed carve-outs, matching the main kio-rs lexer so the two
        // agree on token vocabulary (see specs/grammar.md § Lexical
        // structure (Kio')). Parser sites peel a structural delimiter
        // (`->`, `>`) back off the front of a longer run where the
        // grammar needs it. The leading-minus carve-out above already
        // consumed a `-` that should become part of a numeric literal.
        let structural = match b {
            b'(' => Some(TokenKind::LParen),
            b')' => Some(TokenKind::RParen),
            b'{' => Some(TokenKind::LBrace),
            b'}' => Some(TokenKind::RBrace),
            b',' => Some(TokenKind::Comma),
            b';' => Some(TokenKind::Semicolon),
            _ => None,
        };
        if let Some(kind) = structural {
            tokens.push(Token {
                kind,
                span: Span { start, end: i + 1 },
            });
            i += 1;
            continue;
        }
        if is_op_char(b) {
            tokens.push(lex_op_run(bytes, &mut i, start));
            continue;
        }

        // String literal.
        if b == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i >= bytes.len() {
                return Err(LexError {
                    offset: start,
                    message: "unterminated string literal".into(),
                });
            }
            i += 1; // closing quote
            tokens.push(Token {
                kind: TokenKind::StrLit,
                span: Span { start, end: i },
            });
            continue;
        }

        // Number literal: digits, optional `.digits`, optional
        // exponent. No width suffix — a Kio' literal's type comes
        // from its mandatory trailing `(Type)` call form
        // (specs/grammar.md § Kio' grammar — `LiteralCall ::=
        // LITERAL '(' Type ')'`). `100i32` lexes as the integer
        // `100` followed by the identifier `i32`, which the parser
        // then rejects.
        if b.is_ascii_digit() {
            tokens.push(lex_numeric_literal(bytes, &mut i, start));
            continue;
        }

        // Identifier (and the keyword-shaped words).
        if is_ident_start(b) {
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let text = &src[start..i];
            tokens.push(Token {
                kind: TokenKind::Ident(text.to_string()),
                span: Span { start, end: i },
            });
            continue;
        }

        return Err(LexError {
            offset: start,
            message: format!("unexpected character `{}`", b as char),
        });
    }

    Ok(tokens)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Characters admitted into a greedy operator [`TokenKind::SymbolRun`].
/// The full operator-character set (`+ - * / % ^ ~ ? @ # $ \ ' ` < >
/// = ! & | : . [ ]`).
/// Mirrors the main kio-rs lexer's `is_op_char_in_run` so the two
/// implementations agree on token vocabulary; `.` joins runs like any
/// other op-char, so `.>` and `=.` fuse by ordinary greedy lexing.
fn is_op_char(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'^'
            | b'~'
            | b'?'
            | b'@'
            | b'#'
            | b'$'
            | b'\\'
            | b'\''
            | b'`'
            | b'<'
            | b'>'
            | b'='
            | b'!'
            | b'&'
            | b'|'
            | b':'
            | b'.'
            | b'['
            | b']'
    )
}

/// Greedy operator-character run starting at `bytes[*i]` (which must
/// be an op-char). Consumes op-chars until a non-op char or a
/// block-listed sequence, producing one [`TokenKind::SymbolRun`]. The
/// block-list reserves `//` (a line comment) and any second `/` in a
/// run, so a run carries at most one `/`. Mirrors the main kio-rs
/// lexer's `lex_op_run`; the arrow `->`, the dot-splice family, and
/// fused runs like `=.` are all produced here rather than by fixed
/// carve-outs.
fn lex_op_run(bytes: &[u8], i: &mut usize, start: usize) -> Token {
    let mut slash_count = 0;
    while *i < bytes.len() && is_op_char(bytes[*i]) {
        if bytes[*i] == b'/' {
            if slash_count >= 1 || bytes.get(*i + 1) == Some(&b'/') {
                break;
            }
            slash_count += 1;
        }
        *i += 1;
    }
    let text = std::str::from_utf8(&bytes[start..*i]).expect("operator bytes are ASCII");
    Token {
        kind: TokenKind::SymbolRun(text.to_owned()),
        span: Span { start, end: *i },
    }
}

/// Read an integer or float literal starting at `bytes[*i]` (which
/// must be an ASCII digit), advancing `i` past the literal. The
/// `start` byte offset is used for the produced span; pass the
/// position of the leading `-` (if the leading-minus carve-out
/// consumed one) or of the leading digit otherwise.
fn lex_numeric_literal(bytes: &[u8], i: &mut usize, start: usize) -> Token {
    while *i < bytes.len() && (bytes[*i].is_ascii_digit() || bytes[*i] == b'_') {
        *i += 1;
    }
    let mut is_float = false;
    // Fractional part: `.` followed by a digit (not `.@` or method
    // access on a literal, which Kio' forbids anyway).
    if *i + 1 < bytes.len() && bytes[*i] == b'.' && bytes[*i + 1].is_ascii_digit() {
        is_float = true;
        *i += 1;
        while *i < bytes.len() && (bytes[*i].is_ascii_digit() || bytes[*i] == b'_') {
            *i += 1;
        }
    }
    // Exponent.
    if *i < bytes.len() && (bytes[*i] == b'e' || bytes[*i] == b'E') {
        is_float = true;
        *i += 1;
        if *i < bytes.len() && (bytes[*i] == b'+' || bytes[*i] == b'-') {
            *i += 1;
        }
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
    }
    let kind = if is_float {
        TokenKind::FloatLit
    } else {
        TokenKind::IntLit
    };
    Token {
        kind,
        span: Span { start, end: *i },
    }
}

/// True iff the previous token (or absence thereof, at BOF) ends an
/// expression. Drives the leading-minus carve-out (see [`lex`]).
/// **Expression-ending** kinds: identifiers, every literal kind,
/// closing structural brackets `)`, `}`. Everything else (BOF, opening
/// brackets, structural separators, any other `SymbolRun`) leaves
/// the lexer in an expression-starting position. Mirrors
/// `kio-rs/src/lexer.rs` `token_ends_expression`.
fn prior_ends_expression(last: Option<&Token>) -> bool {
    let Some(last) = last else {
        return false; // BOF — expression-starting
    };
    match &last.kind {
        TokenKind::Ident(_) => true,
        TokenKind::IntLit | TokenKind::FloatLit | TokenKind::StrLit => true,
        TokenKind::RParen | TokenKind::RBrace => true,
        TokenKind::LParen
        | TokenKind::LBrace
        | TokenKind::Comma
        | TokenKind::Semicolon
        | TokenKind::SymbolRun(_)
        // A doc-comment line is trivia-like — it never ends an
        // expression, so it leaves the lexer expression-starting.
        | TokenKind::DocLine => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lex_punctuation() {
        let ks = kinds("( ) { } < > [ ] , ; : . = | & ! -> .> .>> .< .<<");
        assert_eq!(
            ks,
            vec![
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBrace,
                TokenKind::RBrace,
                TokenKind::sym("<"),
                TokenKind::sym(">"),
                TokenKind::sym("["),
                TokenKind::sym("]"),
                TokenKind::Comma,
                TokenKind::Semicolon,
                TokenKind::sym(":"),
                TokenKind::sym("."),
                TokenKind::sym("="),
                TokenKind::sym("|"),
                TokenKind::sym("&"),
                TokenKind::sym("!"),
                TokenKind::sym("->"),
                TokenKind::sym(".>"),
                TokenKind::sym(".>>"),
                TokenKind::sym(".<"),
                TokenKind::sym(".<<"),
            ]
        );
    }

    #[test]
    fn lex_bare_int() {
        assert_eq!(kinds("42"), vec![TokenKind::IntLit]);
        assert_eq!(kinds("2_147_483_647"), vec![TokenKind::IntLit]);
    }

    #[test]
    fn lex_bare_float() {
        assert_eq!(kinds("3.14"), vec![TokenKind::FloatLit]);
        assert_eq!(kinds("1.5e10"), vec![TokenKind::FloatLit]);
        assert_eq!(kinds("1.0e-9"), vec![TokenKind::FloatLit]);
    }

    // ---- Leading-minus carve-out ------------------------------------------
    //
    // Mirrors the main `kio-rs` lexer's contextual decision so the
    // Kio'-grammar verifier agrees on token vocabulary: a `-` flush
    // against a digit fuses into the following numeric literal when
    // the prior token doesn't end an expression. After an
    // expression-ending token (identifier, literal, `)`, or `}`),
    // the `-` stays as its own op-token.

    #[test]
    fn leading_minus_at_bof_lexes_as_negative_literal() {
        // BOF is expression-starting; `-40` lexes as one IntLit.
        let toks = lex("-40").unwrap();
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::IntLit);
        assert_eq!(toks[0].span.start, 0);
        assert_eq!(toks[0].span.end, 3);
    }

    #[test]
    fn leading_minus_after_open_paren_lexes_as_negative_literal() {
        // After `(`, `-40` fuses into one IntLit. Models `f(-40)`.
        let ks = kinds("f(-40)");
        assert_eq!(ks.len(), 4);
        assert!(matches!(ks[0], TokenKind::Ident(ref s) if s == "f"));
        assert_eq!(ks[1], TokenKind::LParen);
        assert_eq!(ks[2], TokenKind::IntLit);
        assert_eq!(ks[3], TokenKind::RParen);
    }

    #[test]
    fn leading_minus_after_equals_lexes_as_negative_literal() {
        // After `=` (a SymbolRun), `-40` fuses into one IntLit.
        let ks = kinds("let x = -40;");
        assert_eq!(
            ks,
            vec![
                TokenKind::Ident("let".into()),
                TokenKind::Ident("x".into()),
                TokenKind::sym("="),
                TokenKind::IntLit,
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn binary_minus_after_int_literal_lexes_as_subtraction() {
        // After an IntLit, the `-` is binary subtraction.
        let ks = kinds("1-2");
        assert_eq!(
            ks,
            vec![TokenKind::IntLit, TokenKind::sym("-"), TokenKind::IntLit]
        );
    }

    #[test]
    fn binary_minus_after_ident_lexes_as_subtraction() {
        // After an Ident, the `-` is binary subtraction.
        let ks = kinds("a-1");
        assert_eq!(
            ks,
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::sym("-"),
                TokenKind::IntLit,
            ]
        );
    }

    #[test]
    fn binary_minus_after_close_paren_lexes_as_subtraction() {
        // After `)`, the `-` is binary subtraction.
        let ks = kinds("(x)-1");
        assert_eq!(
            ks,
            vec![
                TokenKind::LParen,
                TokenKind::Ident("x".into()),
                TokenKind::RParen,
                TokenKind::sym("-"),
                TokenKind::IntLit,
            ]
        );
    }

    #[test]
    fn whitespace_between_minus_and_digit_keeps_two_tokens() {
        // `- 40` at start-of-file is two tokens — the carve-out
        // requires the `-` to sit flush against the digit.
        let ks = kinds("- 40");
        assert_eq!(ks, vec![TokenKind::sym("-"), TokenKind::IntLit]);
    }

    #[test]
    fn leading_minus_followed_by_ident_is_two_tokens() {
        // `-y` at BOF: the carve-out requires a digit after the `-`,
        // so this is `SymbolRun("-")` then `Ident`.
        let ks = kinds("-y");
        assert_eq!(ks, vec![TokenKind::sym("-"), TokenKind::Ident("y".into())]);
    }

    #[test]
    fn leading_minus_on_float_literal_lexes_as_negative_float() {
        // `-3.14` at BOF lexes as one FloatLit.
        let ks = kinds("-3.14");
        assert_eq!(ks, vec![TokenKind::FloatLit]);
    }

    #[test]
    fn dash_arrow_still_lexes_as_arrow_at_bof() {
        // The carve-out must not absorb the `->` arrow at BOF —
        // `-` followed by `>` is not followed by a digit.
        let ks = kinds("->");
        assert_eq!(ks, vec![TokenKind::sym("->")]);
    }

    #[test]
    fn lex_suffix_shaped_ident_after_int_is_separate_token() {
        // `100i32` is no longer a single token: the digits lex as
        // an integer, the trailing `i32` as a separate identifier.
        let ks = kinds("100i32");
        assert_eq!(ks.len(), 2);
        assert_eq!(ks[0], TokenKind::IntLit);
        assert!(matches!(ks[1], TokenKind::Ident(ref s) if s == "i32"));
    }

    #[test]
    fn lex_string() {
        assert_eq!(kinds("\"hi\""), vec![TokenKind::StrLit]);
        assert_eq!(kinds("\"a\\nb\""), vec![TokenKind::StrLit]);
    }

    #[test]
    fn lex_skips_line_comments() {
        let ks = kinds("// comment\n42 // tail\n");
        assert_eq!(ks, vec![TokenKind::IntLit]);
    }

    #[test]
    fn lex_idents() {
        let ks = kinds("module foo bar_baz");
        assert!(matches!(ks[0], TokenKind::Ident(ref s) if s == "module"));
        assert!(matches!(ks[1], TokenKind::Ident(ref s) if s == "foo"));
        assert!(matches!(ks[2], TokenKind::Ident(ref s) if s == "bar_baz"));
    }

    // ---- Greedy operator-run fusion -------------------------------------
    //
    // Adjacent operator characters fuse into one `SymbolRun`, matching
    // the main kio-rs lexer: without the same fusion a flush spelling
    // like `type A=.;` lexes as `=` `.` here and slips past the
    // verifier while kio-rs (which fuses `=.`) rejects it.

    #[test]
    fn op_chars_fuse_greedily() {
        assert_eq!(kinds("=."), vec![TokenKind::sym("=.")]);
        assert_eq!(kinds("**"), vec![TokenKind::sym("**")]);
        assert_eq!(kinds("***"), vec![TokenKind::sym("***")]);
        assert_eq!(kinds("->"), vec![TokenKind::sym("->")]);
        assert_eq!(kinds(">:"), vec![TokenKind::sym(">:")]);
        assert_eq!(kinds("><"), vec![TokenKind::sym("><")]);
    }

    #[test]
    fn whitespace_breaks_op_run() {
        // The spaced spelling stays two one-character symbols, so the
        // parser reads `=` then `.` — `type A = .;` still parses.
        assert_eq!(kinds("= ."), vec![TokenKind::sym("="), TokenKind::sym(".")]);
    }

    #[test]
    fn brackets_join_maximal_operator_runs() {
        assert_eq!(
            kinds("[! ]] ]- ][*"),
            vec![
                TokenKind::sym("[!"),
                TokenKind::sym("]]"),
                TokenKind::sym("]-"),
                TokenKind::sym("][*"),
            ]
        );
        assert_eq!(
            kinds("[ ! ] ] ] - ] [ *"),
            vec![
                TokenKind::sym("["),
                TokenKind::sym("!"),
                TokenKind::sym("]"),
                TokenKind::sym("]"),
                TokenKind::sym("]"),
                TokenKind::sym("-"),
                TokenKind::sym("]"),
                TokenKind::sym("["),
                TokenKind::sym("*"),
            ]
        );
    }

    #[test]
    fn bracket_operator_run_does_not_end_expression_for_minus() {
        assert_eq!(kinds("]-1"), vec![TokenKind::sym("]-"), TokenKind::IntLit]);
        assert_eq!(kinds("] -1"), vec![TokenKind::sym("]"), TokenKind::IntLit]);
    }

    #[test]
    fn op_run_carries_at_most_one_slash() {
        // A module path separator: an identifier breaks the run, so
        // each `/` is its own one-character `SymbolRun`.
        assert_eq!(
            kinds("a/b"),
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::sym("/"),
                TokenKind::Ident("b".into()),
            ]
        );
    }
}
