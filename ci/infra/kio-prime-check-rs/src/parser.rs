//! Recursive-descent parser implementing the Kio' grammar from
//! `specs/prime.md` § Grammar.
//!
//! Returns `Ok(())` if the source is syntactically Kio'. Any
//! deviation — surface sugar (trailing blocks, compact nominal groups,
//! macro declarations and tagged macro imports, n-ary tuple literals at
//! the value level), type-expression naming-rule violations, or any
//! other parse failure — is reported as `ParseError`.
//!
//! Postfix square brackets supply explicit type arguments; parentheses
//! supply one value packet. Each square pair contains exactly one type;
//! type-constructor applications and literal annotations use the same delimiter.
//!
//! Naming-convention enforcement is syntax-local: declaration heads,
//! binders, type-expression paths, selective imports, and value paths are
//! checked against their exact admitted name roles. Value-packet operands
//! additionally reject type-shaped paths.
//! Reserved compiler-internal type spellings remain admissible in phase
//! artifacts, but reservation supplies hygiene rather than privilege.

use crate::lexer::{LexError, Span, Token, TokenKind, lex};
use std::fmt;

#[derive(Debug)]
pub struct ParseError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        Self {
            offset: e.offset,
            message: e.message,
        }
    }
}

pub fn parse_module(src: &str) -> Result<(), ParseError> {
    let tokens = lex(src)?;
    // Keep the trivia offsets until the parser establishes whether the
    // contextual `import` token introduces an import or names an ordinary item.
    let mut import_doc_comments = Vec::new();
    for w in tokens.windows(2) {
        if matches!(w[0].kind, TokenKind::DocLine)
            && matches!(&w[1].kind, TokenKind::Ident(name) if name == "import")
        {
            import_doc_comments.push((w[1].span.start, w[0].span.start));
        }
    }
    let tokens: Vec<Token> = tokens
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::DocLine))
        .collect();
    let mut p = Parser {
        src_len: src.len(),
        tokens,
        pos: 0,
        current_residual: None,
        import_doc_comments,
    };
    p.module()?;
    p.expect_eof()?;
    Ok(())
}

struct Parser {
    src_len: usize,
    /// The unconsumed suffix of the token at `pos` after a contextual
    /// structural peel. The lexed token vector stays immutable, so a parser
    /// checkpoint is only `(pos, current_residual)`; restoring it also restores
    /// the current structural token suffix.
    current_residual: Option<Token>,
    tokens: Vec<Token>,
    pos: usize,
    import_doc_comments: Vec<(usize, usize)>,
}

/// Length of the leading `sep`-run of a `SymbolRun` when it is an
/// admissible type-chain separator: the whole run is `sep`
/// (`&` / `&&` / `&&&` / …, which collapse to one separator), or the
/// `sep`-run is immediately followed by `.` (a `sep`-prefixed run such
/// as `&.` whose `.` continues the next type atom). Returns `None`
/// otherwise. Mirrors the kio-rs parser's `chain_sep_prefix_len`.
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

impl Parser {
    fn peek_kind(&self) -> Option<&TokenKind> {
        self.current_residual
            .as_ref()
            .or_else(|| self.tokens.get(self.pos))
            .map(|t| &t.kind)
    }

    fn peek_kind_at(&self, offset: usize) -> Option<&TokenKind> {
        if offset == 0 {
            return self.peek_kind();
        }
        self.tokens.get(self.pos + offset).map(|t| &t.kind)
    }

    fn peek_token_at(&self, offset: usize) -> Option<&Token> {
        if offset == 0 {
            return self
                .current_residual
                .as_ref()
                .or_else(|| self.tokens.get(self.pos));
        }
        self.tokens.get(self.pos + offset)
    }

