//! Lexer for Kio'.
//!
//! Produces a flat token stream from source text. Every word in the
//! language — including the item-leading words `fn` / `type` /
//! `literal` / `newtype` / `labels` / `module` / `import` and the
//! expression-leading words `let` / `fn` / `if` / `else` / `do` —
//! is lexed as an ordinary `Ident` and disambiguated in the parser
//! by position. The structural punctuators `( ) { } , ;` keep
//! dedicated token kinds; `.`, `[`, and `]` are ordinary op-chars that join
//! greedy `SymbolRun`s alongside `+ - * < > = ! & | :` etc., and
//! string and number literals also get their own token kinds.
//!
//! Spec for which words are contextual at which positions: see
//! `specs/grammar.md` § "Contextual keywords".
//!
//! **Comment and newline trivia.** Each [`Token`] carries a
//! `leading_trivia` field holding the line comments and newlines that
//! appear between the previous meaningful token and this one (in
//! source order). Horizontal whitespace is discarded — the formatter
//! regenerates indent and inline spacing from style rules — and any
//! trailing whitespace inside a comment is dropped on the way in.
//! "Trailing comment of the previous token" is recovered on the
//! emission side by checking whether the next token's leading_trivia
//! starts with a `LineComment` not preceded by a `Newline`.
//!
//! The parser consumes tokens by `kind` and preserves trivia only at the
//! bounded AST positions used by the formatter. The pretty-printer reads
//! top-level trivia (above `import` / item), while compact declarations hoist
//! otherwise-unowned internal comments; expression/container positions retain
//! their own explicitly captured trivia.

use crate::error::Error;
use crate::span::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// Trivia (newlines, line comments) between the previous
    /// meaningful token and this one, in source order. Horizontal
    /// whitespace is not represented; the formatter regenerates it.
    pub leading_trivia: Vec<Trivia>,
}

/// One trivia element preceding a meaningful token. Trivia is
/// "leading-only" — every gap between meaningful tokens is recorded
/// as the leading trivia of the *following* token; there is no
/// `trailing_trivia`. To recover a trailing comment of the previous
/// token, inspect the next token's `leading_trivia` and check whether
/// it starts with a `LineComment` not preceded by a `Newline`.
///
/// `Trivia` derives `serde` so it can ride through `Module<Surface>`
/// when the phase-polymorphic AST is serialized. The enriched-IR
/// cache itself never sees a `Surface` module — the cached value is
/// `Module<Enriched>`, whose `LeadingTrivia` collapses to `()` — but
/// the derive needs to satisfy a generic bound, so the per-phase
/// instantiation is uniform.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Trivia {
    /// `// …` line comment. `text` is the comment's content with the
    /// leading `//` and any trailing horizontal whitespace stripped
    /// (no prefix, no trailing newline). `span` covers the source
    /// bytes from the leading `//` up to (but not including) the
    /// terminating newline — the syntax-highlighting tokens dump
    /// uses it to emit `comment.line` regions; the pretty-printer
    /// only reads `text`.
    LineComment { text: String, span: Span },
    /// `/// …` doc-comment line. Recognized when `///` is followed by
    /// whitespace, end-of-line, or end-of-file — a fourth `/`
    /// (`////`) is a lex error, not a longer comment. `text` is the
    /// line's payload with the leading `///` stripped and one optional
    /// leading space removed (so `/// foo` stores `"foo"` and `///`
    /// stores `""`). `span` covers the source bytes from the leading
    /// `///` up to (but not including) the terminating newline.
    DocCommentLine { text: String, span: Span },
    /// One source-level `\n` (CR, CRLF, and bare LF normalize to a
    /// single `Newline`). Multiple consecutive newlines produce
    /// multiple `Newline` entries; the formatter applies the blank-
    /// line collapse rule from `specs/language.md` on emission.
    Newline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// Identifiers and contextual keywords. Includes the item-leading
    /// words (`fn`, `type`, `literal`, `newtype`, `labels`, `module`,
    /// `import`), the expression-leading words (`let`, `if`,
    /// `else`, `do`), boolean-shaped words (`true`, `false`), the contextual
    /// set (`pub`, `from`, `as`,
    /// `target`, `out`, `constructor`, `projector`, `role`), and
    /// reserved `__name__` spellings (`__pair__`,
    /// `__intrinsics__`, …).
    /// Disambiguation by position happens in the parser / resolver.
    Ident(String),

    // Literals.
    StrLit(String), // post-escape value
    IntLit {
        digits: String,
    }, // digit separators stripped
    FloatLit {
        digits: String,
    }, // digit separators stripped; includes the `.` and exponent
    BoolLit(bool),

    // Pure-structural punctuators. These never fuse with adjacent
    // characters and keep dedicated token kinds.
    LParen,    // (
    RParen,    // )
    LBrace,    // {
    RBrace,    // }
    Comma,     // ,
    Semicolon, // ;

    /// A run of symbol characters — the unified token for every
    /// non-structural symbol. Covers user-defined operator runs
    /// (`+`, `?`, `++`, `][`, …), the type-chain separators `&` / `|`,
    /// and fixed-role symbols such as `< > [ ] : = ! -> .> .>> .< .<<`
    /// that the grammar pins to specific positions. Greedy lexing over the
    /// operator-character set `+ - * / % ^ ~ ? @ # $ \ ' ` < > = !
    /// & | : . [ ]`; brackets, arrows, and dot-splice spellings fuse with adjacent
    /// op-chars under the same greedy rule. Parser sites that want a
    /// specific symbol match on the run's content — see
    /// [`TokenKind::sym`] / [`TokenKind::is_sym`].
    SymbolRun(String),

    /// `_` — plain slot token, recognized at slot positions
    /// (op pattern, wildcard `let _`, `match _`, type-arg `_`,
    /// variadic-op element pattern). The lexer emits a
    /// dedicated token kind for pure-underscore runs so every
    /// parser context branches on the token kind directly rather
    /// than matching `Ident("_")`.
    Slot1,
    /// `__` — same-op recursive slot token. Same admissibility
    /// story as [`Slot1`].
    Slot2,
    /// `___` — greedy slot token. Same admissibility story as
    /// [`Slot1`].
    Slot3,
}

impl TokenKind {
    /// Construct a [`TokenKind::SymbolRun`] from its spelling.
    /// Shorthand for the common parser-site need to name one
    /// specific symbol (`TokenKind::sym("<")`, `TokenKind::sym("->")`).
    pub fn sym(s: &str) -> TokenKind {
        TokenKind::SymbolRun(s.to_owned())
    }

    /// True iff this token is the [`TokenKind::SymbolRun`] spelled
    /// exactly `s`. Parser sites that want one fixed-role symbol
    /// (`<`, `:`, `->`, …) match on the run's content through this
    /// predicate.
    pub fn is_sym(&self, s: &str) -> bool {
        matches!(self, TokenKind::SymbolRun(run) if run == s)
    }
}

pub fn lex(source: &str) -> Result<Vec<Token>, Error> {
    Ok(lex_with_trailing(source)?.0)
}

/// Like [`lex`], but also returns the trivia run after the last
/// meaningful token (the comments / newlines between it and
/// end-of-file). The leading-only trivia model has no token to
/// attach this run to, so [`lex`] discards it; the formatter needs
/// it to keep a trailing-after-item or end-of-file comment from
/// being dropped on a `kio fmt` round-trip.
pub fn lex_with_trailing(source: &str) -> Result<(Vec<Token>, Vec<Trivia>), Error> {
    let LexPrefix {
        tokens,
        trailing,
        error,
        ..
    } = lex_prefix(source);
    match error {
        Some(error) => Err(error),
        None => Ok((tokens, trailing)),
    }
}

pub(crate) struct LexPrefix {
    pub(crate) tokens: Vec<Token>,
    pub(crate) trailing: Vec<Trivia>,
    pub(crate) error: Option<Error>,
    #[cfg(any(test, feature = "cli"))]
    pub(crate) frontier: u32,
}

pub(crate) fn lex_prefix(source: &str) -> LexPrefix {
    let mut lexer = Lexer::new(source);
    let mut tokens = Vec::new();
    loop {
        match lexer.next_token() {
            Ok(Some(token)) => tokens.push(token),
            Ok(None) => {
                return LexPrefix {
                    tokens,
                    trailing: lexer.take_trailing_trivia(),
                    error: None,
                    #[cfg(any(test, feature = "cli"))]
                    frontier: source.len() as u32,
                };
            }
            Err(error) => {
                return LexPrefix {
                    tokens,
                    trailing: lexer.take_trailing_trivia(),
                    error: Some(error),
                    #[cfg(any(test, feature = "cli"))]
                    frontier: lexer.pending_start as u32,
                };
            }
        }
    }
}

/// Render the rejected marker-plus-next-char for the comment-marker
/// diagnostic — e.g. `marker_with_next("//", Some(b'f'))` yields
/// `"//f"`. A non-ASCII or absent next byte is dropped (the marker
/// alone is already the informative part).
fn marker_with_next(marker: &str, next: Option<u8>) -> String {
    match next {
        Some(b) if b.is_ascii_graphic() => format!("{marker}{}", b as char),
        _ => marker.to_owned(),
    }
}

struct Lexer<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// An error can point inside its token; prefix consumers need the start
    /// of the token or trivia that failed instead of that diagnostic span.
    #[cfg(any(test, feature = "cli"))]
    pending_start: usize,
    /// Whether the previously emitted meaningful token closes an
    /// expression. Used by the leading-minus carve-out in
    /// [`Lexer::next_token`]: after an expression-ending token, a `-`
    /// flush against a digit is still a binary operator; after an
    /// expression-starting position (BOF or any token that doesn't
    /// end an expression), the `-` is consumed as part of the
    /// numeric literal. Initialised to `false` — start of file is
    /// an expression-starting position. Classification of each
    /// produced token lives in [`token_ends_expression`].
    prior_ends_expr: bool,
    /// Retained until a complete token owns it, including on lexical failure
    /// or EOF where no token can carry these comments.
    pending_trivia: Vec<Trivia>,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            bytes: source.as_bytes(),
            pos: 0,
            #[cfg(any(test, feature = "cli"))]
            pending_start: 0,
            prior_ends_expr: false,
            pending_trivia: Vec::new(),
        }
    }

    fn take_trailing_trivia(&mut self) -> Vec<Trivia> {
        std::mem::take(&mut self.pending_trivia)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn bump(&mut self) {
        self.pos += 1;
    }

    fn bump_source_char(&mut self) -> char {
        let start = self.pos;
        self.pos += utf8_char_len(self.bytes[start]);
        std::str::from_utf8(&self.bytes[start..self.pos])
            .expect("lexer positions remain on source UTF-8 character boundaries")
            .chars()
            .next()
            .expect("a source character contains at least one byte")
    }

    fn span(&self, start: usize) -> Span {
        Span::new(start as u32, self.pos as u32)
    }

    fn parse_err(&self, start: usize, message: impl Into<String>) -> Error {
        Error::parse(self.span(start), message)
    }

    /// Walk forward over horizontal whitespace, newlines, and line
    /// comments, collecting the trivia between the previous meaningful
    /// token (or the start of file) and the next one. Horizontal
    /// whitespace is discarded; newlines and line comments are
    /// preserved in source order, with any trailing horizontal
    /// whitespace inside a comment stripped before storage.
    ///
    /// Returns a parse error when a comment marker (`//` or `///`) is
    /// not immediately followed by whitespace, end-of-line, or
    /// end-of-file — see [`Lexer::comment_marker_rejects`] for the
    /// rule. `////` (four or more slashes) is one instance of that
    /// rejection: after `///` the next character is a fourth `/`,
    /// which is not whitespace.
    fn collect_leading_trivia(&mut self) -> Result<(), Error> {
        loop {
            #[cfg(any(test, feature = "cli"))]
            {
                self.pending_start = self.pos;
            }
            match self.peek() {
                Some(b' ') | Some(b'\t') => {
                    // Horizontal whitespace: discard.
                    self.bump();
                }
                Some(b'\r') => {
                    // CR or CRLF: normalize to a single Newline.
                    self.bump();
                    if self.peek() == Some(b'\n') {
                        self.bump();
                    }
                    self.pending_trivia.push(Trivia::Newline);
                }
                Some(b'\n') => {
                    self.bump();
                    self.pending_trivia.push(Trivia::Newline);
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    // A `//` regular line comment or a `///` doc-comment
                    // line. The marker (`//` or `///`) must be followed
                    // immediately by whitespace, end-of-line, or
                    // end-of-file; a non-whitespace character flush
                    // against the marker — including a fourth `/`
                    // (`////`) — is a parse error. Dispatch on the third
                    // character to tell `//` from `///`.
                    let comment_start = self.pos;
                    let is_doc = self.peek_at(2) == Some(b'/');
                    // The character immediately after the marker: index
                    // 2 for `//`, index 3 for `///`.
                    let after_marker = self.peek_at(if is_doc { 3 } else { 2 });
                    if Self::comment_marker_rejects(after_marker) {
                        let marker = if is_doc { "///" } else { "//" };
                        let span = Span::new(
                            comment_start as u32,
                            (comment_start + if is_doc { 3 } else { 2 }) as u32,
                        );
                        return Err(Error::parse(
                            span,
                            format!(
                                "`{marker}` comment marker must be followed by whitespace or \
                                 end-of-line; `{}` is not a comment marker, and a non-whitespace \
                                 character flush against `{marker}` is rejected (the `//…` \
                                 operator family is reserved)",
                                marker_with_next(marker, after_marker),
                            ),
                        ));
                    }
                    // Consume past the `//` (or `///` for doc-comments).
                    self.bump(); // first /
                    self.bump(); // second /
                    if is_doc {
                        self.bump(); // third /
                    }
                    let body_start = self.pos;
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                    // Strip trailing space/tab (and any trailing CR
                    // before the newline) from the comment body. The
                    // recorded `span` keeps those bytes — it covers
                    // the whole source region of the comment, while
                    // `text` carries only the canonicalized body the
                    // pretty-printer should emit.
                    let mut end = self.pos;
                    while end > body_start {
                        let b = self.bytes[end - 1];
                        if b == b' ' || b == b'\t' || b == b'\r' {
                            end -= 1;
                        } else {
                            break;
                        }
                    }
                    let raw = std::str::from_utf8(&self.bytes[body_start..end])
                        .expect("source is UTF-8 (validated at lex entry)");
                    let span = Span::new(comment_start as u32, self.pos as u32);
                    if is_doc {
                        // Strip one optional leading space from the payload
                        // so `/// foo` stores `"foo"` and `///` stores `""`.
                        let text = raw.strip_prefix(' ').unwrap_or(raw).to_owned();
                        self.pending_trivia
                            .push(Trivia::DocCommentLine { text, span });
                    } else {
                        self.pending_trivia.push(Trivia::LineComment {
                            text: raw.to_owned(),
                            span,
                        });
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// Whether a comment marker (`//` or `///`) followed by `next` is
    /// rejected. The character immediately after the marker must be
    /// whitespace, end-of-line, or end-of-file; anything else —
    /// including a further `/` (the `////` ruler shape, and the
    /// reserved `//…` operator family) — is a parse error. The rule
    /// applies uniformly: there is no alphanumeric/underscore
    /// exception (`//foo`, `///bar`, `//=`, `////` all reject).
    fn comment_marker_rejects(next: Option<u8>) -> bool {
        match next {
            None => false,
            Some(b) => !matches!(b, b' ' | b'\t' | b'\r' | b'\n'),
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>, Error> {
        self.collect_leading_trivia()?;
        let start = self.pos;
        let Some(b) = self.peek() else {
            return Ok(None);
        };
        // Leading-minus carve-out (specs/grammar.md § "Notes (Kio
        // surface)"): when the prior context is expression-starting
        // (BOF or any token that doesn't end an expression) and a `-`
        // sits flush against a digit, the lexer consumes the `-` as
        // part of the numeric literal so `-40` is a single negative
        // literal token. After an expression-ending token (identifier,
        // literal, `)`, or `}`), the `-` is always a binary
        // operator and lexes as an ordinary op-run regardless of what
        // follows. The "flush against a digit" half is required so
        // that `- 40` (whitespace between) keeps lexing as two tokens.
        if b == b'-' && !self.prior_ends_expr && self.peek_at(1).is_some_and(|c| c.is_ascii_digit())
        {
            self.bump(); // consume the leading `-`
            let mut tok = self.lex_number(start)?;
            // The lexed number's `digits` doesn't see the leading `-`
            // we already consumed; prepend it so JS / Rust emission
            // (which prints `digits` verbatim) gets `-40` / `-3.14e9`.
            match &mut tok.kind {
                TokenKind::IntLit { digits } | TokenKind::FloatLit { digits } => {
                    digits.insert(0, '-');
                }
                _ => unreachable!("lex_number returns IntLit or FloatLit"),
            }
            tok.leading_trivia = std::mem::take(&mut self.pending_trivia);
            self.prior_ends_expr = token_ends_expression(&tok.kind);
            return Ok(Some(tok));
        }
        let kind = match b {
            b'(' => {
                self.bump();
                TokenKind::LParen
            }
            b')' => {
                self.bump();
                TokenKind::RParen
            }
            b'{' => {
                self.bump();
                TokenKind::LBrace
            }
            b'}' => {
                self.bump();
                TokenKind::RBrace
            }
            b',' => {
                self.bump();
                TokenKind::Comma
            }
            b';' => {
                self.bump();
                TokenKind::Semicolon
            }
            // Every operator character lexes as a greedy `SymbolRun`.
            // The fixed-role symbols `< > = ! & | : -` are unified
            // with the user-operator characters: a standalone `<`
            // and a fused `<=` are the same token kind, distinguished
            // only by content. `&` / `|` join an adjacent run too
            // (so `&&` / `||` lex as one token); the type-chain
            // parsers accept any all-`&` / all-`|` run as a chain
            // separator. The arrow and dot-splice spellings are not
            // carved out — they fuse with neighbours like any other
            // op-char run, and the parser peels them off at the
            // structural-recovery sites.
            b'+' | b'-' | b'*' | b'/' | b'%' | b'^' | b'~' | b'?' | b'@' | b'$' | b'\\' | b'\''
            | b'`' | b'<' | b'>' | b'=' | b'!' | b'&' | b'|' | b':' | b'.' | b'#' | b'[' | b']' => {
                self.lex_op_run()
            }
            b'"' => {
                let mut tok = self.lex_string(start)?;
                tok.leading_trivia = std::mem::take(&mut self.pending_trivia);
                self.prior_ends_expr = token_ends_expression(&tok.kind);
                return Ok(Some(tok));
            }
            b'0'..=b'9' => {
                let mut tok = self.lex_number(start)?;
                tok.leading_trivia = std::mem::take(&mut self.pending_trivia);
                self.prior_ends_expr = token_ends_expression(&tok.kind);
                return Ok(Some(tok));
            }
            b if is_ident_start(b) => self.lex_ident_or_keyword(start)?,
            _ => {
                let ch = self.bump_source_char();
                return Err(self.parse_err(start, format!("unexpected character: {ch:?}")));
            }
        };
        self.prior_ends_expr = token_ends_expression(&kind);
        Ok(Some(Token {
            kind,
            span: self.span(start),
            leading_trivia: std::mem::take(&mut self.pending_trivia),
        }))
    }

    /// Greedy operator-character run starting at the current
    /// position. Consumes characters in the operator set
    /// (`+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | : . [ ]`) until it hits
    /// a non-operator char or a block-listed sequence, and produces
    /// one [`TokenKind::SymbolRun`]. The block-list reserves `//`
    /// (line comment) and the second-or-later `/` of any run (so a
    /// run carries at most one `/`). Lexing is uniformly greedy
    /// over op-chars — no structural carve-outs for `->`, `.>`, or
    /// any other arrow-shaped sequence. Parser sites that need to
    /// recognize an arrow swallowed by greedy fusion peel it back
    /// via `SkeletonCursor::split_current_sym`.
    fn lex_op_run(&mut self) -> TokenKind {
        let start = self.pos;
        let mut slash_count = 0;
        while let Some(b) = self.peek() {
            if !is_op_char_in_run(b) {
                break;
            }
            // `//` starts a line comment, captured by
            // `collect_leading_trivia` on the next pass. Also block
            // any run carrying a second `/` so that `+/+/` and
            // similar are reserved.
            if b == b'/' {
                if slash_count >= 1 || self.peek_at(1) == Some(b'/') {
                    break;
                }
                slash_count += 1;
            }
            self.bump();
        }
        let text =
            std::str::from_utf8(&self.bytes[start..self.pos]).expect("operator bytes are ASCII");
        TokenKind::SymbolRun(text.to_owned())
    }

    fn lex_ident_or_keyword(&mut self, start: usize) -> Result<TokenKind, Error> {
        while let Some(b) = self.peek() {
            if is_ident_continue(b) {
                self.bump();
            } else {
                break;
            }
        }
        let text =
            std::str::from_utf8(&self.bytes[start..self.pos]).expect("identifier bytes are ASCII");
        // Pure-underscore runs are slot-token territory. Length
        // 1/2/3 → dedicated `Slot1` / `Slot2` / `Slot3` token
        // kinds; length 4+ is reserved for any future slot-kind
        // extension and rejected at the lexer level. A run mixing
        // `_` with letters / digits (`_foo`, `_a1`) is
        // unaffected — those stay as ordinary identifiers.
        if !text.is_empty() && text.chars().all(|c| c == '_') {
            return Ok(match text.len() {
                1 => TokenKind::Slot1,
                2 => TokenKind::Slot2,
                3 => TokenKind::Slot3,
                n => {
                    return Err(Error::parse(
                        Span::new(start as u32, self.pos as u32),
                        format!(
                            "underscore run `{text}` (length {n}) is reserved; only `_`, `__`, `___` are admissible slot tokens"
                        ),
                    ));
                }
            });
        }
        Ok(TokenKind::Ident(text.to_owned()))
    }

    fn lex_string(&mut self, start: usize) -> Result<Token, Error> {
        self.bump(); // opening quote
        let mut value = String::new();
        loop {
            let Some(b) = self.peek() else {
                return Err(self.parse_err(start, "unterminated string literal"));
            };
            match b {
                b'"' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::StrLit(value),
                        span: self.span(start),
                        leading_trivia: Vec::new(),
                    });
                }
                b'\n' => {
                    return Err(self.parse_err(start, "unterminated string literal (newline)"));
                }
                b'\\' => {
                    let escape_start = self.pos;
                    self.bump();
                    let Some(esc) = self.peek() else {
                        return Err(self.parse_err(start, "unterminated string literal"));
                    };
                    match esc {
                        b'"' => {
                            self.bump();
                            value.push('"');
                        }
                        b'\\' => {
                            self.bump();
                            value.push('\\');
                        }
                        b'/' => {
                            self.bump();
                            value.push('/');
                        }
                        b'b' => {
                            self.bump();
                            value.push('\u{0008}');
                        }
                        b'f' => {
                            self.bump();
                            value.push('\u{000C}');
                        }
                        b'n' => {
                            self.bump();
                            value.push('\n');
                        }
                        b'r' => {
                            self.bump();
                            value.push('\r');
                        }
                        b't' => {
                            self.bump();
                            value.push('\t');
                        }
                        b'u' => {
                            self.bump();
                            let mut code: u32 = 0;
                            for _ in 0..4 {
                                let Some(h) = self.peek() else {
                                    return Err(self.parse_err(
                                        escape_start,
                                        "incomplete \\u escape (need 4 hex digits)",
                                    ));
                                };
                                let digit = match h {
                                    b'0'..=b'9' => (h - b'0') as u32,
                                    b'a'..=b'f' => (h - b'a' + 10) as u32,
                                    b'A'..=b'F' => (h - b'A' + 10) as u32,
                                    _ => {
                                        return Err(self.parse_err(
                                            escape_start,
                                            "invalid hex digit in \\u escape",
                                        ));
                                    }
                                };
                                code = code * 16 + digit;
                                self.bump();
                            }
                            let Some(ch) = char::from_u32(code) else {
                                return Err(self.parse_err(
                                    escape_start,
                                    "\\u escape is a UTF-16 surrogate (not a valid code point)",
                                ));
                            };
                            value.push(ch);
                        }
                        _ => {
                            let ch = self.bump_source_char();
                            return Err(self.parse_err(
                                escape_start,
                                format!("invalid escape sequence: \\{ch}"),
                            ));
                        }
                    }
                }
                _ => {
                    // Copy one UTF-8 char (the source is &str, so multi-byte
                    // sequences stay aligned).
                    let ch_start = self.pos;
                    let len = utf8_char_len(b);
                    self.pos += len;
                    let chunk = std::str::from_utf8(&self.bytes[ch_start..self.pos])
                        .map_err(|_| self.parse_err(ch_start, "invalid UTF-8 in string literal"))?;
                    value.push_str(chunk);
                }
            }
        }
    }

    fn lex_number(&mut self, start: usize) -> Result<Token, Error> {
        let mut digits = String::new();
        self.consume_digit_run(&mut digits);

        // Float case: a `.` followed by at least one digit.
        let is_float = self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(b'0'..=b'9'));

        if is_float {
            digits.push('.');
            self.bump(); // consume the dot
            self.consume_digit_run(&mut digits);

            // Optional exponent: e/E, optional sign, then digits.
            if matches!(self.peek(), Some(b'e' | b'E')) {
                let exp_start = self.pos;
                let exp_marker = self.peek().unwrap();
                self.bump();
                let sign = matches!(self.peek(), Some(b'+' | b'-'));
                let sign_byte = if sign {
                    Some(self.peek().unwrap())
                } else {
                    None
                };
                if sign {
                    self.bump();
                }
                if !matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.parse_err(exp_start, "exponent has no digits"));
                }
                digits.push(exp_marker as char);
                if let Some(s) = sign_byte {
                    digits.push(s as char);
                }
                self.consume_digit_run(&mut digits);
            }

            return Ok(Token {
                kind: TokenKind::FloatLit { digits },
                span: self.span(start),
                leading_trivia: Vec::new(),
            });
        }

        // Integer case. Literals carry no type suffix — the type is
        // pinned by a trailing `(Type)` call form or resolved from
        // context by the typer (see `specs/language.md` § Literals). A
        // suffix-shaped identifier glued to the digits (`100i32`) lexes
        // as the literal followed by a separate identifier token, which
        // the parser rejects as adjacent-with-no-operator.
        Ok(Token {
            kind: TokenKind::IntLit { digits },
            span: self.span(start),
            leading_trivia: Vec::new(),
        })
    }

    /// Consumes a run of digits, allowing single `_` separators between them,
    /// appending the digits (without `_`) to `out`.
    fn consume_digit_run(&mut self, out: &mut String) {
        while let Some(b) = self.peek() {
            match b {
                b'0'..=b'9' => {
                    out.push(b as char);
                    self.bump();
                }
                b'_' if matches!(self.peek_at(1), Some(b'0'..=b'9')) => {
                    self.bump();
                }
                _ => break,
            }
        }
    }
}

/// True iff the kind of the just-emitted token closes an expression.
/// Drives the leading-minus carve-out in [`Lexer::next_token`]: after
/// a token that ends an expression, a `-` followed by a digit is a
/// binary operator (subtraction); otherwise the lexer consumes the
/// `-` into the following numeric literal.
///
/// **Expression-ending** kinds:
/// - any identifier (`Ident(_)`) — value reference / contextual
///   keyword in expression-tail position (`a-1` is subtraction);
/// - any literal (`IntLit`, `FloatLit`, `StrLit`, `BoolLit`) —
///   `1-2` is subtraction;
/// - closing structural brackets `)` and `}` — closing a call or block.
///
/// A placeholder reference is a value, but it lexes as a `SymbolRun`
/// matching the lambda's marker — indistinguishable at lex time from
/// an operator awaiting an operand — so it is *not* expression-ending
/// here. The indexed form preserves subtraction anyway (`#1 - 2`
/// lexes as `SymbolRun("#")`, `IntLit("1")`, `-`, `2`; the `IntLit`
/// is expression-ending, so the `-` is binary). For a bare marker,
/// `#  -  2` (the `-` not flush against the digit) is binary
/// subtraction; `# -2` fuses `-2` into a negative literal exactly as
/// `1 + -2` does after the `+` op-run — both spell subtraction with
/// the `-` separated from the digit (`# - 2`).
///
/// Everything else (`LParen`, `LBrace`, `Comma`, `Semicolon`, every
/// `SymbolRun` including `]`, `=`, `<`, `>`, `:`,
/// `,` and arithmetic op-runs, slot tokens)
/// leaves the lexer in an expression-starting position.
fn token_ends_expression(kind: &TokenKind) -> bool {
    match kind {
        TokenKind::Ident(_) => true,
        TokenKind::IntLit { .. }
        | TokenKind::FloatLit { .. }
        | TokenKind::StrLit(_)
        | TokenKind::BoolLit(_) => true,
        TokenKind::RParen | TokenKind::RBrace => true,
        TokenKind::LParen
        | TokenKind::LBrace
        | TokenKind::Comma
        | TokenKind::Semicolon
        | TokenKind::SymbolRun(_)
        | TokenKind::Slot1
        | TokenKind::Slot2
        | TokenKind::Slot3 => false,
    }
}

/// Characters admitted into a greedy [`TokenKind::SymbolRun`]. The
/// full operator-character set: `+ - * / % ^ ~ ? @ # $ \ ' ` < > =
/// ! & | : . [ ]`. The fixed-role symbols (`< > [ ] = ! & | : ->` plus the
/// dot-splice family `.> .>> .< .<<`) are unified with the user-
/// operator characters — they all join a run when adjacent, and a
/// standalone one lexes as a one-character run.
/// `.` joins runs alongside the rest (so the dot-splice family fuses
/// by ordinary greedy lexing, and `..`, `.+`, `<.>` lex as single
/// runs). Parser sites decide which dotted runs are admissible as
/// user operator tokens. Brackets join their neighbours under the same
/// maximal-run rule; parser sites peel them only where a forall binder
/// structurally requires them.
fn is_op_char_in_run(b: u8) -> bool {
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
            | b':'
            | b'&'
            | b'|'
            | b'.'
            | b'['
            | b']'
    )
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Length in bytes of the UTF-8 character starting with the given lead byte.
/// Stray continuation bytes (0x80..0xC0) advance 1 to keep the lexer making
/// progress; in practice they never appear, since the source is `&str`.
fn utf8_char_len(b: u8) -> usize {
    match b {
        0x00..=0xBF => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Diagnostic;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn empty_source_yields_no_tokens() {
        assert_eq!(kinds(""), Vec::<TokenKind>::new());
        assert_eq!(kinds("   \n\t  // a comment\n"), Vec::<TokenKind>::new());
    }

    #[test]
    fn keyword_words_lex_as_idents() {
        // Every keyword-shaped word is lexed as a plain Ident; the
        // parser disambiguates by position. See `specs/language.md`
        // § "Contextual keywords" for the spec promise.
        let src = "fn type literal alias newtype let fn use module labels if else match";
        assert_eq!(
            kinds(src),
            vec![
                TokenKind::Ident("fn".into()),
                TokenKind::Ident("type".into()),
                TokenKind::Ident("literal".into()),
                TokenKind::Ident("alias".into()),
                TokenKind::Ident("newtype".into()),
                TokenKind::Ident("let".into()),
                TokenKind::Ident("fn".into()),
                TokenKind::Ident("use".into()),
                TokenKind::Ident("module".into()),
                TokenKind::Ident("labels".into()),
                TokenKind::Ident("if".into()),
                TokenKind::Ident("else".into()),
                TokenKind::Ident("match".into()),
            ]
        );
    }

    #[test]
    fn fn_followed_by_dot_does_not_fuse() {
        assert_eq!(
            kinds("fn."),
            vec![TokenKind::Ident("fn".into()), TokenKind::sym(".")]
        );
        assert_eq!(
            kinds("fn.let"),
            vec![
                TokenKind::Ident("fn".into()),
                TokenKind::sym("."),
                TokenKind::Ident("let".into()),
            ]
        );
    }

    #[test]
    fn fn_followed_by_slash_does_not_fuse() {
        assert_eq!(
            kinds("fn/"),
            vec![TokenKind::Ident("fn".into()), TokenKind::sym("/")]
        );
        assert_eq!(
            kinds("fn/b"),
            vec![
                TokenKind::Ident("fn".into()),
                TokenKind::sym("/"),
                TokenKind::Ident("b".into()),
            ]
        );
    }

    #[test]
    fn fn_then_space_then_hash_is_two_tokens() {
        assert_eq!(
            kinds("fn #"),
            vec![TokenKind::Ident("fn".into()), TokenKind::sym("#")]
        );
    }

    #[test]
    fn hash_lexes_uniformly_as_op_run_then_optional_int() {
        // `#` is an ordinary op-char: a standalone `#` is a
        // one-character run, `##` fuses, and `#1` is the run
        // followed by an `IntLit` (the indexed-placeholder suffix
        // the parser peels off span-adjacently). No dedicated
        // placeholder token kinds exist.
        assert_eq!(
            kinds("# #1 #42 #1234"),
            vec![
                TokenKind::sym("#"),
                TokenKind::sym("#"),
                TokenKind::IntLit { digits: "1".into() },
                TokenKind::sym("#"),
                TokenKind::IntLit {
                    digits: "42".into()
                },
                TokenKind::sym("#"),
                TokenKind::IntLit {
                    digits: "1234".into()
                },
            ]
        );
    }

    #[test]
    fn hash_zero_lexes_as_run_then_int() {
        // `#0` is no longer a lexer error — the slot-index range
        // check (`>= 1`) moved to the parser, which sees the
        // `SymbolRun("#")` + `IntLit("0")` pair. The lexer just
        // produces the two uniform tokens.
        assert_eq!(
            kinds("#0"),
            vec![
                TokenKind::sym("#"),
                TokenKind::IntLit { digits: "0".into() },
            ]
        );
    }

    #[test]
    fn pure_underscore_run_of_four_or_more_is_lex_error() {
        // `_`, `__`, `___` are admissible (slot tokens at the
        // lexer level today; the parser routes them at slot
        // positions). `____` and longer are reserved for any
        // future slot-kind extension and rejected here.
        for src in ["____", "_____", "________"] {
            let err = lex(src).unwrap_err();
            let (_, message) = err.diag();
            assert!(
                message.contains("reserved") && message.contains("admissible slot tokens"),
                "lex({src:?}): got: {message}"
            );
        }
    }

    #[test]
    fn mixed_underscore_idents_unaffected() {
        // The carve-out only fires on **pure**-underscore runs.
        // `_foo`, `_a1`, `__name__`-style idents stay as
        // identifiers.
        assert_eq!(
            kinds("_foo _a1"),
            vec![
                TokenKind::Ident("_foo".into()),
                TokenKind::Ident("_a1".into()),
            ]
        );
    }

    #[test]
    fn bool_words_lex_as_identifiers() {
        assert_eq!(
            kinds("true false"),
            vec![
                TokenKind::Ident("true".into()),
                TokenKind::Ident("false".into()),
            ]
        );
    }

    #[test]
    fn identifiers_basic() {
        assert_eq!(
            kinds("foo Bar _baz qux_2"),
            vec![
                TokenKind::Ident("foo".into()),
                TokenKind::Ident("Bar".into()),
                TokenKind::Ident("_baz".into()),
                TokenKind::Ident("qux_2".into()),
            ]
        );
    }

    #[test]
    fn contextual_keywords_lex_as_idents() {
        assert_eq!(
            kinds("pub from as type pkg env bridge source target rec role constructor projector"),
            vec![
                TokenKind::Ident("pub".into()),
                TokenKind::Ident("from".into()),
                TokenKind::Ident("as".into()),
                TokenKind::Ident("type".into()),
                TokenKind::Ident("pkg".into()),
                TokenKind::Ident("env".into()),
                TokenKind::Ident("bridge".into()),
                TokenKind::Ident("source".into()),
                TokenKind::Ident("target".into()),
                TokenKind::Ident("rec".into()),
                TokenKind::Ident("role".into()),
                TokenKind::Ident("constructor".into()),
                TokenKind::Ident("projector".into()),
            ]
        );
    }

    #[test]
    fn reserved_underscore_idents_lex_as_idents() {
        assert_eq!(
            kinds("__pair__ __intrinsics__ __Tag_foo__"),
            vec![
                TokenKind::Ident("__pair__".into()),
                TokenKind::Ident("__intrinsics__".into()),
                TokenKind::Ident("__Tag_foo__".into()),
            ]
        );
    }

    #[test]
    fn punctuation() {
        // The structural punctuators (`( ) { } , ;`) keep
        // dedicated kinds; every fixed-role symbol — including
        // `< > [ ] : -> .> .>> .< .<< | & = ! .` — lexes as a `SymbolRun`
        // carrying its spelling. `.` joins greedy op-runs and
        // appears as a one-char `SymbolRun` when standalone.
        assert_eq!(
            kinds("( ) { } < > [ ] , ; : -> .> .>> .< .<< | & = . !"),
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
                TokenKind::sym("->"),
                TokenKind::sym(".>"),
                TokenKind::sym(".>>"),
                TokenKind::sym(".<"),
                TokenKind::sym(".<<"),
                TokenKind::sym("|"),
                TokenKind::sym("&"),
                TokenKind::sym("="),
                TokenKind::sym("."),
                TokenKind::sym("!"),
            ]
        );
    }

    #[test]
    fn dash_arrow_lexes_as_arrow() {
        // `->` is the function-type / return-type arrow — a
        // two-character `SymbolRun`.
        let toks = lex("->").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].kind.is_sym("->"));
    }

    #[test]
    fn dot_splice_lexes_as_symbol_run() {
        // Dot-splice symbols are produced by ordinary greedy fusion
        // of `.` and following op-chars.
        let toks = lex(".>>").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].kind.is_sym(".>>"));
    }

    #[test]
    fn standalone_dot_lexes_as_one_char_symbol_run() {
        // A standalone `.` lexes as a one-character `SymbolRun` —
        // `.` is an ordinary op-char that joins greedy runs.
        let toks = lex(".").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].kind.is_sym("."));
    }

    #[test]
    fn dot_then_ident_lexes_as_two_tokens() {
        assert_eq!(
            kinds("foo.bar"),
            vec![
                TokenKind::Ident("foo".into()),
                TokenKind::sym("."),
                TokenKind::Ident("bar".into()),
            ]
        );
    }

    #[test]
    fn dot_runs_fuse_under_greedy_lexing() {
        // Dotted op-char runs fuse uniformly. Whether a run is
        // admissible as a user operator token is a parser-side rule.
        let toks = lex(".. .+ <.>").unwrap();
        assert_eq!(toks.len(), 3);
        assert!(toks[0].kind.is_sym(".."));
        assert!(toks[1].kind.is_sym(".+"));
        assert!(toks[2].kind.is_sym("<.>"));
    }

    #[test]
    fn integer_literal_lexes_bare() {
        // Literals carry no type suffix — `42` lexes as a bare
        // `IntLit`. Digit separators are stripped.
        for (src, digits) in [("0", "0"), ("42", "42"), ("1_000_000", "1000000")] {
            let toks = lex(src).unwrap();
            assert_eq!(toks.len(), 1, "for {src}");
            match &toks[0].kind {
                TokenKind::IntLit { digits: d } => assert_eq!(d, digits),
                k => panic!("expected IntLit, got {k:?} for {src}"),
            }
        }
    }

    #[test]
    fn suffix_shaped_ident_after_int_lexes_separately() {
        // The old `100i32` suffix form is gone: the digits lex as a
        // bare `IntLit` and the suffix-shaped word as a separate
        // `Ident`. The parser rejects the adjacency.
        let toks = lex("42i32").unwrap();
        assert_eq!(toks.len(), 2);
        assert!(matches!(&toks[0].kind, TokenKind::IntLit { digits } if digits == "42"));
        assert_eq!(toks[1].kind, TokenKind::Ident("i32".into()));
    }

    #[test]
    fn float_literal_lexes_bare() {
        let toks = lex("3.14").unwrap();
        assert_eq!(toks.len(), 1);
        match &toks[0].kind {
            TokenKind::FloatLit { digits } => assert_eq!(digits, "3.14"),
            k => panic!("expected FloatLit, got {k:?}"),
        }
    }

    #[test]
    fn float_with_exponent() {
        let toks = lex("1.0e-9").unwrap();
        assert_eq!(toks.len(), 1);
        match &toks[0].kind {
            TokenKind::FloatLit { digits } => assert_eq!(digits, "1.0e-9"),
            k => panic!("expected FloatLit, got {k:?}"),
        }
    }

    // ---- Leading-minus carve-out ------------------------------------------
    //
    // `specs/grammar.md` § "Notes (Kio surface)": the parser admits no
    // prefix-`-` form, so a bare `-` followed by a numeric literal would
    // otherwise be a parse error. The lexer's carve-out fuses the `-`
    // into the literal when the prior context is expression-starting and
    // the `-` sits flush against a digit, producing a single negative
    // literal token. After an expression-ending token (identifier,
    // literal, `)`, or `}`), the `-` is always binary subtraction. A
    // `SymbolRun`, including a bracket-bearing run, instead leaves the lexer
    // in an expression-starting position.

    #[test]
    fn leading_minus_at_bof_lexes_as_negative_literal() {
        // Start of file is an expression-starting position; `-40`
        // immediately at the start lexes as one IntLit with `digits =
        // "-40"`.
        let toks = lex("-40").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(matches!(&toks[0].kind, TokenKind::IntLit { digits } if digits == "-40"));
    }

    #[test]
    fn leading_minus_after_open_paren_lexes_as_negative_literal() {
        // After `(` (a token that does not end an expression), `-40`
        // fuses into one negative literal. Models `f(-40)`.
        let toks = lex("f(-40)").unwrap();
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[0].kind, TokenKind::Ident("f".into()));
        assert_eq!(toks[1].kind, TokenKind::LParen);
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "-40"));
        assert_eq!(toks[3].kind, TokenKind::RParen);
    }

    #[test]
    fn leading_minus_after_equals_lexes_as_negative_literal() {
        // After `=` (a SymbolRun that does not end an expression),
        // `-40` is one negative literal. Models `let x = -40;`.
        let toks = lex("let x = -40;").unwrap();
        assert_eq!(toks.len(), 5);
        assert_eq!(toks[0].kind, TokenKind::Ident("let".into()));
        assert_eq!(toks[1].kind, TokenKind::Ident("x".into()));
        assert!(toks[2].kind.is_sym("="));
        assert!(matches!(&toks[3].kind, TokenKind::IntLit { digits } if digits == "-40"));
        assert_eq!(toks[4].kind, TokenKind::Semicolon);
    }

    #[test]
    fn leading_minus_after_comma_lexes_as_negative_literal() {
        // Same carve-out fires after `,`. Models `f(1, -2)`.
        let toks = lex("f(1, -2)").unwrap();
        assert_eq!(toks.len(), 6);
        assert_eq!(toks[0].kind, TokenKind::Ident("f".into()));
        assert_eq!(toks[1].kind, TokenKind::LParen);
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "1"));
        assert_eq!(toks[3].kind, TokenKind::Comma);
        assert!(matches!(&toks[4].kind, TokenKind::IntLit { digits } if digits == "-2"));
        assert_eq!(toks[5].kind, TokenKind::RParen);
    }

    #[test]
    fn binary_minus_after_int_literal_lexes_as_subtraction() {
        // After an IntLit (which ends an expression), the `-` is
        // binary subtraction regardless of adjacency. Models `1-2`.
        let toks = lex("1-2").unwrap();
        assert_eq!(toks.len(), 3);
        assert!(matches!(&toks[0].kind, TokenKind::IntLit { digits } if digits == "1"));
        assert!(toks[1].kind.is_sym("-"));
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "2"));
    }

    #[test]
    fn binary_minus_after_ident_lexes_as_subtraction() {
        // After an Ident, the `-` is binary subtraction. Models `a-1`.
        let toks = lex("a-1").unwrap();
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].kind, TokenKind::Ident("a".into()));
        assert!(toks[1].kind.is_sym("-"));
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "1"));
    }

    #[test]
    fn binary_minus_after_close_paren_lexes_as_subtraction() {
        // After `)` (closes an expression), the `-` is binary. Models
        // `(x)-1`.
        let toks = lex("(x)-1").unwrap();
        assert_eq!(toks.len(), 5);
        assert_eq!(toks[0].kind, TokenKind::LParen);
        assert_eq!(toks[1].kind, TokenKind::Ident("x".into()));
        assert_eq!(toks[2].kind, TokenKind::RParen);
        assert!(toks[3].kind.is_sym("-"));
        assert!(matches!(&toks[4].kind, TokenKind::IntLit { digits } if digits == "1"));
    }

    #[test]
    fn whitespace_between_minus_and_digit_keeps_two_tokens() {
        // The "flush against a digit" requirement: `- 40` with
        // whitespace between is two tokens even at start-of-file.
        // Today the parser rejects the bare leading `-`; the future
        // user-defined prefix-`-` op would parse it.
        let toks = lex("- 40").unwrap();
        assert_eq!(toks.len(), 2);
        assert!(toks[0].kind.is_sym("-"));
        assert!(matches!(&toks[1].kind, TokenKind::IntLit { digits } if digits == "40"));
    }

    #[test]
    fn leading_minus_followed_by_ident_is_two_tokens() {
        // The carve-out fires on digits only — `-y` stays as two
        // tokens (SymbolRun(`-`) + Ident).
        let toks = lex("-y").unwrap();
        assert_eq!(toks.len(), 2);
        assert!(toks[0].kind.is_sym("-"));
        assert_eq!(toks[1].kind, TokenKind::Ident("y".into()));
    }

    #[test]
    fn leading_minus_on_float_literal_lexes_as_negative_float() {
        // The carve-out fires on the leading digit; whether the
        // resulting literal is Int or Float is decided by what follows.
        // `-3.14` at start-of-file lexes as one FloatLit with digits
        // `"-3.14"`.
        let toks = lex("-3.14").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(matches!(&toks[0].kind, TokenKind::FloatLit { digits } if digits == "-3.14"));
    }

    #[test]
    fn leading_minus_after_open_brace_lexes_as_negative_literal() {
        // After `{` (a token that does not end an expression — opens
        // a block expression), `-40` fuses. Models a block whose
        // first expression is a negative literal.
        let toks = lex("{-40}").unwrap();
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].kind, TokenKind::LBrace);
        assert!(matches!(&toks[1].kind, TokenKind::IntLit { digits } if digits == "-40"));
        assert_eq!(toks[2].kind, TokenKind::RBrace);
    }

    #[test]
    fn bracket_operator_run_does_not_end_expression_for_minus() {
        // `]` is an ordinary operator run, not a lexer-level structural
        // closer. Flush `]-` therefore fuses, while a separated `] -1`
        // leaves the lexer in expression-starting position and reads a
        // negative literal.
        let flush = lex("]-1").unwrap();
        assert_eq!(flush.len(), 2);
        assert!(flush[0].kind.is_sym("]-"));
        assert!(matches!(&flush[1].kind, TokenKind::IntLit { digits } if digits == "1"));

        let separated = lex("] -1").unwrap();
        assert_eq!(separated.len(), 2);
        assert!(separated[0].kind.is_sym("]"));
        assert!(matches!(&separated[1].kind, TokenKind::IntLit { digits } if digits == "-1"));
    }

    #[test]
    fn binary_minus_after_close_brace_lexes_as_subtraction() {
        // `}` ends a block expression — `} -1` is binary subtraction
        // regardless of adjacency.
        let toks = lex("}-1").unwrap();
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].kind, TokenKind::RBrace);
        assert!(toks[1].kind.is_sym("-"));
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "1"));
    }

    #[test]
    fn binary_minus_after_bool_shaped_identifier_lexes_as_subtraction() {
        // `true` / `false` are ordinary identifiers now, and
        // identifiers end an expression for adjacency-sensitive
        // minus lexing.
        let toks = lex("true-1").unwrap();
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].kind, TokenKind::Ident("true".into()));
        assert!(toks[1].kind.is_sym("-"));
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "1"));
    }

    #[test]
    fn binary_minus_after_str_literal_lexes_as_subtraction() {
        // String literals end an expression. (Subtracting a string is
        // an ill-typed program; the lex output is what's pinned here.)
        let toks = lex(r#""s"-1"#).unwrap();
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].kind, TokenKind::StrLit("s".into()));
        assert!(toks[1].kind.is_sym("-"));
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "1"));
    }

    #[test]
    fn leading_minus_after_arithmetic_op_lexes_as_negative_literal() {
        // After an arithmetic op-run (which doesn't end an
        // expression), the next `-N` flush against a digit fuses
        // into a negative literal. Models `1 + -2` (which would
        // parse as `1 + (-2)`). The space between `+` and `-` is
        // required so the greedy op-run lexer doesn't fuse `+-`
        // into a single `SymbolRun`.
        let toks = lex("1 + -2").unwrap();
        assert_eq!(toks.len(), 3);
        assert!(matches!(&toks[0].kind, TokenKind::IntLit { digits } if digits == "1"));
        assert!(toks[1].kind.is_sym("+"));
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "-2"));
    }

    #[test]
    fn op_run_greedily_fuses_plus_minus_before_digit() {
        // The leading-minus carve-out does not undo the lexer's
        // greedy op-run rule: `1+-2` lexes as `IntLit(1)`,
        // `SymbolRun("+-")`, `IntLit(2)`. The carve-out only fires
        // when the `-` is the *leading* char at a token boundary —
        // here `-` is the second char of a greedy `+-` run.
        let toks = lex("1+-2").unwrap();
        assert_eq!(toks.len(), 3);
        assert!(matches!(&toks[0].kind, TokenKind::IntLit { digits } if digits == "1"));
        assert!(toks[1].kind.is_sym("+-"));
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "2"));
    }

    #[test]
    fn leading_minus_after_newline_uses_prior_token_context() {
        // Newlines are trivia; the carve-out keys on the last
        // meaningful token, not on whitespace. `a\n-1` keeps `a`'s
        // expression-ending state, so the `-` is binary subtraction.
        let toks = lex("a\n-1").unwrap();
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].kind, TokenKind::Ident("a".into()));
        assert!(toks[1].kind.is_sym("-"));
        assert!(matches!(&toks[2].kind, TokenKind::IntLit { digits } if digits == "1"));
    }

    #[test]
    fn leading_minus_after_line_comment_uses_prior_token_context() {
        // Line comments are trivia. `(\n// note\n-40)` — after `(`,
        // the comment is trivia, then `-40` fuses into one literal.
        let toks = lex("(\n// note\n-40)").unwrap();
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].kind, TokenKind::LParen);
        assert!(matches!(&toks[1].kind, TokenKind::IntLit { digits } if digits == "-40"));
        assert_eq!(toks[2].kind, TokenKind::RParen);
    }

    #[test]
    fn integer_then_dot_then_ident_does_not_form_float() {
        // `42.foo` — `.foo` is a member access, not part of the int.
        // `lex_number` only continues into a float when `.` is
        // followed by a digit, so `.foo` exits and the next
        // pass produces `SymbolRun(".")` then `Ident("foo")`.
        let toks = lex("42.foo").unwrap();
        assert_eq!(toks.len(), 3);
        assert!(matches!(toks[0].kind, TokenKind::IntLit { .. }));
        assert!(toks[1].kind.is_sym("."));
        assert_eq!(toks[2].kind, TokenKind::Ident("foo".into()));
    }

    #[test]
    fn string_simple() {
        let toks = lex(r#""hello""#).unwrap();
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::StrLit("hello".into()));
    }

    #[test]
    fn string_with_escapes() {
        let toks = lex(r#""line1\nline2\t\"q\"\\back\/slash""#).unwrap();
        assert_eq!(
            toks[0].kind,
            TokenKind::StrLit("line1\nline2\t\"q\"\\back/slash".into())
        );
    }

    #[test]
    fn string_with_unicode_escape() {
        let toks = lex(r#""é中""#).unwrap();
        assert_eq!(toks[0].kind, TokenKind::StrLit("é中".into()));
    }

    #[test]
    fn string_unterminated_is_error() {
        let err = lex(r#""no end"#).unwrap_err();
        let (_, message) = err.diag();
        assert!(message.contains("unterminated"));
    }

    #[test]
    fn string_with_newline_is_error() {
        let err = lex("\"line1\nline2\"").unwrap_err();
        let (_, message) = err.diag();
        assert!(message.contains("newline"));
    }

    #[test]
    fn string_with_invalid_escape_is_error() {
        let err = lex(r#""\q""#).unwrap_err();
        let (_, message) = err.diag();
        assert!(message.contains("invalid escape"));
    }

    #[test]
    fn string_with_surrogate_escape_is_error() {
        // \uD800 is a high surrogate, not a valid scalar value.
        let err = lex(r#""\uD800""#).unwrap_err();
        let (_, message) = err.diag();
        assert!(message.contains("surrogate"));
    }

    #[test]
    fn line_comment_is_dropped() {
        let toks = lex("foo // a comment\nbar").unwrap();
        assert_eq!(
            toks.iter().map(|t| &t.kind).collect::<Vec<_>>(),
            vec![
                &TokenKind::Ident("foo".into()),
                &TokenKind::Ident("bar".into()),
            ]
        );
    }

    #[test]
    fn comment_marker_requires_whitespace_or_eol() {
        // `//` / `///` followed by whitespace, end-of-line, or
        // end-of-file are the only admissible comment forms.
        assert!(lex("// ok\n").is_ok());
        assert!(lex("//\tok\n").is_ok());
        assert!(lex("//\n").is_ok()); // empty comment (EOL right after)
        assert!(lex("//").is_ok()); // empty comment at EOF
        assert!(lex("/// doc\n").is_ok());
        assert!(lex("///\n").is_ok());
        assert!(lex("///").is_ok());
        assert!(lex("foo //").is_ok());
        // A non-whitespace character flush against the marker rejects.
        assert!(lex("//foo\n").is_err());
        assert!(lex("//=\n").is_err());
        assert!(lex("///doc\n").is_err());
        assert!(lex("//1\n").is_err());
        assert!(lex("//_x\n").is_err());
    }

    #[test]
    fn four_or_more_slashes_reject() {
        // `////` and longer are no longer comment rulers — the fourth
        // `/` is a non-whitespace character flush against the `///`
        // marker.
        assert!(lex("//// ruler\n").is_err());
        assert!(lex("///////\n").is_err());
        // The error span covers the `///` marker.
        let err = lex("//// ruler\n").unwrap_err();
        match err {
            Error::Parse(Diagnostic { span, .. }) => {
                assert_eq!((span.start, span.end), (0, 3));
            }
            other => panic!("expected Error::Parse, got {other:?}"),
        }
    }

    #[test]
    fn doc_comment_lexes_as_doc_trivia() {
        let toks = lex("/// the answer\nfoo").unwrap();
        assert_eq!(toks.len(), 1);
        assert_eq!(
            toks[0].leading_trivia.first(),
            Some(&Trivia::DocCommentLine {
                text: "the answer".into(),
                span: Span::new(0, 14),
            })
        );
    }

    #[test]
    fn bare_at_lexes_as_op_token() {
        let toks = lex("@").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].kind.is_sym("@"));
    }

    #[test]
    fn bare_minus_lexes_as_op_token() {
        // `-` always lexes as a greedy op-run; the arrow `->` is
        // its own two-character run, not a lone `-`.
        let toks = lex("-").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].kind.is_sym("-"));
    }

    #[test]
    fn bare_question_lexes_as_op_token() {
        let toks = lex("?").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].kind.is_sym("?"));
    }

    #[test]
    fn op_run_greedy() {
        // Multi-char operator runs greedily lex as a single token.
        let toks = lex("++").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].kind.is_sym("++"));

        let toks = lex("+- *").unwrap();
        assert_eq!(toks.len(), 2);
        assert!(toks[0].kind.is_sym("+-"));
        assert!(toks[1].kind.is_sym("*"));
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
    fn op_run_absorbs_dash_arrow_under_greedy_fusion() {
        // The lexer is uniformly greedy over op-chars — `+->` lexes
        // as one `SymbolRun("+->")`. Parser sites that want to
        // recognize the trailing `->` peel it back via
        // `SkeletonCursor::split_current_sym`.
        let toks = lex("+->").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].kind.is_sym("+->"));
    }

    #[test]
    fn op_run_absorbs_dot_splice_under_greedy_fusion() {
        // `+.>>` lexes as one greedy `SymbolRun("+.>>")` — dot-
        // splice spellings are not carved out. The parser peels the
        // relevant prefix at its recognition site when it needs to.
        let toks = lex("+.>>").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].kind.is_sym("+.>>"));
    }

    #[test]
    fn apostrophe_and_backtick_lex_as_op_tokens() {
        // `'` and `` ` `` are part of the greedy op-char set,
        // available as user-defined operator tokens.
        let toks = lex("' ` '`+").unwrap();
        assert_eq!(toks.len(), 3);
        assert!(toks[0].kind.is_sym("'"));
        assert!(toks[1].kind.is_sym("`"));
        assert!(toks[2].kind.is_sym("'`+"));
    }

    #[test]
    fn invalid_unicode_tokens_report_complete_scalars_and_byte_spans() {
        for (ch, message) in [
            ('π', "unexpected character: 'π'"),
            ('中', "unexpected character: '中'"),
            ('🦀', "unexpected character: '🦀'"),
        ] {
            for prefix in ["", "module main;\n", "// π中🦀\nmodule main;\n"] {
                let source = format!("{prefix}{ch} trailing");
                let error =
                    lex(&source).expect_err("non-ASCII characters are not identifier tokens");
                assert_eq!(
                    (error.diagnostic().span, error.diagnostic().message.as_str()),
                    (
                        Span::new(prefix.len() as u32, (prefix.len() + ch.len_utf8()) as u32),
                        message
                    ),
                    "wrong source character for {source:?}"
                );
            }
        }
    }

    #[test]
    fn invalid_unicode_escapes_report_complete_scalars_and_byte_spans() {
        for (ch, message) in [
            ('π', "invalid escape sequence: \\π"),
            ('中', "invalid escape sequence: \\中"),
            ('🦀', "invalid escape sequence: \\🦀"),
        ] {
            for prefix in ["\"", "module main;\n\"début "] {
                let source = format!("{prefix}\\{ch}\"");
                let error = lex(&source).expect_err("only the specified escapes are admitted");
                assert_eq!(
                    (error.diagnostic().span, error.diagnostic().message.as_str()),
                    (
                        Span::new(
                            prefix.len() as u32,
                            (prefix.len() + 1 + ch.len_utf8()) as u32
                        ),
                        message
                    ),
                    "wrong escape character for {source:?}"
                );
            }
        }
    }

    #[test]
    fn unicode_diagnostic_repairs_preserve_ascii_and_literal_grammar() {
        let error = lex("\x07").expect_err("BEL is not a token");
        assert_eq!(error.diagnostic().span, Span::new(0, 1));
        assert_eq!(error.diagnostic().message, "unexpected character: '\\u{7}'");
        let error = lex("\"\\q\"").expect_err("q is not an escape");
        assert_eq!(error.diagnostic().span, Span::new(1, 3));
        assert_eq!(error.diagnostic().message, "invalid escape sequence: \\q");
        assert_eq!(
            kinds("\"π中🦀\""),
            vec![TokenKind::StrLit("π中🦀".to_owned())]
        );
        assert_eq!(
            kinds("// π中🦀\nlet ascii"),
            vec![
                TokenKind::Ident("let".to_owned()),
                TokenKind::Ident("ascii".to_owned())
            ]
        );
        let error = lex("naπme").expect_err("identifier grammar stays ASCII-only");
        assert_eq!(error.diagnostic().span.start, 2);
    }

    #[test]
    fn unknown_char_is_error() {
        // Non-operator non-ident chars error. With the op-char
        // set extended to `$` and `\`, those are valid tokens
        // now too. Use a genuinely-unused char for the baseline.
        let err = lex("\x07").unwrap_err();
        let (_, message) = err.diag();
        assert!(message.contains("unexpected character"));
    }

    #[test]
    fn spans_track_byte_offsets() {
        let toks = lex("foo bar").unwrap();
        assert_eq!(toks[0].span, Span::new(0, 3));
        assert_eq!(toks[1].span, Span::new(4, 7));
    }

    #[test]
    fn hello_world_package_file_lexes() {
        let src = r#"package hello;

env {
  type String role(str);
  fn print(p0: String) -> .;
}

bridge main {
  type String = String;
  fn print(p0: String) -> . = print;
}

export {
  fn main() -> . = main.main;
}
"#;
        let toks = lex(src).unwrap();
        // Smoke check: lex succeeds, contextual keywords come through as Idents,
        // and there are no string literals in the package file.
        assert!(
            toks.iter()
                .any(|t| t.kind == TokenKind::Ident("package".into()))
        );
        assert!(
            toks.iter()
                .any(|t| t.kind == TokenKind::Ident("bridge".into()))
        );
        assert!(
            toks.iter()
                .any(|t| t.kind == TokenKind::Ident("role".into()))
        );
        assert!(!toks.iter().any(|t| matches!(t.kind, TokenKind::StrLit(_))));
    }

    #[test]
    fn hello_world_module_file_lexes() {
        let src = r#"module main;

env {
  type String role(str);
  fn print(p0: String) -> .;
}

pub fn main() -> . {
  print("Hello, world!")
}
"#;
        let toks = lex(src).unwrap();
        assert!(
            toks.iter()
                .any(|t| matches!(&t.kind, TokenKind::StrLit(s) if s == "Hello, world!"))
        );
        assert!(
            toks.iter()
                .any(|t| matches!(&t.kind, TokenKind::Ident(s) if s == "module"))
        );
        assert!(
            toks.iter()
                .any(|t| matches!(&t.kind, TokenKind::Ident(s) if s == "fn"))
        );
    }

    // ---- Trivia model -----------------------------------------------------

    #[test]
    fn first_token_carries_no_leading_trivia_when_source_starts_with_token() {
        let toks = lex("foo").unwrap();
        assert_eq!(toks.len(), 1);
        assert!(toks[0].leading_trivia.is_empty());
    }

    #[test]
    fn newlines_between_tokens_become_newline_trivia() {
        let toks = lex("a\nb\n\nc").unwrap();
        assert_eq!(toks.len(), 3);
        // First token has no leading trivia.
        assert!(toks[0].leading_trivia.is_empty());
        // Second token's leading trivia is a single Newline.
        assert_eq!(toks[1].leading_trivia, vec![Trivia::Newline]);
        // Third token has two consecutive Newlines (blank line).
        assert_eq!(
            toks[2].leading_trivia,
            vec![Trivia::Newline, Trivia::Newline]
        );
    }

    #[test]
    fn horizontal_whitespace_is_dropped_not_recorded() {
        let toks = lex("a   b\tc").unwrap();
        assert_eq!(toks.len(), 3);
        assert!(toks[0].leading_trivia.is_empty());
        assert!(toks[1].leading_trivia.is_empty());
        assert!(toks[2].leading_trivia.is_empty());
    }

    #[test]
    fn line_comments_become_line_comment_trivia_and_strip_trailing_whitespace() {
        let src = "// header  \na\n// trailing  \nb";
        let toks = lex(src).unwrap();
        assert_eq!(toks.len(), 2);
        // First token: the header comment + a Newline.
        // The comment's span covers the whole source region (`// header  `),
        // including trailing whitespace, even though `text` is canonicalised.
        let header_start = 0;
        let header_end = src.find('\n').unwrap();
        assert_eq!(
            toks[0].leading_trivia,
            vec![
                Trivia::LineComment {
                    text: " header".to_owned(),
                    span: Span::new(header_start as u32, header_end as u32),
                },
                Trivia::Newline,
            ]
        );
        // Second token: a newline (after `a`), the trailing comment,
        // then a newline. Trailing horizontal whitespace inside each
        // comment body is stripped from `text`; `span` keeps it.
        let trailing_start = src.find("// trailing").unwrap();
        let trailing_end = src[trailing_start..].find('\n').unwrap() + trailing_start;
        assert_eq!(
            toks[1].leading_trivia,
            vec![
                Trivia::Newline,
                Trivia::LineComment {
                    text: " trailing".to_owned(),
                    span: Span::new(trailing_start as u32, trailing_end as u32),
                },
                Trivia::Newline,
            ]
        );
    }

    #[test]
    fn trailing_comment_on_same_line_is_leading_of_next_token() {
        // `fn` then a same-line trailing `// note`, then a newline,
        // then `foo`. The trivia model has no `trailing_trivia` —
        // the comment becomes the next token's leading trivia, with
        // no `Newline` between the previous token and the comment.
        let src = "fn // note\nfoo";
        let toks = lex(src).unwrap();
        assert_eq!(toks.len(), 2);
        assert!(toks[0].leading_trivia.is_empty());
        let note_start = src.find("// note").unwrap();
        let note_end = src.find('\n').unwrap();
        assert_eq!(
            toks[1].leading_trivia,
            vec![
                Trivia::LineComment {
                    text: " note".to_owned(),
                    span: Span::new(note_start as u32, note_end as u32),
                },
                Trivia::Newline,
            ]
        );
    }

    #[test]
    fn crlf_normalizes_to_one_newline() {
        let toks = lex("a\r\nb").unwrap();
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].leading_trivia, vec![Trivia::Newline]);
    }

    #[test]
    fn line_comment_keeps_internal_whitespace() {
        // Only *trailing* whitespace is stripped; internal spaces stay.
        let src = "// hello  world  \nx";
        let toks = lex(src).unwrap();
        assert_eq!(toks.len(), 1);
        let comment_end = src.find('\n').unwrap();
        assert_eq!(
            toks[0].leading_trivia,
            vec![
                Trivia::LineComment {
                    text: " hello  world".to_owned(),
                    span: Span::new(0, comment_end as u32),
                },
                Trivia::Newline,
            ]
        );
    }
}