    fn advance(&mut self) -> Option<Token> {
        let t = self
            .current_residual
            .take()
            .or_else(|| self.tokens.get(self.pos).cloned());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn current_offset(&self) -> usize {
        self.current_residual
            .as_ref()
            .or_else(|| self.tokens.get(self.pos))
            .map(|t| t.span.start)
            .unwrap_or(self.src_len)
    }

    fn previous_span(&self) -> Span {
        self.tokens
            .get(self.pos.saturating_sub(1))
            .map(|t| t.span)
            .unwrap_or(Span {
                start: self.src_len,
                end: self.src_len,
            })
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            offset: self.current_offset(),
            message: msg.into(),
        })
    }

    fn err_at<T>(&self, span: Span, msg: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            offset: span.start,
            message: msg.into(),
        })
    }

    fn ident_named(&self, name: &str) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::Ident(s)) if s == name)
    }

    fn ident_named_at(&self, offset: usize, name: &str) -> bool {
        matches!(self.peek_kind_at(offset), Some(TokenKind::Ident(s)) if s == name)
    }

    /// True when a leading `host` ident introduces a `host type` /
    /// `host fn` declaration — i.e. the next token after `host` is
    /// `type` / `fn`, or `pub` followed by `type` / `fn`. Mirrors
    /// `kio-rs`'s `host_decl_follows`. Anywhere else `host` is an
    /// ordinary identifier.
    fn host_decl_follows(&self) -> bool {
        if self.ident_named_at(1, "type") || self.ident_named_at(1, "fn") {
            return true;
        }
        self.ident_named_at(1, "pub")
            && (self.ident_named_at(2, "type") || self.ident_named_at(2, "fn"))
    }

    fn let_statement_starts_here(&self) -> bool {
        if !self.ident_named("let") {
            return false;
        }
        if !matches!(self.peek_kind_at(1), Some(TokenKind::Ident(_))) {
            return false;
        }
        matches!(self.peek_kind_at(2), Some(k) if k.is_sym("="))
    }

    /// Consume zero or more consecutive comma tokens. Returns `true`
    /// if at least one comma was advanced past. Comma-separated
    /// productions accept any number of commas in any position —
    /// leading, repeated between items, and trailing — all collapsing
    /// to the same AST as the canonical one-comma-per-separator
    /// layout. Mirrors the `Parser::skip_commas` helper in
    /// `kio-rs/src/parser.rs`. See specs/prime.md § Grammar.
    fn skip_commas(&mut self) -> bool {
        let mut any = false;
        while matches!(self.peek_kind(), Some(TokenKind::Comma)) {
            self.advance();
            any = true;
        }
        any
    }

    /// Consume zero or more consecutive semicolons. Returns `true` if
    /// at least one was advanced past. Block bodies admit `;` runs at
    /// every position — leading, repeated between statements, and
    /// trailing after the final expression (`specs/grammar.md`
    /// § Productions (Kio') — `BlockBody ::= ';'* (Stmt ';'*)* Expr
    /// ';'*`). Mirrors the kio-rs parser's `skip_semicolon_separators`.
    fn skip_semicolons(&mut self) -> bool {
        let mut any = false;
        while matches!(self.peek_kind(), Some(TokenKind::Semicolon)) {
            self.advance();
            any = true;
        }
        any
    }

    /// Type contexts peel one structural bracket from a maximal symbol run.
    fn peek_starts_type_bracket(&self) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::SymbolRun(run)) if run.starts_with('['))
    }

    fn expect_type_bracket_open(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::SymbolRun(run)) if run == "[" => {
                self.advance();
                Ok(())
            }
            Some(TokenKind::SymbolRun(run)) if run.starts_with('[') => {
                self.split_current_sym(1);
                Ok(())
            }
            _ => self.err("expected `[` opening a type bracket"),
        }
    }

    fn expect_type_bracket_close(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::SymbolRun(run)) if run == "]" => {
                self.advance();
                Ok(())
            }
            Some(TokenKind::SymbolRun(run)) if run.starts_with(']') => {
                self.split_current_sym(1);
                Ok(())
            }
            _ => self.err("expected `]` after one type or binder"),
        }
    }

    fn lookahead_is_empty_or_value_group(&self) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::LParen))
            && !matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::SymbolRun(run)) if run.starts_with('[')
            )
    }

    /// Each square pair binds one name; leading stars encode its kind.
    fn consume_type_param_binder_group(&mut self, what: &str) -> Result<(), ParseError> {
        self.expect_type_bracket_open()?;
        self.consume_leading_stars();
        let (name, span) = self.expect_ident_with_span(what)?;
        Self::validate_type_name(&name, span)?;
        self.expect_type_bracket_close()
    }

    /// Consume one StrLit token and any immediately-following adjacent
    /// StrLits. Adjacent string literals fold into a single AST literal
    /// per specs/language.md § Literals: the chunking present in the
    /// source is informational only.
    fn skip_adjacent_str_lits(&mut self) {
        while matches!(self.peek_kind(), Some(TokenKind::StrLit)) {
            self.advance();
        }
    }

    fn eat_ident_named(&mut self, name: &str) -> bool {
        if self.ident_named(name) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Eat an optional `pub`, including a `pub(<module-path>)` scope
    /// restriction. The prime-checker validates Kio' shape, not
    /// visibility, so the scope is parsed and discarded like the bare
    /// `pub`.
    fn eat_vis(&mut self) -> Result<(), ParseError> {
        if self.eat_ident_named("pub") && matches!(self.peek_kind(), Some(TokenKind::LParen)) {
            self.expect_kind(&TokenKind::LParen, "`(` opening `pub(...)` scope")?;
            self.module_path()?;
            self.expect_kind(&TokenKind::RParen, "`)` closing `pub(...)` scope")?;
        }
        Ok(())
    }

    fn decl_modifiers(&mut self) -> Result<(bool, bool, Option<Span>), ParseError> {
        let mut saw_vis = false;
        let mut saw_pure = false;
        let mut scoped_vis = None;
        loop {
            if self.ident_named("pub") {
                let span = self.tokens[self.pos].span;
                if saw_vis {
                    return self.err_at(span, "duplicate `pub` modifier");
                }
                saw_vis = true;
                if matches!(self.peek_kind_at(1), Some(TokenKind::LParen)) {
                    scoped_vis = Some(span);
                }
                self.eat_vis()?;
            } else if self.ident_named("pure") {
                let span = self.tokens[self.pos].span;
                self.advance();
                if saw_pure {
                    return self.err_at(span, "duplicate `pure` modifier");
                }
                saw_pure = true;
            } else {
                break;
            }
        }
        Ok((saw_vis, saw_pure, scoped_vis))
    }

    fn expect_ident_named(&mut self, name: &str) -> Result<(), ParseError> {
        if self.eat_ident_named(name) {
            Ok(())
        } else {
            self.err(format!("expected `{name}`"))
        }
    }

    fn expect_kind(&mut self, want: &TokenKind, what: &str) -> Result<(), ParseError> {
        if self.peek_kind() == Some(want) {
            self.advance();
            Ok(())
        } else {
            self.err(format!("expected {what}"))
        }
    }

    /// Consume the [`TokenKind::SymbolRun`] spelled exactly `s`; on
    /// mismatch, error with `expected <what>`.
    fn expect_sym(&mut self, s: &str, what: &str) -> Result<(), ParseError> {
        self.expect_kind(&TokenKind::sym(s), what)
    }

    /// True iff the next token is the [`TokenKind::SymbolRun`]
    /// spelled exactly `s`.
    fn at_sym(&self, s: &str) -> bool {
        matches!(self.peek_kind(), Some(k) if k.is_sym(s))
    }

    /// Peel the leading `n` bytes off the current [`TokenKind::SymbolRun`],
    /// consuming that prefix. If the run is exactly `n` bytes (or the
    /// current token is not a `SymbolRun`), advance past it entirely;
    /// otherwise replace the current token with the residual suffix so
    /// the next consumer reads it. Operator runs are ASCII, so byte
    /// and char indices coincide. Mirrors kio-rs's
    /// `SkeletonCursor::split_current_sym`.
    fn split_current_sym(&mut self, n: usize) {
        let residual = match self
            .current_residual
            .as_ref()
            .or_else(|| self.tokens.get(self.pos))
        {
            Some(Token {
                kind: TokenKind::SymbolRun(run),
                span,
            }) if n < run.len() => Some((
                run[n..].to_owned(),
                Span {
                    start: span.start + n,
                    end: span.end,
                },
            )),
            _ => None,
        };
        match residual {
            Some((suffix, span)) => {
                self.current_residual = Some(Token {
                    kind: TokenKind::SymbolRun(suffix),
                    span,
                });
            }
            None => {
                self.advance();
            }
        }
    }

    /// True iff the current token is a `SymbolRun` beginning with
    /// `->` — a standalone arrow or one greedy fusion has absorbed
    /// into a longer run (`->!`, `->.`, …).
    fn at_arrow(&self) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::SymbolRun(s)) if s.starts_with("->"))
    }

    /// Consume a `->` at the current position, peeling it off the
    /// front of a fused run when greedy fusion has absorbed it.
    /// Returns `true` if an arrow was consumed. Mirrors kio-rs's
    /// `expect_fn_arrow`.
    fn eat_arrow(&mut self) -> bool {
        match self.peek_kind() {
            Some(TokenKind::SymbolRun(s)) if s == "->" => {
                self.advance();
                true
            }
            Some(TokenKind::SymbolRun(s)) if s.starts_with("->") => {
                self.split_current_sym(2);
                true
            }
            _ => false,
        }
    }

    /// Consume a required `->`, peeling from a fused run as
    /// [`Self::eat_arrow`] does; error with `expected <what>` otherwise.
    fn expect_arrow(&mut self, what: &str) -> Result<(), ParseError> {
        if self.eat_arrow() {
            Ok(())
        } else {
            self.err(format!("expected {what}"))
        }
    }

    /// Consume the `>` closing an existential binder, peeling it off
    /// the front of a fused run (`>:`, `><`, …) when greedy fusion has
    /// absorbed it. Mirrors kio-rs's `expect_existential_close`.
    fn expect_existential_close(&mut self, what: &str) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::SymbolRun(s)) if s == ">" => {
                self.advance();
                Ok(())
            }
            Some(TokenKind::SymbolRun(s)) if s.starts_with('>') => {
                self.split_current_sym(1);
                Ok(())
            }
            _ => self.err(format!("expected {what}")),
        }
    }

    /// Consume a leading run of `*` kind-marker stars — one greedy
    /// `SymbolRun` of all `*` (`*` / `**` / `***`) — and return the
    /// star count (0 when the next token is not an all-stars run).
    /// The verifier only checks acceptance, so callers discard the
    /// count. Mirrors kio-rs's `consume_leading_stars`.
    fn consume_leading_stars(&mut self) -> usize {
        match self.peek_kind() {
            Some(TokenKind::SymbolRun(run)) if !run.is_empty() && run.chars().all(|c| c == '*') => {
                let n = run.chars().count();
                self.advance();
                n
            }
            _ => 0,
        }
    }

    /// True iff the current token opens a type atom that begins with a
    /// `.` unit — a standalone `.` or a `.` fused with the start of the
    /// following construct (`.->`, `.&`, `.|`). Mirrors kio-rs's
    /// `at_unit_type_dot`.
    fn at_unit_type_dot(&self) -> bool {
        matches!(
            self.peek_kind(),
            Some(TokenKind::SymbolRun(run))
                if run == "."
                    || run.starts_with(".->")
                    || run.starts_with(".&")
                    || run.starts_with(".|")
        )
    }

    /// True iff the current token is an admissible `sep`-chain
    /// separator: a `SymbolRun` whose `sep` prefix collapses to one
    /// separator (`&`, `&&`, …, or a `sep`-run followed by `.`).
    fn is_chain_sep(&self, sep: char) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::SymbolRun(s)) if chain_sep_prefix_len(s, sep).is_some())
    }

    /// True iff the current token is an admissible `&`-product chain
    /// separator.
    fn at_amp_chain_sep(&self) -> bool {
        self.is_chain_sep('&')
    }

    /// True iff the current token is an admissible `|`-sum chain
    /// separator.
    fn at_pipe_chain_sep(&self) -> bool {
        self.is_chain_sep('|')
    }

    /// Consume a run of `sep`-chain separators. A separator may be a
    /// standalone `sep`, a fused all-`sep` run (`&&&`), or several
    /// spaced `sep` tokens; a `sep`-prefixed run whose remainder starts
    /// with `.` (`&.`) has the `.` residual left as the next atom.
    /// Returns `true` if at least one separator was consumed. Mirrors
    /// the kio-rs parser's chain-separator collapse.
    fn skip_chain_seps(&mut self, sep: char) -> bool {
        let mut any = false;
        while let Some(TokenKind::SymbolRun(run)) = self.peek_kind() {
            match chain_sep_prefix_len(run, sep) {
                Some(len) if len == run.chars().count() => {
                    self.advance();
                    any = true;
                }
                Some(len) => {
                    self.split_current_sym(len);
                    any = true;
                    break;
                }
                None => break,
            }
        }
        any
    }

    fn expect_ident_with_span(&mut self, what: &str) -> Result<(String, Span), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::Ident(_)) => {
                if let Some(Token {
                    kind: TokenKind::Ident(s),
                    span,
                }) = self.advance()
                {
                    Ok((s, span))
                } else {
                    unreachable!()
                }
            }
            _ => self.err(format!("expected {what}")),
        }
    }

    fn validate_type_name(name: &str, span: Span) -> Result<(), ParseError> {
        if name.starts_with("__") {
            return Err(ParseError {
                offset: span.start,
                message: format!(
                    "user identifier `{name}` may not begin with `__` (reserved for compiler-generated names)"
                ),
            });
        }
        if Self::is_type_name(name) {
            Ok(())
        } else {
            Err(ParseError {
                offset: span.start,
                message: format!(
                    "type name `{name}` must match _?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*"
                ),
            })
        }
    }

    fn is_type_name(name: &str) -> bool {
        !name.starts_with("__") && Self::has_name_shape(name, true)
    }

    fn has_name_shape(name: &str, type_name: bool) -> bool {
        let body = name.trim_matches('_');
        if body.is_empty() {
            return false;
        }
        body.split('_').enumerate().all(|(index, word)| {
            let Some((first, rest)) = word.as_bytes().split_first() else {
                return false;
            };
            if if index == 0 && type_name {
                !first.is_ascii_uppercase()
            } else {
                !first.is_ascii_lowercase()
            } {
                return false;
            }
            let letters = rest
                .iter()
                .take_while(|byte| byte.is_ascii_lowercase())
                .count();
            rest[letters..].iter().all(u8::is_ascii_digit)
        })
    }

    fn starts_like_type_name(name: &str) -> bool {
        name.trim_start_matches('_')
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
    }

    fn is_value_name(name: &str) -> bool {
        !name.starts_with("__") && Self::has_name_shape(name, false)
    }

    fn validate_value_name(name: &str, span: Span) -> Result<(), ParseError> {
        if name.starts_with("__") {
            return Err(ParseError {
                offset: span.start,
                message: format!(
                    "user identifier `{name}` may not begin with `__` (reserved for compiler-generated names)"
                ),
            });
        }
        if Self::is_value_name(name) {
            Ok(())
        } else {
            Err(ParseError {
                offset: span.start,
                message: format!(
                    "value name `{name}` must match _?[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*"
                ),
            })
        }
    }

    fn validate_value_binder_name(name: &str, span: Span) -> Result<(), ParseError> {
        if name == "_" {
            Ok(())
        } else {
            Self::validate_value_name(name, span)
        }
    }

    fn is_compiler_reserved_name(name: &str) -> bool {
        name.starts_with("__")
            && (Self::has_name_shape(name, false) || Self::has_name_shape(name, true))
    }

    fn validate_value_or_type_name(name: &str, span: Span) -> Result<(), ParseError> {
        if Self::is_value_name(name) || Self::is_type_name(name) {
            return Ok(());
        }
        if Self::starts_like_type_name(name) {
            Self::validate_type_name(name, span)
        } else {
            Self::validate_value_name(name, span)
        }
    }

    fn validate_path_segment_name(name: &str, span: Span) -> Result<(), ParseError> {
        if Self::is_compiler_reserved_name(name) {
            Ok(())
        } else {
            Self::validate_value_or_type_name(name, span)
        }
    }

    fn validate_type_path_name(name: &str, span: Span) -> Result<(), ParseError> {
        if Self::has_name_shape(name, true) {
            return Ok(());
        }
        Self::validate_type_name(name, span)
    }

    fn expect_eof(&mut self) -> Result<(), ParseError> {
        if self.pos == self.tokens.len() {
            Ok(())
        } else {
            self.err("expected end of file")
        }
    }

    /// `ModuleFile ::= DocBlock? ModuleDecl Import* Item*`
    ///
    /// Host items are ordinary module `Item`s (`host type` / `host fn`),
    /// not a special block — see `specs/prime.md` § Host declarations.
    /// There is no `env` block in Kio'.
    fn module(&mut self) -> Result<(), ParseError> {
        self.expect_ident_named("mod").map_err(|_| ParseError {
            offset: self.current_offset(),
            message: "expected `mod` declaration at the start of the file".into(),
        })?;
        self.module_path()?;
        self.expect_kind(&TokenKind::Semicolon, "`;` after `mod` path")?;

        while self.ident_named("import") {
            self.import_stmt()?;
        }

        while self.peek_kind().is_some() {
            self.item()?;
        }

        Ok(())
    }

    /// `ModulePath ::= IDENT ('/' IDENT)*`
    fn module_path(&mut self) -> Result<(), ParseError> {
        let (name, span) = self.expect_ident_with_span("identifier")?;
        Self::validate_value_name(&name, span)?;
        while self.at_sym("/") {
            self.advance();
            let (name, span) = self.expect_ident_with_span("identifier after `/`")?;
            Self::validate_value_name(&name, span)?;
        }
        Ok(())
    }

    /// `Import ::= ImportIntrinsics | ImportSelective | ImportQualified`
    fn import_stmt(&mut self) -> Result<(), ParseError> {
        if let Ok(index) = self
            .import_doc_comments
            .binary_search_by_key(&self.current_offset(), |(offset, _)| *offset)
        {
            return Err(ParseError {
                offset: self.import_doc_comments[index].1,
                message: "doc-comment (`///`) attached to nothing — `import` clauses are not \
                          documented; move the doc-comment to the following definition"
                    .into(),
            });
        }
        self.expect_ident_named("import")?;

        if self.ident_named("__intrinsics__") {
            self.advance();
            self.expect_kind(&TokenKind::Semicolon, "`;` after `import __intrinsics__`")?;
            return Ok(());
        }
        if self.ident_named("__comptime__") {
            return self.err("`import __comptime__` is compile-time surface syntax — not in Kio'");
        }
        if self.ident_named("__internal__") {
            return self.err("`__internal__` is not admitted in Kio' imports");
        }

        self.module_path()?;
        if self.eat_ident_named("as") {
            let (alias, span) = self.expect_ident_with_span("alias after `as`")?;
            Self::validate_value_name(&alias, span)?;
        } else {
            self.expect_kind(&TokenKind::LBrace, "`{` after the import provider")?;
            while matches!(self.peek_kind(), Some(TokenKind::Semicolon)) {
                self.advance();
            }
            if matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
                return self.err("expected at least one imported item");
            }
            loop {
                let kind = if self.eat_ident_named("fn") {
                    false
                } else if self.eat_ident_named("type") {
                    true
                } else if self.ident_named("op")
                    || self.ident_named("op_macro")
                    || self.ident_named("op_var")
                {
                    return self.err("operator imports are surface syntax — not in Kio'");
                } else if self.ident_named("lit") || self.ident_named("macro") {
                    return self.err("literal and macro imports are surface syntax — not in Kio'");
                } else {
                    return self.err("expected import kind `fn` or `type`");
                };
                let (name, span) = self.expect_ident_with_span("imported identifier")?;
                if kind {
                    Self::validate_type_name(&name, span)?;
                } else {
                    Self::validate_value_name(&name, span)?;
                }
                if self.eat_ident_named("as") {
                    let (alias, span) = self.expect_ident_with_span("alias after `as`")?;
                    if kind {
                        Self::validate_type_name(&alias, span)?;
                    } else {
                        Self::validate_value_name(&alias, span)?;
                    }
                }
                if matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
                    break;
                }
                self.expect_kind(&TokenKind::Semicolon, "`;` or `}` in the import list")?;
                while matches!(self.peek_kind(), Some(TokenKind::Semicolon)) {
                    self.advance();
                }
                if matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
                    break;
                }
            }
            self.expect_kind(&TokenKind::RBrace, "`}` closing the import list")?;
            self.optional_outer_semicolon();
            return Ok(());
        }
        self.expect_kind(&TokenKind::Semicolon, "`;` after the import")?;
        Ok(())
    }

    /// `Item ::= FnDef | TypeAlias | Newtype | RecursiveNewtype |
    ///           TypeRecGroup | HostType | HostFn`
    ///
    /// `host` is a contextual keyword: it introduces an opaque host
    /// declaration only when followed by `type` / `fn` (optionally with
    /// a redundant `pub` in between, accepting `host pub type`). Host
    /// items are always public, so a leading `pub` before `host` is
    /// admitted but redundant. In any other position `host` is an
    /// ordinary identifier. See `kio-rs`'s `item_modifiers` /
    /// `host_decl_follows`.
    fn item(&mut self) -> Result<(), ParseError> {
        let (vis, pure, scoped_vis) = self.decl_modifiers()?;
        if self.ident_named("host") && self.host_decl_follows() {
            if let Some(span) = scoped_vis {
                return self.err_at(span, "host declarations cannot use `pub(path)` visibility");
            }
            if pure {
                let host_fn = self.ident_named_at(1, "fn")
                    || (self.ident_named_at(1, "pub") && self.ident_named_at(2, "fn"));
                return if host_fn {
                    self.err(
                        "a host fn cannot carry `pure` because `pure` means that a function does not call host functions",
                    )
                } else {
                    self.err("`pure` is valid only on ordinary `fn` declarations")
                };
            }
            self.advance();
            let _ = self.eat_ident_named("pub");
            return self.host_item_after_keyword();
        }

        if self.ident_named("rec") {
            if pure {
                return self.err("`pure` is valid only on ordinary `fn` declarations");
            }
            self.advance();
            if self.eat_ident_named("type") {
                self.nominal_after_keyword()?;
                self.optional_outer_semicolon();
                Ok(())
            } else if matches!(self.peek_kind(), Some(TokenKind::LBrace)) {
                if vis {
                    return self.err(
                        "a bare `rec { ... }` type group has no leading visibility; put visibility on each member",
                    );
                }
                self.type_rec_group()?;
                self.optional_outer_semicolon();
                Ok(())
            } else {
                self.err("expected `type { ... }` or `{` after `rec` in Kio'")
            }
        } else if self.ident_named("fn") {
            self.advance();
            self.fn_def_after_keyword()?;
            self.optional_outer_semicolon();
            Ok(())
        } else if self.ident_named("type") {
            if pure {
                return self.err("`pure` is not valid on `type` declarations");
            }
            self.advance();
            if matches!(self.peek_kind(), Some(TokenKind::LBrace)) {
                self.nominal_after_keyword()?;
                self.optional_outer_semicolon();
                Ok(())
            } else {
                self.type_alias_after_keyword()
            }
        } else if self.ident_named("lit") {
            self.err("`lit` aliases are Kio surface sugar — not in Kio'")
        } else if self.ident_named("if") {
            self.err("`if`/`else` is surface sugar — not in Kio'")
        } else if self.ident_named("match") {
            self.err("`match` is surface sugar — not in Kio'")
        } else if self.ident_named("op") {
            self.err(
                "`op` declarations are Kio surface sugar for user-defined operators — not in Kio'",
            )
        } else if self.ident_named("op_var") {
            self.err("variadic operator declarations are Kio surface sugar — not in Kio'")
        } else if self.ident_named("macro") {
            self.err("`macro` declarations are not in Kio'")
        } else {
            self.err("expected `fn` or `type`")
        }
    }

    fn optional_outer_semicolon(&mut self) {
        if matches!(self.peek_kind(), Some(TokenKind::Semicolon)) {
            self.advance();
        }
    }

    fn type_rec_group(&mut self) -> Result<(), ParseError> {
        self.expect_kind(&TokenKind::LBrace, "`{` opening recursive type group")?;
        if matches!(self.peek_kind(), Some(TokenKind::Semicolon)) {
            self.advance();
        }
        let mut members = 0usize;
        while !matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
            let (_, pure, _) = self.decl_modifiers()?;
            if pure {
                return self.err("a bare `rec { ... }` type group admits only `type` declarations");
            }
            if self.eat_ident_named("type") {
                if matches!(self.peek_kind(), Some(TokenKind::LBrace)) {
                    self.nominal_after_keyword()?;
                } else {
                    self.type_alias_body()?;
                }
            } else if self.ident_named("rec") {
                return self.err("the enclosing `rec { ... }` already supplies recursive scope");
            } else {
                return self.err("expected `type` in bare `rec { ... }` type group");
            }
            members += 1;
            if matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
                break;
            }
            self.expect_kind(&TokenKind::Semicolon, "`;` between recursive type members")?;
        }
        self.expect_kind(&TokenKind::RBrace, "`}` closing recursive type group")?;
        if members < 2 {
            return self.err("one recursive data declaration uses a `rec` modifier, not a group");
        }
        Ok(())
    }

    /// After consuming `fn`: `IDENT SignatureGroups ('->' Type)? Block`
    ///
    /// The `-> Type` annotation is optional per
    /// `specs/language.md` § Function definitions: when omitted, the
    /// declaration return type defaults to `.`. Anonymous functions
    /// have separate optional parameter and return annotations.
    fn fn_def_after_keyword(&mut self) -> Result<(), ParseError> {
        let (name, span) = self.expect_ident_with_span("name after `fn`")?;
        Self::validate_value_name(&name, span)?;
        self.signature_groups(true, false)?;
        if self.eat_arrow() {
            self.type_()?;
        }
        self.block()?;
        Ok(())
    }

    /// After consuming `type`:
    /// `IDENT TypeParamList? '=' Type ';'`.
    fn type_alias_after_keyword(&mut self) -> Result<(), ParseError> {
        self.type_alias_body()?;
        self.expect_kind(&TokenKind::Semicolon, "`;` ending `type`")?;
        Ok(())
    }

    fn type_alias_body(&mut self) -> Result<(), ParseError> {
        let (name, span) = self.expect_ident_with_span("name after `type`")?;
        Self::validate_type_name(&name, span)?;
        if self.peek_starts_type_bracket() {
            self.type_param_list()?;
        }
        self.expect_sym("=", "`=` in `type`")?;
        self.type_()?;
        Ok(())
    }

    /// After consuming `host` (and any redundant `pub`): a host type
    /// or host function declaration. Both are opaque, signature-only,
    /// and `;`-terminated; they have no body to verify. The verifier
    /// only checks that the signature parses and that its type
    /// expressions are well-formed Kio' types.
    fn host_item_after_keyword(&mut self) -> Result<(), ParseError> {
        if self.eat_ident_named("type") {
            self.host_type_after_keyword()
        } else if self.eat_ident_named("fn") {
            self.host_fn_after_keyword()
        } else {
            self.err("expected `type` or `fn` after `host`")
        }
    }

    /// After consuming `host type`:
    /// `IDENT TypeParamList? RoleAnnotation? ';'`.
    ///
    /// A host type is an opaque nominal type the host supplies; the
    /// optional `role(...)` annotation pins its literal role.
    /// The role name comes from the grammar's closed vocabulary; whether its
    /// host type has admissible parameters remains a semantic check.
    fn host_type_after_keyword(&mut self) -> Result<(), ParseError> {
        let (name, span) = self.expect_ident_with_span("name after `host type`")?;
        Self::validate_type_name(&name, span)?;
        if self.peek_starts_type_bracket() {
            self.type_param_list()?;
        }
        if self.eat_ident_named("role") {
            self.expect_kind(&TokenKind::LParen, "`(` opening `role` annotation")?;
            let (role, span) = self.expect_ident_with_span("role name")?;
            if !matches!(
                role.as_str(),
                "i8" | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "f32"
                    | "f64"
                    | "bool"
                    | "str"
            ) {
                return self.err_at(span, format!("unknown `role` kind `{role}`"));
            }
            self.expect_kind(&TokenKind::RParen, "`)` closing `role` annotation")?;
        }
        self.expect_kind(&TokenKind::Semicolon, "`;` ending `host type`")?;
        Ok(())
    }

    /// After consuming `host fn`:
    /// `IDENT SignatureGroups ('->' Type)? ';'`.
    ///
    /// A host function is signature-only — the host supplies the body
    /// — so the declaration ends at `;` with no block. Value parameters
    /// carry the same named, annotated shape as ordinary function
    /// signatures (`name: Type`).
    fn host_fn_after_keyword(&mut self) -> Result<(), ParseError> {
        let (name, span) = self.expect_ident_with_span("name after `host fn`")?;
        Self::validate_value_name(&name, span)?;
        self.signature_groups(true, true)?;
        if self.at_arrow() {
            self.expect_arrow("`->` in `host fn`")?;
            self.type_()?;
        }
        self.expect_kind(&TokenKind::Semicolon, "`;` ending `host fn`")?;
        Ok(())
    }

    /// After `type`: one explicit nominal entry enclosed in braces.
    ///
    /// Existential binders trail the universal-parameter list as
    /// zero-or-more whitespace-separated `<X>` atoms before the `:`.
    /// The payload is an ordinary type expression — it must not begin
    /// with `(<…>, …)`, since standalone existential type expressions
    /// are no longer admissible.
    fn nominal_after_keyword(&mut self) -> Result<(), ParseError> {
        self.expect_kind(
            &TokenKind::LBrace,
            "`{` opening an explicit singleton nominal declaration",
        )?;
        self.optional_outer_semicolon();
        let (name, span) = self.expect_ident_with_span("nominal type name")?;
        Self::validate_type_name(&name, span)?;
        if self.peek_starts_type_bracket() {
            self.type_param_list()?;
        }
        while self.at_sym("<") {
            self.consume_angle_binder("existential binder name on nominal header")?;
        }
        self.expect_sym(":", "`:` before nominal payload")?;
        self.type_()?;
        self.newtype_body()?;
        self.optional_outer_semicolon();
        self.expect_kind(&TokenKind::RBrace, "`}` after the singleton nominal entry")?;
        Ok(())
    }

    /// Newtype members are separated by semicolon runs; edge runs are allowed.
    /// The two members form an unordered set: exactly one `mk`
    /// and exactly one `get`, in either order. Both are required;
    /// neither may be duplicated.
    fn newtype_body(&mut self) -> Result<(), ParseError> {
        self.expect_kind(
            &TokenKind::LBrace,
            "`{` opening the explicit `mk`/`get` body",
        )?;
        let mut saw_constructor = false;
        let mut saw_projector = false;
        self.skip_semicolons();
        while !matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
            self.eat_vis()?;
            if self.eat_ident_named("mk") {
                if saw_constructor {
                    return self.err("duplicate `mk` member in nominal body");
                }
                saw_constructor = true;
            } else if self.eat_ident_named("get") {
                if saw_projector {
                    return self.err("duplicate `get` member in nominal body");
                }
                saw_projector = true;
            } else {
                return self.err("expected `mk` or `get` in nominal body");
            }
            let separated = self.skip_semicolons();
            if !separated && !matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
                return self.err("expected `;` or `}` after nominal member");
            }
        }
        if !saw_constructor {
            return self.err("missing `mk` member in nominal body");
        }
        if !saw_projector {
            return self.err("missing `get` member in nominal body");
        }
        self.expect_kind(&TokenKind::RBrace, "`}` closing nominal body")?;
        Ok(())
    }

    /// `TypeParamList ::= UniversalBinder*`
    fn type_param_list(&mut self) -> Result<(), ParseError> {
        while self.peek_starts_type_bracket() {
            self.consume_type_param_binder_group("type-parameter name")?;
        }
        Ok(())
    }

    fn signature_groups(
        &mut self,
        annotated: bool,
        require_value_group: bool,
    ) -> Result<(), ParseError> {
        let mut saw_group = false;
        let mut saw_value_group = false;
        loop {
            if self.peek_starts_type_bracket() {
                saw_group = true;
                self.type_param_list()?;
            } else if matches!(self.peek_kind(), Some(TokenKind::LParen)) {
                saw_group = true;
                saw_value_group = true;
                self.value_signature_group(annotated)?;
            } else {
                break;
            }
        }
        if !saw_group {
            return self.err("expected a function parameter group");
        }
        if require_value_group && !saw_value_group {
            return self.err("callable signatures require an explicit value group `()`");
        }
        Ok(())
    }

    fn value_signature_group(&mut self, annotated: bool) -> Result<(), ParseError> {
        self.expect_kind(&TokenKind::LParen, "`(` opening value parameter group")?;
        self.skip_commas();
        while !matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            if annotated {
                self.signature_value_param()?;
            } else {
                self.fn_signature_value_param()?;
            }
            let saw_comma = self.skip_commas();
            if !saw_comma && !matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                return self.err("expected `,` or `)`");
            }
        }
        self.expect_kind(&TokenKind::RParen, "`)` closing value parameter group")?;
        Ok(())
    }

    fn signature_value_param(&mut self) -> Result<(), ParseError> {
        if self.peek_starts_type_bracket() {
            return self.err("type binders in signatures must be written before a value group");
        }
        let (name, span) = self.expect_ident_with_span("parameter name")?;
        Self::validate_value_binder_name(&name, span)?;
        self.expect_sym(":", "`:` between parameter name and type")?;
        self.type_()
    }

    fn fn_signature_value_param(&mut self) -> Result<(), ParseError> {
        if self.peek_starts_type_bracket() {
            return self.err("type binders in lambdas must be written before a value group");
        }
        let (name, span) = self.expect_ident_with_span("parameter name")?;
        Self::validate_value_binder_name(&name, span)?;
        if self.at_sym(":") {
            self.expect_sym(":", "`:` between parameter name and type")?;
            self.type_()?;
        }
        Ok(())
    }

    fn type_signature_prefix(&mut self) -> Result<(), ParseError> {
        while self.peek_starts_type_bracket() {
            self.type_param_list()?;
        }
        if self.at_arrow() {
            return self.err("type-binder run must be followed by a body type");
        }
        self.type_()
    }

    /// `Type ::= TypeAtom '->' TypeArrowResult | TypeChain`
    /// `TypeChain ::= TypeAtom (('&' TypeAtom)+ | ('|' TypeAtom)+)?`
    ///
    /// Bare same-operator chains (`A & B & C`, `A | B | C`) are
    /// admitted as parse-time associativity sugar: both `A & B & C`
    /// and `(A & (B & C))` produce the same binary AST, with no
    /// later phase able to distinguish them. Mixing `&` and `|`
    /// outside parens is a parse error — the user must write
    /// `(A & B) | C` or `A & (B | C)`. Mirrors the kio-rs parser's
    /// `type_expr` logic.
    fn type_(&mut self) -> Result<(), ParseError> {
        self.type_with_chain(true)
    }

    fn type_with_chain(&mut self, allow_chain: bool) -> Result<(), ParseError> {
        if self.peek_starts_type_bracket() {
            return self.type_signature_prefix();
        }

        // Optional leading-operator run (the multi-line layout).
        let leading_op: Option<char> = if self.at_amp_chain_sep() {
            self.skip_chain_seps('&');
            Some('&')
        } else if self.at_pipe_chain_sep() {
            self.skip_chain_seps('|');
            Some('|')
        } else {
            None
        };

        let mut used_chain = leading_op.is_some();
        if leading_op.is_some() && !self.peek_starts_type_atom() {
            if !allow_chain {
                return self
                    .err("product and sum types on the right of `->` must be parenthesized");
            }
            if self.at_amp_chain_sep() || self.at_pipe_chain_sep() {
                return self.err(
                    "mixing `&` and `|` requires explicit parentheses; \
                     e.g., `A & (B | C)` or `(A & B) | C`",
                );
            }
            if self.at_arrow() {
                return self.err("product and sum types on the left of `->` must be parenthesized");
            }
            return Ok(());
        }

        self.type_atom()?;

        // Determine chain operator: from leading run if seen, else
        // from the next token. No chain → return the atom.
        let chain_op: Option<char> = match leading_op {
            Some(op) => Some(op),
            None if self.at_amp_chain_sep() => Some('&'),
            None if self.at_pipe_chain_sep() => Some('|'),
            None => None,
        };

        if let Some(op) = chain_op {
            used_chain = true;
            loop {
                if self.is_chain_sep(op) {
                    self.skip_chain_seps(op);
                    // Trailing-op rule: a run of operators may
                    // absorb into the chain's terminator.
                    if !self.peek_starts_type_atom() {
                        break;
                    }
                    self.type_atom()?;
                } else if self.at_amp_chain_sep() || self.at_pipe_chain_sep() {
                    return self.err(
                        "mixing `&` and `|` requires explicit parentheses; \
                         e.g., `A & (B | C)` or `(A & B) | C`",
                    );
                } else {
                    break;
                }
            }
        }
        if used_chain && !allow_chain {
            return self.err("product and sum types on the right of `->` must be parenthesized");
        }
        if self.at_arrow() {
            if used_chain {
                return self.err("product and sum types on the left of `->` must be parenthesized");
            }
            self.eat_arrow();
            self.type_with_chain(false)?;
        }
        Ok(())
    }

    /// True iff the next token can begin a `type_atom`. Mirrors
    /// `kio-rs/src/parser.rs::peek_starts_type_atom`.
    fn peek_starts_type_atom(&self) -> bool {
        self.at_unit_type_dot()
            || self.at_sym("!")
            || self.peek_starts_type_bracket()
            || matches!(
                self.peek_kind(),
                Some(TokenKind::LParen) | Some(TokenKind::Ident(_))
            )
    }

    /// One atom in a type expression. Handles all paren-prefixed forms
    /// by one-token lookahead after `(`.
    fn type_atom(&mut self) -> Result<(), ParseError> {
        if self.at_unit_type_dot() {
            // Unit `.` — standalone, or a `.` greedy fusion joined to
            // the start of the following construct (`.->`, `.&`, `.|`);
            // peel the `.` and leave the residual for the next
            // production.
            match self.peek_kind() {
                Some(TokenKind::SymbolRun(run)) if run == "." => {
                    self.advance();
                }
                _ => self.split_current_sym(1),
            }
            return Ok(());
        }
        match self.peek_kind() {
            Some(k) if k.is_sym("!") => {
                self.advance();
                Ok(())
            }
            Some(TokenKind::LParen) => self.paren_type(),
            Some(TokenKind::Ident(_)) => self.type_path(),
            Some(_) if self.peek_starts_type_bracket() => self.type_signature_prefix(),
            _ => self.err("expected a type"),
        }
    }

    fn type_path(&mut self) -> Result<(), ParseError> {
        // `_` is the surface-level type-inference placeholder. Kio'
        // has no inference — every type position is explicit — so
        // `_` is not a valid Kio' type.
        if self.ident_named("_") {
            return self.err("`_` type placeholder is not in Kio'");
        }

        let first = self.expect_ident_with_span("type name")?;
        let mut qualifiers = vec![first];
        while self.at_sym("/") {
            self.advance();
            qualifiers.push(self.expect_ident_with_span("module path segment after `/`")?);
        }

        let (name, name_span) = if self.at_sym(".") {
            for (qualifier, span) in &qualifiers {
                Self::validate_value_name(qualifier, *span)?;
            }
            self.advance();
            self.expect_ident_with_span("type name after `.`")?
        } else {
            if qualifiers.len() > 1 {
                return self.err("qualified type paths use `module/path.TypeName`");
            }
            qualifiers
                .pop()
                .expect("a type path starts with one identifier")
        };

        Self::validate_type_path_name(&name, name_span)?;
        while self.peek_starts_type_bracket() {
            self.type_arg_list()?;
        }
        Ok(())
    }

    /// Disambiguates the `(` … `)`-prefixed type productions. Accepts
    /// grouping, product chains, and sum chains. Chain-operator runs
    /// collapse per the language-level rule.
    fn paren_type(&mut self) -> Result<(), ParseError> {
        self.expect_kind(&TokenKind::LParen, "`(`")?;

        // A leading operator pins the chain shape: `& A & B …` is a
        // product chain and `| A | B …` is a sum chain.
        let leading_chain_op: Option<char> = if self.at_amp_chain_sep() {
            self.skip_chain_seps('&');
            Some('&')
        } else if self.at_pipe_chain_sep() {
            self.skip_chain_seps('|');
            Some('|')
        } else if matches!(self.peek_kind(), Some(TokenKind::Comma)) {
            return self.err("commas are not product type syntax; write `&` between product types");
        } else {
            None
        };

        // Leading operator runs before `)` collapse to the same
        // zero-item chain identity. Without a leading chain operator,
        // parentheses are grouping and must contain a type.
        if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            self.advance();
            if leading_chain_op.is_none() {
                return self.err("expected type expression inside parentheses");
            }
            return Ok(());
        }

        // A parenthesized grouped function type is ordinary grouping
        // around the binder-prefix syntax.
        if self.peek_starts_type_bracket() {
            self.type_()?;
            self.expect_kind(&TokenKind::RParen, "`)` closing parenthesized type")?;
            if self.eat_arrow() {
                self.type_with_chain(false)?;
            }
            return Ok(());
        }

        // Leading `&` / `|` layouts stay in the explicit chain path.
        // Their elements are atoms, so a function-typed element must
        // be parenthesized. Otherwise a parenthesized item is a full
        // type expression, so `(A -> B)` works naturally.
        if leading_chain_op.is_some() {
            self.type_atom()?;
        } else {
            self.type_()?;
        }

        // Branch on the leading operator if we saw one — it pins the
        // chain shape. Otherwise dispatch on what follows the first
        // type.
        if let Some(op) = leading_chain_op {
            self.collect_type_chain_after_first(op)?;
            self.expect_kind(&TokenKind::RParen, "`)` closing parenthesised type")?;
            if self.eat_arrow() {
                self.type_with_chain(false)?;
            }
            return Ok(());
        }

        if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            self.advance();
            if self.eat_arrow() {
                self.type_with_chain(false)?;
            }
            return Ok(());
        }
        if matches!(self.peek_kind(), Some(TokenKind::Comma)) {
            return self.err("commas are not product type syntax; write `&` between product types");
        }
        if self.at_pipe_chain_sep() {
            self.collect_type_chain_after_first('|')?;
            self.expect_kind(&TokenKind::RParen, "`)` closing parenthesised type")?;
            if self.eat_arrow() {
                self.type_with_chain(false)?;
            }
            return Ok(());
        }
        if self.at_amp_chain_sep() {
            self.collect_type_chain_after_first('&')?;
            self.expect_kind(&TokenKind::RParen, "`)` closing parenthesised type")?;
            if self.eat_arrow() {
                self.type_with_chain(false)?;
            }
            return Ok(());
        }
        self.err("expected `)`, `|`, or `&` continuing the parenthesised type")
    }

    /// Consume one `<IDENT>` angle binder. Used by the existential
    /// binder run that trails a nominal declaration's universal-
    /// parameter list (e.g., `type { Box[A] <U>: A & U { mk; get; }; }`).
    fn consume_angle_binder(&mut self, what: &str) -> Result<(), ParseError> {
        self.expect_sym("<", "`<` opening existential binder")?;
        let (name, span) = self.expect_ident_with_span(what)?;
        Self::validate_type_name(&name, span)?;
        // The closing `>` may be fused with what follows it (`>:` before
        // the payload separator, `><` before the next binder); peel it.
        self.expect_existential_close("`>` closing existential binder")?;
        Ok(())
    }

    /// Continue an `&`- or `|`-chain after the first type has been
    /// parsed. Accepts any number of `op_kind` tokens between items
    /// and a trailing run before `)`. Mixing `&` and `|` in one chain
    /// is rejected (the parens-mixing rule from specs/language.md
    /// § Anonymous sum and product types).
    fn collect_type_chain_after_first(&mut self, sep: char) -> Result<(), ParseError> {
        loop {
            // After an item: zero or more `sep`s, then either an
            // item or the closer.
            if self.is_chain_sep(sep) {
                self.skip_chain_seps(sep);
            } else if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                break;
            } else if self.at_amp_chain_sep() || self.at_pipe_chain_sep() {
                return self.err("mixing `&` and `|` requires explicit parentheses");
            } else {
                return self.err(format!("expected `{sep}` or `)` in type chain"));
            }
            if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                break;
            }
            self.type_atom()?;
        }
        Ok(())
    }

    /// One explicit type argument, shared by named types and value stages.
    fn type_arg_list(&mut self) -> Result<(), ParseError> {
        self.expect_type_bracket_open()?;
        self.type_()?;
        self.expect_type_bracket_close()
    }

    /// `Block ::= '{' BlockBody '}'`
    /// `BlockBody ::= ';'* (Stmt ';'*)* Expr ';'*`
    /// `Stmt ::= 'let' IDENT '=' Expr ';' | Expr ';'`
    ///
    /// A block is a run of `;`-terminated statements followed by a
    /// mandatory final expression, with optional `;` runs at every
    /// position — leading, repeated between statements, and trailing
    /// after the final expression. A block with no final expression
    /// (empty, `;`-only, or ending in a statement) is a parse error.
    fn block(&mut self) -> Result<(), ParseError> {
        self.expect_kind(&TokenKind::LBrace, "`{` opening block")?;
        self.block_body()?;
        self.expect_kind(&TokenKind::RBrace, "`}` closing block")?;
        Ok(())
    }

    /// Body of a block: an optional leading `;` run, zero or more
    /// `;`-terminated statements (each followed by an optional `;`
    /// run), then a mandatory final expression with an optional
    /// trailing `;` run (`specs/grammar.md` § Productions (Kio') —
    /// `BlockBody ::= ';'* (Stmt ';'*)* Expr ';'*`). The caller has
    /// consumed the opening `{`; this returns just before the closing
    /// `}`. Mirrors the kio-rs parser's `block_body_continuation`.
    fn block_body(&mut self) -> Result<(), ParseError> {
        // Leading `;` run.
        self.skip_semicolons();
        loop {
            if matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
                return self.err(
                    "block has no final expression; a block must end with an expression \
                     (statements alone are not enough)",
                );
            }
            if self.let_statement_starts_here() {
                self.let_statement()?;
                // A statement may be followed by a run of `;`.
                self.skip_semicolons();
                continue;
            }
            // Otherwise: parse an expression, then consume any `;`
            // run that follows. If a `}` follows the run, the
            // expression was the block's final expression trailed by
            // `;` — return. If more follows, the expression was an
            // expression statement — continue. If no `;` follows at
            // all, the expression is the final expression.
            self.expr()?;
            if matches!(self.peek_kind(), Some(TokenKind::Semicolon)) {
                self.skip_semicolons();
                if matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
                    return Ok(());
                }
                continue;
            }
            return Ok(());
        }
    }

    /// `'let' IDENT '=' Expr ';'`
    fn let_statement(&mut self) -> Result<(), ParseError> {
        self.expect_ident_named("let")?;
        let (name, span) = self.expect_ident_with_span("name after `let`")?;
        Self::validate_value_binder_name(&name, span)?;
        self.expect_sym("=", "`=` in `let`")?;
        self.expr()?;
        if self.ident_named("in") {
            return self.err(
                "expected `;` after let binding; the `let X = E in body` expression form was \
                 removed in favor of block statements (`let X = E;` followed by more statements)",
            );
        }
        self.expect_kind(&TokenKind::Semicolon, "`;` after let binding")?;
        Ok(())
    }

    /// `Expr ::= ExprPostfix`
    ///
    /// `let` opens a statement only when the surrounding block parser
    /// has recognized a full let-statement shape; otherwise it is a
    /// value identifier like any other contextual word.
    fn expr(&mut self) -> Result<(), ParseError> {
        self.expr_postfix().map(|_| ())
    }

    /// `ExprPostfix ::= ValuePath CallSuffix* | ExprAtom CallSuffix*`
    ///
    /// Dotted segments belong only to an identifier-led value path and are
    /// consumed before any call suffix. They are not a generic expression
    /// suffix: `(f).g` and `f().g` are outside both Kio and Kio'.
    // Retain only the type-shaped head, if any, through grouping and call
    // suffixes. A value packet rejects that head; other expression contexts
    // leave namespace resolution to the consumer.
    fn expr_postfix(&mut self) -> Result<Option<Span>, ParseError> {
        let mut final_name = self.peek_token_at(0).and_then(|token| {
            if let TokenKind::Ident(name) = &token.kind {
                Some((name.clone(), token.span))
            } else {
                None
            }
        });
        let mut type_head = if matches!(self.peek_kind(), Some(TokenKind::LParen)) {
            self.paren_expr()?
        } else {
            self.expr_atom()?;
            None
        };
        if final_name.is_some() {
            while self.peek_kind().is_some_and(|kind| kind.is_sym(".")) {
                self.advance();
                let (name, span) = self.expect_ident_with_span("identifier after `.`")?;
                Self::validate_path_segment_name(&name, span)?;
                final_name = Some((name, span));
            }
            let (name, span) = final_name.expect("identifier-led expression has a final name");
            if Self::starts_like_type_name(&name) {
                type_head = Some(span);
            }
        }
        loop {
            match self.peek_kind() {
                Some(k) if k.is_sym(".") => {
                    return self.err(
                        "dotted value paths must start with an identifier and precede call suffixes",
                    );
                }
                Some(TokenKind::LParen) => {
                    self.call_args()?;
                }
                Some(_) if self.peek_starts_type_bracket() => self.type_arg_list()?,
                Some(k)
                    if k.is_sym(".>") || k.is_sym(".>>") || k.is_sym(".<") || k.is_sym(".<<") =>
                {
                    return self.err(
                        "dot-splice call syntax (`.>`, `.>>`, `.<`, `.<<`) is Kio-surface UFCS dispatch — not in Kio'",
                    );
                }
                _ => break,
            }
        }
        Ok(type_head)
    }

    /// A parenthesized suffix is one value packet; an empty packet is Unit.
    fn call_args(&mut self) -> Result<(), ParseError> {
        self.expect_kind(&TokenKind::LParen, "`(`")?;
        self.skip_commas();
        if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            self.advance();
            return Ok(());
        }
        self.value_operand()?;
        loop {
            let saw_comma = self.skip_commas();
            if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                break;
            }
            if !saw_comma {
                return self.err("expected `,` or `)`");
            }
            self.value_operand()?;
        }
        self.expect_kind(&TokenKind::RParen, "`)` closing call argument list")?;
        Ok(())
    }

    fn value_operand(&mut self) -> Result<(), ParseError> {
        if let Some(span) = self.expr_postfix()? {
            return self.err_at(span, "expected a value operand, found a type path");
        }
        Ok(())
    }

    fn expr_atom(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::IntLit | TokenKind::FloatLit) => {
                // A numeric literal. Kio' mandates the trailing
                // `[Type]` annotation (specs/grammar.md § Kio' grammar
                // — `LiteralCall ::= LITERAL '[' Type ']'`). The
                // `LiteralCall` annotation is consumed inline here so
                // a bare literal surfaces a Kio'-specific rejection
                // rather than reaching `expr_postfix` and looking
                // like a generic "expected expression" failure.
                self.advance();
                self.expect_literal_annotation("numeric")
            }
            Some(TokenKind::StrLit) => {
                // Adjacent string literals fold per the language-level
                // rule (specs/language.md § Literals): two or more
                // StrLit tokens with only trivia between them produce a
                // single AST literal with the concatenated value. The
                // trailing `[Type]` annotation is mandatory in Kio'.
                self.advance();
                self.skip_adjacent_str_lits();
                self.expect_literal_annotation("string")
            }
            Some(TokenKind::LParen) => self.paren_expr().map(|_| ()),
            Some(TokenKind::SymbolRun(run)) if run == "." || run.starts_with(".[") => {
                let dot_end = self.current_offset() + 1;
                if run == "." {
                    self.advance();
                } else {
                    // The maximal operator run for a compact polymorphic
                    // lambda starts `.[` (or `.[*`). Peel the lambda's
                    // structural dot and let the forall parser peel `[`.
                    self.split_current_sym(1);
                }
                match self.peek_kind() {
                    Some(TokenKind::Ident(s)) if s == "t" || s == "f" => {
                        if self.current_offset() != dot_end {
                            return self.err(
                                "a boolean literal requires `t` or `f` immediately after `.`",
                            );
                        }
                        self.advance();
                        self.expect_literal_annotation("boolean")
                    }
                    Some(TokenKind::LParen) if self.lookahead_is_empty_or_value_group() => {
                        self.fn_after_dot()
                    }
                    Some(TokenKind::LParen) => self.err("expected `.t`, `.f`, or `.(...) { ... }`"),
                    _ if self.peek_starts_type_bracket() => self.fn_after_dot(),
                    _ => self.err("expected `.t`, `.f`, or `.(...) { ... }`"),
                }
            }
            Some(TokenKind::Ident(s)) if s == "_" => {
                // `_` in expression position is surface sugar for elided
                // type-arg slots at a polymorphic call (`__left__[_][_](x)`).
                // Kio' has no inference, so `_` has no role here. The
                // type-position spelling is rejected in `type_atom` too.
                self.err("`_` placeholder is not in Kio'")
            }
            Some(TokenKind::Ident(_)) => {
                let (name, span) = self.expect_ident_with_span("identifier")?;
                Self::validate_path_segment_name(&name, span)
            }
            _ => self.err("expected an expression"),
        }
    }

    /// Consume the trailing `'[' Type ']'` annotation of a
    /// `LiteralCall`. Kio' makes the annotation mandatory (see
    /// `specs/grammar.md` § Kio' grammar — `LiteralCall ::= LITERAL
    /// '[' Type ']'`); a bare literal is rejected here so the user
    /// sees a Kio'-specific diagnostic rather than the generic
    /// "expected `,` or `)`" from the surrounding call-args parser.
    fn expect_literal_annotation(&mut self, kind: &str) -> Result<(), ParseError> {
        if !self.peek_starts_type_bracket() {
            return self.err(format!(
                "a {kind} literal must carry a `[Type]` annotation in Kio' \
                 (specs/grammar.md § Kio' grammar — `LiteralCall ::= LITERAL '[' Type ']'`); \
                 contextual literal inference does not apply"
            ));
        }
        self.type_arg_list()?;
        Ok(())
    }

    /// `(  )` (unit), `( Expr )` (grouping). Comma-runs around zero
    /// or one expression collapse to the same shapes; `(a, b, …)`
    /// n-ary tuple literals are surface sugar and rejected.
    fn paren_expr(&mut self) -> Result<Option<Span>, ParseError> {
        let open_span = self.tokens.get(self.pos).map(|t| t.span).unwrap_or(Span {
            start: self.src_len,
            end: self.src_len,
        });
        self.expect_kind(&TokenKind::LParen, "`(`")?;
        self.skip_commas();
        if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            self.advance();
            return Ok(None);
        }
        let mut item_count = 1usize;
        let type_head = self.expr_postfix()?;
        loop {
            let saw_comma = self.skip_commas();
            if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                break;
            }
            if !saw_comma {
                return self.err("expected `,` or `)`");
            }
            item_count += 1;
            self.expr()?;
        }
        self.expect_kind(&TokenKind::RParen, "`)` closing parenthesised expression")?;
        if item_count > 1 {
            return self.err_at(
                open_span,
                "n-ary tuple literal `(a, b, …)` is surface sugar — not in Kio' (use `__pair__` after `import __intrinsics__;`)",
            );
        }
        Ok(type_head)
    }

    /// After consuming `.`: `SignatureGroups ('->' Type)? Block`
    fn fn_after_dot(&mut self) -> Result<(), ParseError> {
        self.signature_groups(false, false)?;
        if self.eat_arrow() {
            self.type_()?;
        }
        self.block()?;
        // If we saw nothing usable, fall back to the previous span as the
        // error anchor — matches no current code path but keeps the helper
        // honest for future callers.
        let _ = self.previous_span();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_module;

    fn ok(src: &str) {
        if let Err(e) = parse_module(src) {
            panic!("expected ok, got error at {}: {}", e.offset, e.message);
        }
    }

    fn err(src: &str) -> String {
        match parse_module(src) {
            Ok(_) => panic!("expected parse error, got ok"),
            Err(e) => e.message,
        }
    }

    #[test]
    fn default_is_an_ordinary_name_not_a_declaration_modifier() {
        ok("mod m; import helpers { fn default as d; fn default } fn default() { () }");
        for source in [
            "mod m; default host type Int role(i32);",
            "mod m; default pub host type Int role(i32);",
            "mod m; default pub type Number = numbers.Int;",
            "mod m; default type Number = Identity(numbers.Int);",
            "mod m; rec { default type Number = Int; type { Box : Int { mk; get; }; } }",
            "mod m; pub default host type Int role(i32);",
            "mod m; default default host type Int role(i32);",
            "mod m; default host fn value();",
            "mod m; default fn value() {}",
            "mod m; default type { Int : . {}; }",
        ] {
            assert!(parse_module(source).is_err(), "{source}");
        }
    }

    #[test]
    fn host_roles_use_the_closed_grammar_vocabulary() {
        for role in [
            "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "f32", "f64",
            "bool", "str",
        ] {
            ok(&format!("mod x; host type T role({role});"));
        }
        assert!(err("mod x; host type T role(no_such_role);").contains("unknown `role`"));
    }

    #[test]
    fn host_declarations_reject_scoped_visibility() {
        for declaration in ["fn f() -> .;", "type T;"] {
            assert!(
                err(&format!("mod x; pub(x) host {declaration}"))
                    .contains("host declarations cannot use `pub(path)`")
            );
            for modifier in ["host", "pub host", "host pub"] {
                ok(&format!("mod x; {modifier} {declaration}"));
            }
        }
    }

    #[test]
    fn boolean_literals_require_adjacent_marker() {
        for literal in [". t", ".\nf", ".// keep\nt"] {
            assert!(
                err(&format!("mod x; fn f() {{ {literal}[Bool] }}"))
                    .contains("immediately after `.`")
            );
        }
        ok("mod x; fn f() { .t[Bool] } fn g() { .f[Bool] }");
    }

    #[test]
    fn numeric_separators_stay_between_digits() {
        for literal in ["1__0", "1_", "1.0__1", "1.0e1__0", "1e1_", "2e3", "2E-3"] {
            let _ = err(&format!("mod x; fn f() {{ {literal}[Number] }}"));
        }
        for literal in ["1_0", "1.0_1", "1.0e1_0", "2.0e3", "2.0E-3"] {
            ok(&format!("mod x; fn f() {{ {literal}[Number] }}"));
        }
    }

    #[test]
    fn empty_module_decl_only() {
        ok("mod foo;");
    }

    #[test]
    fn type_only_ordinary_functions_and_lambdas() {
        ok("mod example; \
            fn named[A][B] -> . { () } \
            pure fn default[A] { () } \
            fn returned() -> [A] . { .[A] { () } } \
            fn annotated() -> [A] . { .[A] -> . { () } } \
            fn explicit[A]() { () }");
        for source in [
            "mod example; fn constant -> . { () }",
            "mod example; host fn imported[A] -> .;",
            "mod example; fn invalid() { . { () } }",
        ] {
            err(source);
        }
        ok("mod example; host fn imported[A]() -> .;");
    }

    #[test]
    fn host_items_as_module_items() {
        ok("mod foo; \
             host type String role(str); host type Box[A]; \
             host fn read(x: Box[String]) -> String; \
             fn id(x: String) -> String { x }");
    }

    #[test]
    fn fn_def_id() {
        ok("mod m; fn id[A](x: A) -> A { x }");
    }

    #[test]
    fn pure_fn_modifier() {
        ok("mod m; pure fn id[A](x: A) -> A { x }");
    }

    #[test]
    fn pure_type_modifier_rejected() {
        let message = err("mod m; pure type Pair[A][B] = (A & B);");
        assert!(
            message.contains("`pure`") && message.contains("`type`"),
            "{message}"
        );
    }

    #[test]
    fn pure_newtype_modifier_rejected() {
        let message = err("mod m; pure type { Wrap[A] : A { pub mk; pub get; }; };");
        assert!(
            message.contains("`pure`") && message.contains("`type`"),
            "{message}"
        );
    }

    #[test]
    fn pure_and_visibility_modifiers_accept_either_order() {
        ok("mod m; pub pure fn f() -> . { () }");
        ok("mod m; pure pub fn f() -> . { () }");
        ok("mod m; pub(scope/path) pure fn f() -> . { () }");
        ok("mod m; pure pub(scope/path) fn f() -> . { () }");
    }

    #[test]
    fn duplicate_pure_modifier_rejected() {
        let message = err("mod m; pure pure fn f() -> . { () }");
        assert!(message.contains("duplicate `pure` modifier"), "{message}");
    }

    #[test]
    fn pure_host_and_macro_declarations_rejected() {
        let host_fn = err("mod m; pure host fn f() -> .;");
        assert!(
            host_fn.contains("`pure` means that a function does not call host functions"),
            "{host_fn}"
        );

        let host_type = err("mod m; pure host type T;");
        assert!(
            host_type.contains("`pure` is valid only on ordinary `fn` declarations"),
            "{host_type}"
        );

        let macro_decl = err("mod m; pure macro e: . -> . { impl helper }");
        assert!(macro_decl.contains("`macro`"), "{macro_decl}");
    }

    #[test]
    fn rejects_angle_universal_binder() {
        // Universals are `[A]`-shaped; `<A>` is reserved for
        // existentials in their paren-wrapped position.
        let m = err("mod m; fn id<A>(x: A) -> A { x }");
        assert!(
            m.contains("`[`")
                || m.contains("[type-param]")
                || m.contains("type-parameter")
                || m.contains("parameter name")
                || m.contains("parameter group"),
            "msg = {m}"
        );
    }

    #[test]
    fn type_alias_bracket_binder() {
        ok("mod m; type Pair[A][B] = (A & B);");
    }

    #[test]
    fn type_binder_name_class_matches_kio_parser() {
        for name in ["A", "_A", "_Foo_bar", "Foo123_bar4", "_Foo__"] {
            ok(&format!(
                "mod m; fn id[{name}](x: {name}) -> {name} {{ x }}"
            ));
        }
        for name in [
            "_ct_type_n0",
            "lowercase",
            "_foo",
            "_1Foo",
            "FooBar",
            "_FooBar",
            "_1",
            "Foo_123",
            "Foo__bar",
            "Foo1bar",
        ] {
            let message = err(&format!("mod m; fn id[{name}](x: .) -> . {{ x }}"));
            assert!(
                message.contains("must match _?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*"),
                "{message}"
            );
        }
        let reserved = err("mod m; fn id[__Type__](x: .) -> . { x }");
        assert!(reserved.contains("may not begin with `__`"), "{reserved}");
    }

    #[test]
    fn type_declaration_heads_enforce_type_name_contract() {
        ok("mod m; \
             host type Host; host type _Host; \
             type Alias = .; type _Alias = .; \
             type { Box : . { mk; get; }; }; \
             type { _Box : . { mk; get; }; };");

        for (name, source) in [
            ("_foo", "mod m; type _foo = .;"),
            ("_1Foo", "mod m; host type _1Foo;"),
            ("_FooBar", "mod m; type { _FooBar : . { mk; get; }; };"),
            ("__Foo", "mod m; type __Foo = .;"),
        ] {
            let message = err(source);
            assert!(
                message.contains("type name") || message.contains("may not begin with `__`"),
                "{name}: {message}"
            );
        }
    }

    #[test]
    fn type_expr_lowercase_path_rejected() {
        let msg = err("mod m; type T = string;");
        assert!(msg.contains("type name `string`"), "msg = {msg}");
    }

    #[test]
    fn type_expr_reserved_internal_path_accepted() {
        ok("mod m; type T = __Type__;");
    }

    #[test]
    fn type_expr_qualified_type_path_accepted() {
        ok("mod m; type T = pkg/types.Box[A];");
    }

    #[test]
    fn newtype_bracket_binder() {
        ok("mod m; type { Wrap[A] : A { pub mk; pub get; }; };");
    }

    #[test]
    fn function_type_bracket_binder() {
        ok("mod m; type Id = [A] A -> A;");
    }

    #[test]
    fn parenthesized_grouped_function_type() {
        ok("mod m; type Id = ([A] A -> A);");
        ok("mod m; type Pair = (& ([A] A -> A) & (. -> .));");
    }

    #[test]
    fn square_pairs_have_exactly_one_binder_or_type() {
        for source in [
            "mod m; type Id = [A, B] A -> B;",
            "mod m; fn pair[A, B](a: A) -> A { a }",
            "mod m; host type Pair[A, B];",
            "mod m; type { Pair[A, B] : A { mk; get; }; };",
            "mod m; type Pair[A, B] = A & B;",
            "mod m; fn run() { .[A, B](a: A) { a } }",
            "mod m; type Applied = Pair[A, B];",
            "mod m; type Applied = Pair[];",
            "mod m; type Applied = Pair[,A];",
            "mod m; type Applied = Pair[A,];",
            "mod m; type Applied = Pair(A);",
            "mod m; type Applied = (Pair)[A];",
            "mod m; fn run() { 42[I32, I64] }",
            "mod m; fn run() { 42(I32) }",
        ] {
            err(source);
        }
        ok("mod m; type Pair[A][B] = A & B; type Applied = Pair[List[I32]][[T] T -> T];");
    }

    #[test]
    fn binder_run_requires_body_type() {
        let m = err("mod m; type F = [A] -> R;");
        assert!(
            m.contains("type-binder run must be followed by a body type"),
            "msg = {m}"
        );
    }

    #[test]
    fn comma_after_function_type_binder_rejected() {
        let m = err("mod m; type Id = ([A], A) -> A;");
        assert!(m.contains("expected") || m.contains("grouped"), "msg = {m}");
    }

    #[test]
    fn comma_function_type_value_group_rejected() {
        let m = err("mod m; type F = (A, B) -> C;");
        assert!(
            m.contains("commas are not product type syntax"),
            "msg = {m}"
        );
    }

    #[test]
    fn malformed_type_atom_is_rejected() {
        err("mod m; type Pack[A] = (<U>, (A & U));");
    }

    #[test]
    fn newtype_universal_with_header_existential() {
        // Universals in brackets, existentials trailing as `<X>` atoms.
        ok("mod m; type { Pack[A] <U> : (A & U) { pub mk; pub get; }; };");
        ok("mod m; type { _Pack[_A] <_Hidden> : (_A & _Hidden) { pub mk; pub get; }; };");
        let message = err("mod m; type { Pack <_hidden> : . { pub mk; pub get; }; };");
        assert!(
            message.contains("must match _?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*"),
            "{message}"
        );
    }

    // ---- Greedy-fusion parsing ------------------------------------------
    //
    // The lexer fuses adjacent operator characters into one greedy
    // `SymbolRun` (matching the main kio-rs lexer); the parser peels a
    // structural delimiter (`->`, `>`) off the front of a fused run
    // where the grammar needs it. Dots and square brackets stay
    // separate structural tokens in both lexers.

    #[test]
    fn unit_type_stays_separate_from_equals() {
        ok("mod m; type A=.;");
        ok("mod m; type A = .;");
    }

    #[test]
    fn fused_kind_star_run_accepted() {
        // `**` / `***` lex as one greedy all-stars run; the binder
        // consumes the run and reads the following name.
        ok("mod m; type T[*F] = .;");
        ok("mod m; type T[**G] = .;");
        ok("mod m; type T[***H] = .;");
    }

    #[test]
    fn compact_forall_brackets_peel_from_maximal_runs() {
        ok("mod m; fn higher[A][*F][**G](x: A) -> A { x }");
        ok("mod m; type Nested = [A][B] A -> B;");
        ok("mod m; fn lambda() -> . { .[*F](x: F[.]) -> F[.] { x } }");
    }

    #[test]
    fn malformed_or_nested_forall_binders_stay_rejected() {
        for source in [
            "mod m; fn missing[A(x: A) -> A { x }",
            "mod m; fn nameless[*](x: .) -> . { x }",
            "mod m; fn nested[[A]](x: A) -> A { x }",
        ] {
            assert!(!err(source).is_empty(), "malformed binder parsed: {source}");
        }
    }

    #[test]
    fn fused_existential_close_peeled() {
        // `>:` (binder close flush against the payload separator) and
        // `><` (two binders with no whitespace) fuse; the parser peels
        // the `>` and leaves the residual for the next consumer —
        // mirrors kio-rs's `expect_existential_close`.
        ok("mod m; type { Single[A] <U>: (A & U) { pub mk; pub get; }; };");
        ok("mod m; type { Pair[A] <U><V>: (A & U & V) { pub mk; pub get; }; };");
    }

    #[test]
    fn fused_unit_dot_before_chain_op_peeled() {
        // `.&` / `.|` fuse the unit atom with the chain separator; the
        // parser peels the `.` — mirrors kio-rs's `at_unit_type_dot`.
        ok("mod m; type P = (.& A);");
        ok("mod m; type S = (.| A);");
        ok("mod m; type P2 = (A &.);");
    }

    #[test]
    fn fused_arrow_after_unit_dot_peeled() {
        // `.->` fuses the unit domain with the function arrow; peeling
        // the `.` leaves `->` for the arrow consumer.
        ok("mod m; type F = .-> .;");
    }

    #[test]
    fn fused_amp_run_still_one_separator() {
        // All-`&` / all-`|` runs collapse to one chain separator
        // whether spaced (`& & &`) or fused (`&&&`).
        ok("mod m; type P = (A && B);");
        ok("mod m; type P2 = (&&& A &&& B &&&);");
        ok("mod m; type S = (A || B);");
    }

    #[test]
    fn fused_mixed_op_run_rejected() {
        // `&|` is neither an all-`&` nor an all-`|` run — mixing
        // still rejects, fused or spaced.
        let m = err("mod m; type P = (A &| B);");
        assert!(m.contains("mixing") || m.contains("expected"), "msg = {m}");
    }

    #[test]
    fn host_type_and_fn_module_body_items() {
        // Opaque host declarations live in the mod body: a `host
        // type` with a `role(...)` annotation and a signature-only
        // `host fn` whose value parameter references it.
        ok("mod m; host type String role(str); host fn print(p0: String) -> .;");
    }

    #[test]
    fn host_type_generic() {
        // A generic host type and a host fn taking a parametric host type.
        ok("mod m; \
             host type Box[T]; \
             host type File; \
             host fn read_string(x: Box[String]) -> String;");
    }

    #[test]
    fn host_items_accept_redundant_pub() {
        // Host items are always public; a `pub` before or after `host`
        // is admitted but redundant.
        ok("mod m; pub host type Token;");
        ok("mod m; host pub type Token;");
        ok("mod m; pub host fn print(p0: String) -> .;");
        ok("mod m; host pub fn print(p0: String) -> .;");
    }

    #[test]
    fn host_keyword_is_contextual() {
        // `host` is only the host-declaration modifier when followed by
        // `type` / `fn`. A function *named* `host` parses as an
        // ordinary definition, and `host` is usable as a value
        // identifier inside a body.
        ok("mod m; fn host() -> . { () }");
        ok("mod m; fn f() -> . { host } fn host() -> . { () }");
    }

    #[test]
    fn host_fn_unit_default_still_rejects_a_body() {
        ok("mod m; host fn print(p0: String);");
        ok("mod m; host fn tick();");
        ok("mod m; host fn staged()[A];");
        ok("mod m; host fn staged[A](value: A)[B](other: B);");
        let m = err("mod m; host fn print(p0: String) { () }");
        assert!(m.contains("`;`"), "msg = {m}");
    }

    #[test]
    fn host_fn_with_type_binder() {
        // Type-parameter binders precede the value group, as in any
        // function signature.
        ok("mod m; host fn identity[A](x: A) -> A;");
    }

    #[test]
    fn rejects_mismatched_binder_brackets() {
        // `[A>` mismatches bracket opener with angle closer — the
        // closer expectation complains.
        let m = err("mod m; fn id[A>(x: A) -> A { x }");
        assert!(m.contains("`]`"), "msg = {m}");
    }

    #[test]
    fn fn_def_unit_returns_unit() {
        ok("mod m; fn main() -> . { () }");
    }

    #[test]
    fn fn_def_unit_return_elided() {
        // The `-> .` annotation may be omitted (one optional type
        // position; see specs/language.md § Function definitions).
        ok("mod m; fn main() { () }");
    }

    // ---- Block-body semicolon runs --------------------------------------
    //
    // `BlockBody ::= ';'* (Stmt ';'*)* Expr ';'*` (specs/grammar.md
    // § Productions (Kio')) admits `;` runs at every position: leading,
    // repeated between statements, and trailing after the final
    // expression. The verifier must accept exactly what the kio-rs
    // parser accepts here; these mirror kio-rs's
    // `block_repeated_and_trailing_semicolons_parse` and
    // `final_semicolon_after_block_expression_is_ignored`.

    #[test]
    fn block_repeated_and_trailing_semicolons_parse() {
        ok("mod x; fn f() -> . { ;;; ();;; ();;; }");
    }

    #[test]
    fn final_semicolon_after_block_expression_is_ignored() {
        ok("mod x; fn f() -> . { (); }");
    }

    #[test]
    fn empty_block_has_no_final_expression() {
        // A block with no final expression is a parse error — the `;`
        // runs never supply the mandatory trailing expression.
        assert!(
            err("mod x; fn f() -> . { }").contains("no final expression"),
            "empty block rejected"
        );
        assert!(
            err("mod x; fn f() -> . { ;; }").contains("no final expression"),
            "`;`-only block rejected"
        );
        assert!(
            err("mod x; fn f() -> . { let x = (); }").contains("no final expression"),
            "block ending in a statement rejected"
        );
    }

    #[test]
    fn type_alias_with_params() {
        ok("mod m; type Pair[A][B] = (A & B);");
    }

    #[test]
    fn label_forwarding_is_not_kio_prime_syntax() {
        for source in [
            "mod m; type {forward} = {count};",
            "mod m; pub type {forward} = {count};",
            "mod m; pub(scope/path) type {forward} = {origin.count};",
            "mod m; rec { type {forward} = {count}; type Alias = .; }",
        ] {
            let message = err(source);
            assert!(
                message.contains("type name `forward`"),
                "{source}: {message}"
            );
        }
        for source in [
            "mod m; type {Forward} = {Count};",
            "mod m; rec { type {Forward} = {Count}; type Alias = .; }",
        ] {
            let message = err(source);
            assert!(
                message.contains("before nominal payload"),
                "{source}: {message}"
            );
        }
        ok("mod m; type Alias = .; pub(scope/path) type Scoped[A] = A;");
        ok("mod m; fn type(forward: .) -> . { forward }");
    }

    #[test]
    fn rejects_literal_side_alias() {
        let m = err("mod m; lit greeting = \"hi\";");
        assert!(m.contains("surface sugar"), "msg = {m}");
    }

    #[test]
    fn newtype_nullary() {
        ok("mod m; type { Wrap : . { pub mk; pub get; }; };");
    }

    #[test]
    fn outer_braced_semicolons_are_optional_and_single() {
        for declaration in [
            "fn f() -> . { () }",
            "type { Wrap : . { mk; get }; }",
            "rec type { Loop : . | Loop { mk; get }; }",
            "rec { type A = B; type B = A }",
        ] {
            ok(&format!("mod m; {declaration} fn next() -> . {{ () }}"));
            ok(&format!("mod m; {declaration}; fn next() -> . {{ () }}"));
            err(&format!("mod m; {declaration};;"));
        }
        for declaration in ["type A = .", "host type A", "host fn f() -> ."] {
            err(&format!("mod m; {declaration}"));
            ok(&format!("mod m; {declaration};"));
        }
        err("mod m; ; fn f() -> . { () }");
    }

    #[test]
    fn newtype_semicolon_edges_preserve_member_policy() {
        for body in ["mk; get", "; mk; get;", ";; get;; mk;;"] {
            ok(&format!("mod m; type {{ Wrap : . {{ {body} }}; }}"));
        }
        for body in ["mk get", ";", "mk; mk; get", "mk"] {
            err(&format!("mod m; type {{ Wrap : . {{ {body} }}; }}"));
        }
    }

    #[test]
    fn nominal_outer_entry_has_single_semicolon_edges() {
        for entry in [
            "N: . { mk; get }",
            "; N: . { mk; get };",
            "N: . { ;;; mk;;; get;;; };",
        ] {
            ok(&format!("mod m; type {{ {entry} }}"));
        }
        for entry in [";; N: . { mk; get }", "N: . { mk; get };;", ";", ""] {
            err(&format!("mod m; type {{ {entry} }}"));
        }
    }

    #[test]
    fn prime_nominal_declarations_require_an_explicit_singleton() {
        for declaration in [
            "type { N: .; }",
            "type { N: . {}; }",
            "type { N: . { mk; get; }; M: . { mk; get; }; }",
            "type Alias = { N: . { mk; get; }; };",
            "type Alias = . & { N: . { mk; get; }; };",
            "rec type { N: . | N; }",
            "rec type Alias = { N: . | Alias { mk; get; }; };",
            "rec { type Alias = { N: . { mk; get; }; }; type Other = Alias; }",
        ] {
            err(&format!("mod m; {declaration}"));
        }
        ok("mod m; pub(scope) type { _N[*F][A]<Hidden>: F[A] & Hidden { pub(scope) get; mk; }; }");
        ok("mod m; rec type { N: . | N { mk; get; }; }");
    }

    #[test]
    fn nominal_member_roles_reject_custom_names_and_bodies() {
        for members in [
            "constructor make; projector read;",
            "mk make; get read;",
            "mk _Foo; get;",
            "mk _foo; get;",
            "mk; get _Foo;",
            "mk; get _foo;",
            "pub make; pub read;",
            "mk() { () }; get;",
            "mk; get { () };",
        ] {
            err(&format!("mod m; type {{ N: . {{ {members} }}; }}"));
        }
    }

    #[test]
    fn recursive_group_semicolons_belong_to_the_group() {
        for body in [
            "type A = B; type B = A",
            "; type A = B; type B = A;",
            "; type { Box : A { mk; get }; }; type A = Box;",
        ] {
            ok(&format!("mod m; rec {{ {body} }}"));
        }
        for body in [
            "type A = B type B = A",
            "type { Box : A { mk; get }; } type A = Box",
            ";; type A = B; type B = A",
            "type A = B;; type B = A",
            "type A = B; type B = A;;",
            ";",
            "; type A = A;",
        ] {
            err(&format!("mod m; rec {{ {body} }}"));
        }
    }

    #[test]
    fn newtype_recursive_self_ref() {
        ok("mod m; rec type { List[A] : (. | (A & List[A])) { pub mk; pub get; }; };");
    }

    #[test]
    fn mutually_recursive_type_group_is_kio_prime_syntax() {
        ok("mod m; rec { \
             type Loop = Box; \
             type { Box : . | Loop { pub mk; pub get; }; }; \
             }");
    }

    #[test]
    fn recursive_type_forms_keep_the_kio_prime_boundary_narrow() {
        assert!(err("mod m; rec type Loop = Loop;").contains("explicit singleton nominal"));
        assert!(
            err("mod m; rec type Loop = { Next: Loop };").contains("explicit singleton nominal")
        );
        assert!(
            err("mod m; pub rec { type A = B; type B = A; }").contains("no leading visibility")
        );
        assert!(err("mod m; rec { fn f() -> . { () } type A = .; }").contains("expected `type`"));
    }

    #[test]
    fn newtype_with_existential_binder() {
        ok("mod m; type { Pack[A] <U> : (A & U) { pub mk; pub get; }; };");
    }

    #[test]
    fn newtype_with_multiple_existential_binders() {
        ok("mod m; type { Pack[A] <L> <R> : (A & L & R) { pub mk; pub get; }; };");
    }

    #[test]
    fn newtype_with_existentials_no_universals() {
        ok("mod m; type { Pack <U> : U { pub mk; pub get; }; };");
    }

    #[test]
    fn newtype_members_reversed_order() {
        ok("mod m; type { Wrap[A] : A { pub get; pub mk; }; };");
    }

    #[test]
    fn newtype_duplicate_constructor_rejected() {
        let m = err("mod m; type { W : . { pub mk; pub mk; pub get; }; };");
        assert!(m.contains("duplicate `mk`"), "got: {m}");
    }

    #[test]
    fn newtype_missing_projector_rejected() {
        let m = err("mod m; type { W : . { pub mk; }; };");
        assert!(m.contains("missing `get`"), "got: {m}");
    }

    #[test]
    fn import_intrinsics() {
        ok("mod m; import __intrinsics__;");
    }

    #[test]
    fn import_selection_requires_separators() {
        err("mod m; import provider { fn foo type String }");
    }

    #[test]
    fn import_qualified_with_alias() {
        ok("mod m; import a/b as q;");
    }

    #[test]
    fn prime_only_internal_qualified_import_is_rejected() {
        for source in [
            "mod m; import __internal__ a/b as __reserved__;",
            "mod m; import __internal__ as q;",
        ] {
            let message = err(source);
            assert!(
                message.contains("`__internal__` is not admitted in Kio' imports"),
                "got: {message}"
            );
        }
    }

    #[test]
    fn multi_segment_module_decl_and_import_path() {
        ok("mod a/b/c; import x/y/z { fn f }");
    }

    #[test]
    fn import_selection_accepts_kinds_and_layout() {
        for source in [
            "mod m; import provider { fn foo }",
            "mod m; import provider { type Foo; fn foo; }",
            "mod m; import provider { ;; type Foo;;; fn foo;; };",
            "mod m; import provider { fn op; fn op_var; fn import; fn type }",
            "mod m; import provider { // first item\n type Foo; // separator\n fn foo }",
        ] {
            ok(source);
        }
    }

    #[test]
    fn empty_or_incomplete_import_selection_is_rejected() {
        for source in [
            "mod m; import provider {}",
            "mod m; import provider { ;;; }",
            "mod m; import provider { fn foo };;",
            "mod m; import provider {",
            "mod m; import provider { fn foo;",
            "mod m; import provider { fn foo",
            "mod m; import provider;",
            "mod m; import provider { foo }",
        ] {
            assert!(!err(source).is_empty(), "accepted `{source}`");
        }
    }

    #[test]
    fn surface_imports_and_variadic_declarations_are_rejected() {
        for source in [
            "mod m; import syntax { op _ <+> _ }",
            "mod m; import syntax { op_macro _ ? _ : ___ }",
            "mod m; import syntax { op_var [* *] }",
            "mod m; import syntax { lit answer }",
            "mod m; import syntax { macro build }",
        ] {
            err(source);
        }
        assert!(
            err("mod m; pub op_var [* *] { foldr1 step seed; } ")
                .contains("operator declarations are Kio surface sugar")
        );
    }

    #[test]
    fn builtin_imports_keep_their_phase_and_statement_boundaries() {
        assert!(err("mod m; import __comptime__;").contains("compile-time surface syntax"));
        for source in [
            "mod m; import __intrinsics__ { fn __pair__ }",
            "mod m; import __intrinsics__ as core;",
            "mod m; fn f() -> . { () } import provider { fn foo }",
        ] {
            assert!(!err(source).is_empty(), "accepted `{source}`");
        }
    }

    #[test]
    fn doc_comment_cannot_attach_to_an_import() {
        for import in ["import __intrinsics__;", "import provider { fn foo }"] {
            let message = err(&format!("mod m;\n/// Documentation\n{import}"));
            assert!(message.contains("`import` clauses are not documented"));
        }
        ok("mod m; import provider { fn foo }\n/// Documentation\nfn f() -> . { () }");
        ok("mod m; fn f(import: .) -> . {\n/// Parameter\nimport\n}");
        ok("mod m; fn\n/// Name trivia\nimport() -> . { () }");
        ok("mod\n/// Module-name trivia\nimport;");
    }

    #[test]
    fn match_identifier_is_not_a_surface_construct() {
        ok("mod m; fn match() -> . { () } fn f() -> . { match() }");
    }

    #[test]
    fn contextual_module_aliases_remain_ordinary_paths() {
        for alias in [
            "module",
            "import",
            "as",
            "pub",
            "pure",
            "type",
            "fn",
            "rec",
            "newtype",
            "let",
            "constructor",
            "projector",
            "host",
            "if",
            "do",
            "else",
            "match",
            "scope",
            "ordinary",
        ] {
            for expression in [
                format!("{alias}.value"),
                format!("{alias}.value()"),
                format!("({alias}.value)()"),
                format!("apply({alias}.value)"),
                format!("{alias}.Box.make(())"),
            ] {
                ok(&format!(
                    "mod m; import provider as {alias}; fn run() {{ {expression} }}"
                ));
            }
        }
    }

    #[test]
    fn contextual_names_do_not_admit_control_or_bang_syntax() {
        for expression in [
            "if x { y } else { z }",
            "do bind { () }",
            "if() { () }",
            "do() { () }",
            "if! .t[Bool] { () } else { () }",
            "do! bind { () }",
            "if.value!()",
            "do.value() { () }",
            "if.",
            "do..value()",
        ] {
            err(&format!("mod m; fn run() {{ {expression} }}"));
        }
    }

    #[test]
    fn rejects_unlowered_operator_calls() {
        err("mod m; fn f() -> . { into!(x) }");
    }

    #[test]
    fn macro_imports_erase_while_contextual_names_remain_ordinary() {
        for selector in ["fn macro", "fn macro as local"] {
            ok(&format!(
                "mod m; import provider {{ {selector} }} fn macro() {{ () }}"
            ));
        }
        for selector in [
            "macro build",
            "macro build as local",
            "macro as",
            "macro as as local",
        ] {
            err(&format!(
                "mod m; import provider {{ {selector} }} fn run() {{ () }}"
            ));
        }
        err("mod m; macro build: . -> . { impl helper }");
        ok("mod m; fn macro() { () } fn run() { macro() }");
    }

    #[test]
    fn dotted_value_paths_are_identifier_led_and_precede_calls() {
        ok("mod m; fn f(x: .) -> . { module_alias.member(x) }");
        for source in [
            "mod m; fn f() -> . { (value).member }",
            "mod m; fn f() -> . { value().member }",
        ] {
            let message = err(source);
            assert!(message.contains("dotted value paths"), "msg = {message}");
        }
    }

    #[test]
    fn accepts_chained_amp() {
        // Chained `&`/`|` are accepted as part of the language-level
        // any-position-operator rule (specs/language.md § Comma-
        // separated lists and operator chains).
        ok("mod m; type T = (A & B & C);");
    }

    #[test]
    fn accepts_chained_pipe() {
        ok("mod m; type T = (A | B | C);");
    }

    #[test]
    fn accepts_leading_amp() {
        // Leading-operator multi-line layout (the formatter's
        // canonical shape for chains too long to fit on one line).
        ok("mod m; type T = ( & A & B & C );");
    }

    #[test]
    fn accepts_repeated_amp_and_pipe() {
        ok("mod m; type P = ( &&& A &&& B &&& );");
        ok("mod m; type S = ( ||| A ||| B ||| );");
    }

    #[test]
    fn accepts_unary_and_empty_type_delimiter_runs() {
        ok("mod m; type P = & A;");
        ok("mod m; type S = | A;");
        ok("mod m; type P = &;");
        ok("mod m; type S = |;");
        ok("mod m; type P = (&);");
        ok("mod m; type S = (|);");
        ok("mod m; type P = (A &);");
        ok("mod m; type S = (A |);");
    }

    #[test]
    fn rejects_comma_product_type_delimiter_runs() {
        for source in [
            "mod m; type P = (A, B);",
            "mod m; type P = (, A ,);",
            "mod m; type P = (,,,);",
        ] {
            let m = err(source);
            assert!(
                m.contains("commas are not product type syntax"),
                "source = {source}; msg = {m}"
            );
        }
    }

    #[test]
    fn rejects_mixed_amp_pipe_in_chain() {
        let m = err("mod m; type T = (A & B | C);");
        assert!(m.contains("mixing"), "msg = {m}");
        let m = err("mod m; type T = A & B | C;");
        assert!(m.contains("mixing"), "msg = {m}");
        let m = err("mod m; type T = A | B & C;");
        assert!(m.contains("mixing"), "msg = {m}");
    }

    #[test]
    fn accepts_comma_framed_value_grouping() {
        ok("mod m; fn f(x: A) -> A { (x,) }");
        ok("mod m; fn f(x: A) -> A { (, x) }");
        ok("mod m; fn f() -> . { (,,,) }");
    }

    #[test]
    fn rejects_n_ary_tuple_value() {
        let m = err("mod m; fn f() -> . { (a, b) }");
        assert!(m.contains("n-ary tuple"), "msg = {m}");
    }

    #[test]
    fn bare_literal_rejected() {
        // Kio' mandates the `[Type]` annotation on every literal
        // (specs/grammar.md § Kio' grammar — `LiteralCall ::= LITERAL
        // '[' Type ']'`); the bare-literal form is surface-only.
        let m = err("mod m; fn f() -> . { 42; () }");
        assert!(m.contains("annotation") || m.contains("Kio'"), "msg = {m}");
        let m = err("mod m; fn f() -> . { 3.14; () }");
        assert!(m.contains("annotation") || m.contains("Kio'"), "msg = {m}");
        let m = err("mod m; fn f() -> . { \"hi\"; () }");
        assert!(m.contains("annotation") || m.contains("Kio'"), "msg = {m}");
        let m = err("mod m; fn f() -> . { .t; () }");
        assert!(m.contains("annotation") || m.contains("Kio'"), "msg = {m}");
    }

    #[test]
    fn literal_call_annotation_accepted() {
        // The trailing `[Type]` annotation form — `42[I32]`,
        // `"hi"[String]`, `.t[Bool]` — is mandatory on every Kio'
        // literal.
        ok("mod m; fn f() -> . { 42[I32]; () }");
        ok("mod m; fn f() -> . { 3.14[F64]; () }");
        ok("mod m; fn f() -> . { \"hi\"[String]; () }");
        ok("mod m; fn f() -> . { .t[Bool]; () }");
        ok("mod m; fn f() -> . { .f[Bool]; () }");
    }

    #[test]
    fn rejects_suffix_literal() {
        // The old lexer-side width suffix (`42i32`) is gone: the
        // digits lex as a bare literal and the `i32` as a trailing
        // identifier the parser can't place.
        err("mod m; fn f() -> . { 42i32 }");
    }

    #[test]
    fn fn_without_annotations() {
        // `fn` value-params and return type may be left bare — the
        // type is filled by bidirectional flow from the use site.
        ok("mod m; fn a() -> . { .(x) { x }(()) }");
    }

    #[test]
    fn fn_value_param_annotation_accepted() {
        // The `: T` value-parameter annotation is optional but
        // admissible — load-bearing for a `let`-bound lambda.
        ok("mod m; fn a() -> . { .(x: .) { x }(()) }");
    }

    #[test]
    fn fn_return_annotation_accepted() {
        // The `-> R` return-type annotation is optional but admissible.
        ok("mod m; fn a() -> . { .(x) -> . { x }(()) }");
    }

    #[test]
    fn let_binder_annotation_is_not_kio_prime_grammar() {
        let msg = err("mod m; fn a() -> . { let y: . = (); y }");
        assert!(
            msg.contains("expected `}` closing block")
                || msg.contains("expected end of file")
                || msg.contains("expected expression"),
            "let binder annotation must fail by grammar mismatch, not an annotation-specific branch: {msg}"
        );
    }

    #[test]
    fn nested_function_type() {
        ok("mod m; fn a(t: . -> .) -> . { t(()) }");
    }

    #[test]
    fn accepts_bare_unary_function_type() {
        ok("mod m; type F = A -> B;");
        ok("mod m; type F = A -> B -> C;");
        ok("mod m; type F = A -> (B & C);");
        ok("mod m; type F = (A -> B);");
    }

    #[test]
    fn arrow_result_chains_require_grouping_but_signature_returns_do_not() {
        for result in ["B & C", "B | C", "& B &", "| B |", "&", "|"] {
            for domain in ["A", "(A)", "(A & B)", "(A | B)", "([A] A -> A)"] {
                for source in [
                    format!("mod m; type F = {domain} -> {result};"),
                    format!("mod m; fn f() -> {domain} -> {result} {{ () }}"),
                    format!("mod m; fn f() {{ .() -> {domain} -> {result} {{ () }} }}"),
                ] {
                    let message = err(&source);
                    if matches!(result, "B & C" | "B | C") {
                        assert!(message.contains("right of `->`"), "{source}: {message}");
                    }
                }
                ok(&format!("mod m; type F = {domain} -> ({result});"));
            }
            if matches!(result, "B & C" | "B | C") {
                ok(&format!("mod m; fn f() -> {result} {{ () }}"));
                ok(&format!("mod m; fn f() {{ .() -> {result} {{ () }} }}"));
            }
        }
        ok("mod m; type F = A -> B -> C;");
        ok("mod m; type F = A -> [B] B & B;");
    }

    #[test]
    fn rejects_bare_product_or_sum_arrow_lhs() {
        let msg = err("mod m; type F = A & B -> C;");
        assert!(
            msg.contains("left of `->`") || msg.contains("parenthesized"),
            "msg = {msg}"
        );
        let msg = err("mod m; type F = A | B -> C;");
        assert!(
            msg.contains("left of `->`") || msg.contains("parenthesized"),
            "msg = {msg}"
        );
    }

    #[test]
    fn explicit_type_stage_accepts_compound_product_type() {
        // Type operands belong to the explicit square stage.
        ok("mod m; import __intrinsics__; fn a(p: (. & .)) -> . { __snd__[.][.](p) }");
    }

    #[test]
    fn explicit_type_stage_accepts_compound_sum_type() {
        ok(
            "mod m; import __intrinsics__; fn a(s: (. | .)) -> . { __either__[.][.][.](s, .(_l) { () }, .(_r) { () }) }",
        );
    }

    #[test]
    fn explicit_type_stage_accepts_function_type() {
        // A function type stays inside the explicit type stage.
        ok("mod m; import __intrinsics__; fn a() -> . { __pair__[(. -> .)][.](.(_x) { () }, ()) }");
    }

    #[test]
    fn boolean_operand_intrinsics_are_ordinary_explicit_core_calls() {
        for intrinsic in ["__and__", "__or__"] {
            ok(&format!(
                "mod m; import __intrinsics__; host type Flag role(bool); fn f(x: Flag) -> Flag {{ let selected = {intrinsic}[Flag]; selected(x, .() -> Flag {{ x }}) }}"
            ));
        }
    }

    #[test]
    fn explicit_type_stage_accepts_bottom_type() {
        ok("mod m; import __intrinsics__; fn a(b: !) -> . { __absurd__[.](b) }");
    }

    #[test]
    fn explicit_type_stage_and_value_packet_have_separate_roles() {
        // Name roles follow written stage delimiters, before lookup.
        ok("mod m; fn a() -> . { f[A](b) }");
    }

    #[test]
    fn explicit_call_stages_compose_without_whitespace_dispatch() {
        for expression in [
            "f[A][B](a)[C](b)",
            "f [ A ] [ B ] (a) [C] (b)",
            "f[A]",
            "f[A][B](a, b)",
            "f(a)(b)",
            "(f)[A](a)",
            "(f(a))[B](b)",
            "(.[A](x: A) { x })[.](())",
            "f[(A & B)][(! | C)][D[E]][[F] F -> F](value)",
            "f[A][B](, a,, b,)",
            "f[A]()(())",
            "f // type stage follows trivia\n [A](value)",
        ] {
            ok(&format!("mod m; fn run() {{ {expression} }}"));
        }
        // Type arguments and literal annotations share the square delimiter;
        // declaration brackets continue to bind names.
        ok(
            "mod m; host type Number role(i32); type Alias[A] = A; fn id[A](x: A) -> A { x } fn run() { id[Alias[Number]](42[Number]) }",
        );
    }

    #[test]
    fn type_shaped_paths_are_rejected_only_as_value_packet_operands() {
        for expr in [
            "A",
            "m.A",
            "(A)",
            "A[B]",
            "m.A[B](())",
            "f(A.member)",
            "f(.[A](v: A) -> A { A })",
            "f(__value)",
        ] {
            ok(&format!("mod m; fn run() {{ {expr} }}"));
        }
        for expr in [
            "f(A)",
            "f(m.A)",
            "f((A))",
            "f(A[B])",
            "f(m.A[B]())",
            "f(g(A))",
            "f(__Type__)",
            "f((A)[B]())",
            "f(.() { g(A) })",
        ] {
            err(&format!("mod m; fn run() {{ {expr} }}"));
        }
    }

    #[test]
    fn explicit_call_stages_reject_empty_types_and_mixed_value_packets() {
        for expression in [
            "f[]",
            "f[,](value)",
            "f[x]",
            "f[A, value](x)",
            "f[A, B](x)",
            "f[, A](x)",
            "f[A,](x)",
            "f[A B](x)",
            "f[_](x)",
            "f(A, x)",
            "f(m.A, x)",
            "f(.)",
            "f(!)",
            "f((A & B))",
            "f([A] A -> A)",
            "f(.[A] A)",
            "f[A].member",
        ] {
            err(&format!("mod m; fn run() {{ {expression} }}"));
        }
    }

    #[test]
    fn call_args_accept_exact_name_roles_only() {
        ok("mod ok; fn id[A](x: A) -> A { x } \
             fn ordinary[A](value: A) -> A { id[A](value) } \
             fn marked[_A](_value: _A) -> _A { id[_A](_value) } \
             fn qualified[_A](_value: _A) -> _A { \
                 id[m._Foo](_value); \
                 let _ = id[_A](Formtype.member[_A](_value)); \
                 let _ = id[_A](m._value1.member(_value)); \
                 id[_A](m._Foo.member(_value)) \
             }");
        for (argument, invalid_name) in [
            ("_FooBar", "_FooBar"),
            ("_FooBar(_A)", "_FooBar"),
            ("_1Foo", "_1Foo"),
            ("FooBar", "FooBar"),
            ("__FooBar", "__FooBar"),
            ("m._FooBar", "_FooBar"),
            ("m._FooBar(_A)", "_FooBar"),
            ("m._1Foo", "_1Foo"),
            ("m.FooBar", "FooBar"),
            ("m.__FooBar", "__FooBar"),
            ("m._FooBar.member", "_FooBar"),
            ("m._FooBar.member(_A)", "_FooBar"),
            ("m._1Foo.member", "_1Foo"),
            ("m.FooBar.member", "FooBar"),
            ("m.__FooBar.member", "__FooBar"),
        ] {
            let message = err(&format!(
                "mod bad; fn id[A](x: A) -> A {{ x }} \
                 fn f[_A](x: .) -> . {{ id({argument}, x) }}"
            ));
            assert!(message.contains(invalid_name), "{argument}: {message}");
        }
    }

    #[test]
    fn expression_paths_accept_exact_name_roles_only() {
        ok("mod ok; fn f[_A](_value: .) -> . { m._Foo.member[_A](_value) }");
        ok("mod ok; fn f[_A](_value: .) -> . { __pair__[_A][_A](_value, _value) }");
        ok("mod ok; fn f() -> . { __Foo.member(__value) }");
        for (expression, invalid_name) in [
            ("_FooBar", "_FooBar"),
            ("_1Foo", "_1Foo"),
            ("FooBar", "FooBar"),
            ("__FooBar", "__FooBar"),
            ("m._FooBar.member", "_FooBar"),
            ("m._1Foo.member", "_1Foo"),
            ("m.FooBar.member", "FooBar"),
            ("m.__FooBar.member", "__FooBar"),
        ] {
            let message = err(&format!("mod bad; fn f() -> . {{ {expression} }}"));
            assert!(message.contains(invalid_name), "{expression}: {message}");
        }
    }

    #[test]
    fn letterless_reserved_shapes_are_not_names() {
        for name in ["__", "___", "____", "__1__"] {
            let expression = err(&format!("mod bad; fn f() -> . {{ {name} }}"));
            assert!(!expression.is_empty(), "expression path accepted `{name}`");

            let ty = err(&format!("mod bad; type T = {name};"));
            assert!(!ty.is_empty(), "type path accepted `{name}`");
        }

        ok(
            "mod good; import __intrinsics__; type T = __Type__; fn f(x: ., y: .) -> . { __pair__[.][.](x, y) }",
        );
    }

    #[test]
    fn named_import_aliases_are_shared_core_syntax() {
        ok(
            "mod m; import provider { type Foo as Bar; fn foo as bar; fn same as same; fn op as operation } fn run() -> . { () }",
        );
        for selection in [
            "type Foo as bar",
            "fn foo as Bar",
            "fn foo as __bad__",
            "op _ + _ as _ - _",
            "op_var [* *] as [% %]",
        ] {
            assert!(!err(&format!("mod m; import provider {{ {selection} }}")).is_empty());
        }
    }

    #[test]
    fn selective_imports_accept_exact_name_roles_only() {
        ok(
            "mod m; import provider { type Foo; fn foo; type _Foo; fn _foo; fn _foo1 } fn a() -> . { () }",
        );
        for name in ["_FooBar", "_1Foo", "FooBar", "__Foo"] {
            let message = err(&format!(
                "mod m; import provider {{ type {name} }} fn a() -> . {{ () }}"
            ));
            assert!(message.contains(name), "{name}: {message}");
        }
    }

    #[test]
    fn marked_type_names_are_rejected_in_value_only_roles() {
        for source in [
            "mod m; type T = m._Foo;",
            "mod m; type T = m.Foo;",
            "mod m; fn f(_: .) -> . { let _ = (); .(_: .) { () }(()) }",
        ] {
            ok(source);
        }
        for (role, invalid, valid) in [
            ("mod head", "mod _Foo;", "mod _foo;"),
            ("mod tail", "mod m/_Foo;", "mod m/_foo;"),
            (
                "qualified import source",
                "mod m; import _Foo as dep; fn f() -> . { () }",
                "mod m; import _foo as dep; fn f() -> . { () }",
            ),
            (
                "qualified import source tail",
                "mod m; import provider/_Foo as dep; fn f() -> . { () }",
                "mod m; import provider/_foo as dep; fn f() -> . { () }",
            ),
            (
                "qualified import alias",
                "mod m; import provider as _Foo; fn f() -> . { () }",
                "mod m; import provider as _foo; fn f() -> . { () }",
            ),
            (
                "function name",
                "mod m; fn _Foo() -> . { () }",
                "mod m; fn _foo() -> . { () }",
            ),
            (
                "host function name",
                "mod m; host fn _Foo() -> .;",
                "mod m; host fn _foo() -> .;",
            ),
            (
                "function parameter",
                "mod m; fn f(_Foo: .) -> . { () }",
                "mod m; fn f(_foo: .) -> . { () }",
            ),
            (
                "host function parameter",
                "mod m; host fn f(_Foo: .) -> .;",
                "mod m; host fn f(_foo: .) -> .;",
            ),
            (
                "lambda parameter",
                "mod m; fn f() -> . { .(_Foo: .) { () } }",
                "mod m; fn f() -> . { .(_foo: .) { () } }",
            ),
            (
                "let binder",
                "mod m; fn f() -> . { let _Foo = (); () }",
                "mod m; fn f() -> . { let _foo = (); () }",
            ),
            (
                "type-path mod qualifier",
                "mod m; type T = _Foo.Bar;",
                "mod m; type T = _foo.Bar;",
            ),
            (
                "type-path mod qualifier tail",
                "mod m; type T = provider/_Foo.Bar;",
                "mod m; type T = provider/_foo.Bar;",
            ),
        ] {
            let message = err(invalid);
            assert!(message.contains("_Foo"), "{role}: {message}");
            ok(valid);
        }
    }

    #[test]
    fn slash_only_qualified_type_paths_are_rejected() {
        for source in ["mod m; type T = m._Foo;", "mod m; type T = m.Foo;"] {
            ok(source);
        }
        for source in ["mod m; type T = m/_Foo;", "mod m; type T = m/Foo;"] {
            let message = err(source);
            assert!(
                message.contains("qualified type paths use"),
                "{source}: {message}"
            );
        }
    }

    #[test]
    fn malformed_lambda_value_never_becomes_a_type_argument() {
        // A malformed lambda remains a rejected value; it cannot fall back
        // to a type argument after consuming its structural dot.
        let _message = err("mod m; fn a() -> . { f(.[A] A) }");
    }

    #[test]
    fn function_type_leading_binder_run_accepted() {
        // `[A][B] T -> R` — all binders at the leading position,
        // then a value group.
        ok("mod m; fn a(_f: [A][B] A -> B) -> . { () }");
    }

    #[test]
    fn function_return_forall_accepted() {
        // `P0 -> [A] P1 -> R` is `Function(P0, Forall(A,
        // Function(P1, R)))`: the arrow return can be any type,
        // including a `forall`.
        ok("mod m; fn a(_f: A -> [B] B -> A) -> . { () }");
    }

    #[test]
    fn nested_function_return_forall_accepted() {
        ok("mod m; fn a(_f: A -> B -> [C] C -> C) -> . { () }");
    }

    #[test]
    fn function_type_forall_value_group_domain_accepted() {
        // A value group's domain may itself be a forall (Rank-N):
        // `([U] U -> R) -> R`, the shape an existential newtype's
        // CPS projector takes as a continuation. `specs/prime.md`
        // § Kind grammar / § Mid-position binders admit Rank-N.
        ok("mod m; fn a(_f: ([U] U -> .) -> .) -> . { () }");
    }

    #[test]
    fn function_type_existential_projector_value_accepted() {
        // The full first-class value type of an existential newtype's
        // CPS projector. This is what `route` eta-expands a
        // value-position projector reference into, so the kio-prime
        // build-target round-trip must re-verify it.
        ok("mod m; \
            type { Pack <U> : . & U { mk; get; }; }; \
            fn a(_f: Pack -> [R] ([U] (Pack & U) -> R) -> R) -> . { () }");
    }
}
