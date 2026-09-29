//! Recursive-descent parser implementing the Kio' grammar from
//! `specs/prime.md` § Grammar.
//!
//! Returns `Ok(())` if the source is syntactically Kio'. Any
//! deviation — surface sugar (`if`/`else`, `do`, `labels`,
//! any `<name>!(…)` elaborator-form syntax, n-ary tuple literals at
//! the value level), type-expression naming-rule violations, or any
//! other parse failure — is reported as `ParseError`.
//!
//! Call-argument lists accept either expressions or types (per
//! `specs/prime.md` § Grammar's `CallArg` production), since the
//! callee's signature is what disambiguates the two; the grammar
//! makes no syntactic distinction.
//!
//! Naming-convention enforcement is syntax-local: declaration heads,
//! binders, type-expression paths, selective imports, and ambiguous raw
//! call arguments are checked against their exact admitted name roles.
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
    /// checkpoint is only `(pos, current_residual)` and speculative call-arg
    /// parsing cannot leak a peeled spelling into its fallback branch.
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

    fn token_is_statement_boundary(tok: Option<&TokenKind>) -> bool {
        matches!(
            tok,
            None | Some(TokenKind::Comma)
                | Some(TokenKind::RParen)
                | Some(TokenKind::RBrace)
                | Some(TokenKind::Semicolon)
        )
    }

    fn peek_after_paren_group_at(&self, open_offset: usize) -> Option<&TokenKind> {
        if !matches!(self.peek_kind_at(open_offset), Some(TokenKind::LParen)) {
            return None;
        }
        let mut depth: i32 = 0;
        let mut offset = open_offset;
        loop {
            match self.peek_kind_at(offset) {
                Some(TokenKind::LParen) => depth += 1,
                Some(TokenKind::RParen) => {
                    depth -= 1;
                    if depth == 0 {
                        return self.peek_kind_at(offset + 1);
                    }
                }
                Some(_) => {}
                None => return None,
            }
            offset += 1;
        }
    }

    fn if_or_do_construct_starts_here(&self, keyword: &str) -> bool {
        if !self.ident_named(keyword) {
            return false;
        }
        if matches!(self.peek_kind_at(1), Some(TokenKind::LParen)) {
            return matches!(self.peek_after_paren_group_at(1), Some(TokenKind::LBrace));
        }
        !Self::token_is_statement_boundary(self.peek_kind_at(1))
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

    /// Returns true if the next operator run starts with the structural
    /// `[` required by a universal type binder. Brackets otherwise join
    /// ordinary maximal operator runs; only this binder context peels one.
    fn peek_starts_type_param(&self) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::SymbolRun(run)) if run.starts_with('['))
    }

    fn expect_type_param_open(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::SymbolRun(run)) if run == "[" => {
                self.advance();
                Ok(())
            }
            Some(TokenKind::SymbolRun(run)) if run.starts_with('[') => {
                self.split_current_sym(1);
                Ok(())
            }
            _ => self.err("expected `[` opening forall binder"),
        }
    }

    fn at_type_param_close(&self) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::SymbolRun(run)) if run.starts_with(']'))
    }

    fn expect_type_param_close(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::SymbolRun(run)) if run == "]" => {
                self.advance();
                Ok(())
            }
            Some(TokenKind::SymbolRun(run)) if run.starts_with(']') => {
                self.split_current_sym(1);
                Ok(())
            }
            _ => self.err("expected `]` closing forall binder"),
        }
    }

    fn lookahead_is_empty_or_value_group(&self) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::LParen))
            && !matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::SymbolRun(run)) if run.starts_with('[')
            )
    }

    /// Consume one universal type-binder group. `[A][B]` is canonical,
    /// while `[A, B]` is accepted as shorthand for the same ordered
    /// binder run. Leading `*` characters encode the binder's kind
    /// (`[*F]` ⇒ `*→*`, `[**G]` ⇒ `*→*→*`; see `specs/grammar.md`
    /// § Kind grammar). The verifier only checks acceptance, so it
    /// discards the star count.
    fn consume_type_param_binder_group(&mut self, what: &str) -> Result<(), ParseError> {
        self.expect_type_param_open()?;
        self.skip_commas();
        if self.at_type_param_close() {
            return self.err("type-parameter group cannot be empty");
        }
        while !self.at_type_param_close() {
            // A leading run of `*` kind-marker stars lexes as one
            // greedy `SymbolRun` (`*` / `**` / `***`); consume the
            // whole run before the binder name.
            self.consume_leading_stars();
            let (name, span) = self.expect_ident_with_span(what)?;
            Self::validate_type_name(&name, span)?;
            let saw_comma = self.skip_commas();
            if self.at_type_param_close() {
                break;
            }
            if !saw_comma {
                return self.err("expected `,` or `]` in type-parameter group");
            }
        }
        self.expect_type_param_close()?;
        Ok(())
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

    fn decl_modifiers(&mut self) -> Result<(bool, bool), ParseError> {
        let mut saw_vis = false;
        let mut saw_pure = false;
        loop {
            if self.ident_named("pub") {
                let span = self.tokens[self.pos].span;
                if saw_vis {
                    return self.err_at(span, "duplicate `pub` modifier");
                }
                saw_vis = true;
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
        Ok((saw_vis, saw_pure))
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

    fn expect_ident(&mut self, what: &str) -> Result<String, ParseError> {
        self.expect_ident_with_span(what).map(|(s, _)| s)
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

    /// Validate every segment of a path-shaped raw call argument against the
    /// exact spelling union admitted before its namespace/type/member role is
    /// established. This remains permissive about the role of each valid
    /// segment, preserving qualified type arguments (`m._Foo`) and value-member
    /// calls (`Formtype.member(...)`), but no declaration can make a spelling
    /// outside both user namespaces legal.
    fn validate_call_arg_path_names(&self) -> Result<(), ParseError> {
        let Some(Token {
            kind: TokenKind::Ident(head),
            span: head_span,
        }) = self.peek_token_at(0)
        else {
            return Ok(());
        };
        Self::validate_path_segment_name(head, *head_span)?;

        let mut offset = 0usize;
        while self
            .peek_kind_at(offset + 1)
            .is_some_and(|kind| kind.is_sym("."))
        {
            let Some(Token {
                kind: TokenKind::Ident(name),
                span,
            }) = self.peek_token_at(offset + 2)
            else {
                break;
            };
            Self::validate_path_segment_name(name, *span)?;
            offset += 2;
        }
        Ok(())
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
        self.expect_ident_named("module").map_err(|_| ParseError {
            offset: self.current_offset(),
            message: "expected `module` declaration at the start of the file".into(),
        })?;
        self.module_path()?;
        self.expect_kind(&TokenKind::Semicolon, "`;` after `module` path")?;

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
            self.expect_kind(&TokenKind::LParen, "`(` after the import provider")?;
            self.skip_commas();
            if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                return self.err("expected at least one imported item");
            }
            loop {
                if matches!(self.peek_kind(), Some(TokenKind::LBrace)) {
                    return self.err(
                        "braced label imports are surface syntax — Kio' import items are ordinary identifiers",
                    );
                }
                if (self.ident_named("op") || self.ident_named("varop"))
                    && !matches!(
                        self.peek_kind_at(1),
                        Some(TokenKind::Comma | TokenKind::RParen)
                    )
                {
                    return self.err("operator imports are surface syntax — not in Kio'");
                }
                let (name, span) = self.expect_ident_with_span("imported identifier")?;
                Self::validate_value_or_type_name(&name, span)?;
                let saw_comma = self.skip_commas();
                if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                    break;
                }
                if !saw_comma {
                    return self.err("expected `,` or `)` in the import list");
                }
            }
            self.expect_kind(&TokenKind::RParen, "`)` closing the import list")?;
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
        let (vis, pure) = self.decl_modifiers()?;
        if self.ident_named("host") && self.host_decl_follows() {
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
            if self.eat_ident_named("newtype") {
                self.newtype_after_keyword()?;
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
            } else if self.ident_named("type") {
                self.err("`rec type` is not a declaration form")
            } else if self.ident_named("labels") {
                self.err("`labels` is surface sugar — not in Kio'")
            } else {
                self.err("expected `newtype` or `{` after `rec` in Kio'")
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
            self.type_alias_after_keyword()
        } else if self.ident_named("newtype") {
            if pure {
                return self.err("`pure` is not valid on `newtype` declarations");
            }
            self.advance();
            self.newtype_after_keyword()?;
            self.optional_outer_semicolon();
            Ok(())
        } else if self.ident_named("literal") {
            self.err("`literal` aliases are Kio surface sugar — not in Kio'")
        } else if self.ident_named("labels") {
            self.err(
                "`labels` is surface sugar — not in Kio' (see specs/prime.md § What's not in Kio')",
            )
        } else if self.ident_named("if") {
            self.err("`if`/`else` is surface sugar — not in Kio'")
        } else if self.ident_named("match") {
            self.err("`match` is surface sugar — not in Kio'")
        } else if self.ident_named("op") {
            self.err(
                "`op` declarations are Kio surface sugar for user-defined operators — not in Kio'",
            )
        } else if self.ident_named("varop") {
            self.err("variadic operator declarations are Kio surface sugar — not in Kio'")
        } else if self.ident_named("elab") {
            self.err("`elab` declarations are not in Kio'")
        } else {
            self.err("expected `fn`, `type`, or `newtype`")
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
            let (_, pure) = self.decl_modifiers()?;
            if pure {
                return self.err(
                    "a bare `rec { ... }` type group admits only `type` and `newtype` declarations",
                );
            }
            if self.eat_ident_named("type") {
                self.type_alias_body()?;
            } else if self.eat_ident_named("newtype") {
                self.newtype_after_keyword()?;
            } else if self.ident_named("labels") {
                return self.err("`labels` is surface sugar — not in Kio'");
            } else if self.ident_named("rec") {
                return self.err("the enclosing `rec { ... }` already supplies recursive scope");
            } else {
                return self.err("expected `type` or `newtype` in bare `rec { ... }` type group");
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
    /// return type defaults to `()`. This is the one optional type
    /// position in the grammar.
    fn fn_def_after_keyword(&mut self) -> Result<(), ParseError> {
        let (name, span) = self.expect_ident_with_span("name after `fn`")?;
        Self::validate_value_name(&name, span)?;
        self.signature_groups(true)?;
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
        if self.peek_starts_type_param() {
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
    /// `IDENT TypeParamList? RoleAnnotation? OwnedBlock? ';'`.
    ///
    /// A host type is an opaque nominal type the host supplies; the
    /// optional `role(...)` annotation pins its literal role. The optional
    /// source-compatible `{ owned }` block selects no alternate facade.
    /// Neither annotation affects how references to the type parse, so the verifier
    /// accepts any role identifier (the closed role-kind set is a
    /// static check, not a grammar constraint).
    fn host_type_after_keyword(&mut self) -> Result<(), ParseError> {
        let (name, span) = self.expect_ident_with_span("name after `host type`")?;
        Self::validate_type_name(&name, span)?;
        if self.peek_starts_type_param() {
            self.type_param_list()?;
        }
        if self.eat_ident_named("role") {
            self.expect_kind(&TokenKind::LParen, "`(` opening `role` annotation")?;
            self.expect_ident("role name")?;
            self.expect_kind(&TokenKind::RParen, "`)` closing `role` annotation")?;
        }
        if matches!(self.peek_kind(), Some(TokenKind::LBrace)) {
            self.owned_block()?;
        }
        self.expect_kind(&TokenKind::Semicolon, "`;` ending `host type`")?;
        Ok(())
    }

    /// After consuming `host fn`:
    /// `IDENT SignatureGroups '->' Type ';'`.
    ///
    /// A host function is signature-only — the host supplies the body
    /// — so the declaration ends at `;` with no block. Value parameters
    /// carry the same named, annotated shape as ordinary function
    /// signatures (`name: Type`).
    fn host_fn_after_keyword(&mut self) -> Result<(), ParseError> {
        let (name, span) = self.expect_ident_with_span("name after `host fn`")?;
        Self::validate_value_name(&name, span)?;
        self.signature_groups(true)?;
        self.expect_arrow("`->` in `host fn`")?;
        self.type_()?;
        self.expect_kind(&TokenKind::Semicolon, "`;` ending `host fn`")?;
        Ok(())
    }

    /// `OwnedBlock ::= '{' 'owned' '}'`, the optional source-compatible
    /// host-type annotation that selects no alternate facade.
    fn owned_block(&mut self) -> Result<(), ParseError> {
        self.expect_kind(
            &TokenKind::LBrace,
            "`{` opening `host type` attribute block",
        )?;
        self.expect_ident_named("owned")?;
        self.expect_kind(
            &TokenKind::RBrace,
            "`}` closing `host type` attribute block",
        )?;
        Ok(())
    }

    /// After consuming `newtype`: `IDENT TypeParamList? ('<' IDENT '>')* ':' Type NewtypeBody`.
    ///
    /// Existential binders trail the universal-parameter list as
    /// zero-or-more whitespace-separated `<X>` atoms before the `:`.
    /// The payload is an ordinary type expression — it must not begin
    /// with `(<…>, …)`, since standalone existential type expressions
    /// are no longer admissible.
    fn newtype_after_keyword(&mut self) -> Result<(), ParseError> {
        let (name, span) = self.expect_ident_with_span("name after `newtype`")?;
        Self::validate_type_name(&name, span)?;
        if self.peek_starts_type_param() {
            self.type_param_list()?;
        }
        while self.at_sym("<") {
            self.consume_angle_binder("existential binder name on newtype header")?;
        }
        self.expect_sym(":", "`:` before `newtype` payload")?;
        self.type_()?;
        self.newtype_body()?;
        Ok(())
    }

    /// Newtype members are separated by semicolon runs; edge runs are allowed.
    /// The two members form an unordered set: exactly one `constructor`
    /// and exactly one `projector`, in either order. Both are required;
    /// neither may be duplicated.
    fn newtype_body(&mut self) -> Result<(), ParseError> {
        self.expect_kind(&TokenKind::LBrace, "`{` opening `newtype` body")?;
        let mut saw_constructor = false;
        let mut saw_projector = false;
        self.skip_semicolons();
        while !matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
            self.eat_vis()?;
            if self.eat_ident_named("constructor") {
                if saw_constructor {
                    return self.err("duplicate `constructor` member in newtype body");
                }
                saw_constructor = true;
            } else if self.eat_ident_named("projector") {
                if saw_projector {
                    return self.err("duplicate `projector` member in newtype body");
                }
                saw_projector = true;
            } else {
                break;
            }
            let (name, span) = self.expect_ident_with_span("member name")?;
            Self::validate_value_name(&name, span)?;
            let separated = self.skip_semicolons();
            if !separated && !matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
                return self.err("expected `;` or `}` after newtype member");
            }
        }
        if !saw_constructor {
            return self.err("missing `constructor` member in newtype body");
        }
        if !saw_projector {
            return self.err("missing `projector` member in newtype body");
        }
        self.expect_kind(&TokenKind::RBrace, "`}` closing `newtype` body")?;
        Ok(())
    }

    /// `TypeParamList ::= UniversalBinder*`
    fn type_param_list(&mut self) -> Result<(), ParseError> {
        while self.peek_starts_type_param() {
            self.consume_type_param_binder_group("type-parameter name")?;
        }
        Ok(())
    }

    fn signature_groups(&mut self, annotated: bool) -> Result<(), ParseError> {
        let mut saw_value_group = false;
        loop {
            if self.peek_starts_type_param() {
                self.type_param_list()?;
            } else if matches!(self.peek_kind(), Some(TokenKind::LParen)) {
                saw_value_group = true;
                self.value_signature_group(annotated)?;
            } else {
                break;
            }
        }
        if !saw_value_group {
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
        if self.peek_starts_type_param() {
            return self.err("type binders in signatures must be written before a value group");
        }
        let (name, span) = self.expect_ident_with_span("parameter name")?;
        Self::validate_value_binder_name(&name, span)?;
        self.expect_sym(":", "`:` between parameter name and type")?;
        self.type_()
    }

    fn fn_signature_value_param(&mut self) -> Result<(), ParseError> {
        if self.peek_starts_type_param() {
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
        while self.peek_starts_type_param() {
            self.type_param_list()?;
        }
        if self.at_arrow() {
            return self.err("type-binder run must be followed by a body type");
        }
        self.type_()
    }

    /// `Type ::= TypeAtom '->' Type | TypeChain`
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
        if self.peek_starts_type_param() {
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
        if self.at_arrow() {
            if used_chain {
                return self.err("product and sum types on the left of `->` must be parenthesized");
            }
            self.eat_arrow();
            self.type_()?;
        }
        Ok(())
    }

    /// True iff the next token can begin a `type_atom`. Mirrors
    /// `kio-rs/src/parser.rs::peek_starts_type_atom`.
    fn peek_starts_type_atom(&self) -> bool {
        self.at_unit_type_dot()
            || self.at_sym("!")
            || self.peek_starts_type_param()
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
            Some(_) if self.peek_starts_type_param() => self.type_signature_prefix(),
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
        if matches!(self.peek_kind(), Some(TokenKind::LParen)) {
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
        if self.peek_starts_type_param() {
            self.type_()?;
            self.expect_kind(&TokenKind::RParen, "`)` closing parenthesized type")?;
            if self.eat_arrow() {
                self.type_()?;
            }
            return Ok(());
        }

        // Standalone existential type expressions `(<U>, body)` are
        // no longer admissible — existentials are declared on a
        // `newtype` header.
        if self.at_sym("<") {
            return self.err(
                "standalone existential type expressions are no longer admissible; \
                 existentials live on a `newtype` header (`newtype Name[A] <U> : body`)",
            );
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
                self.type_()?;
            }
            return Ok(());
        }

        if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            self.advance();
            if self.eat_arrow() {
                self.type_()?;
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
                self.type_()?;
            }
            return Ok(());
        }
        if self.at_amp_chain_sep() {
            self.collect_type_chain_after_first('&')?;
            self.expect_kind(&TokenKind::RParen, "`)` closing parenthesised type")?;
            if self.eat_arrow() {
                self.type_()?;
            }
            return Ok(());
        }
        self.err("expected `)`, `|`, or `&` continuing the parenthesised type")
    }

    /// Consume one `<IDENT>` angle binder. Used by the existential
    /// binder run that trails a `newtype` declaration's universal-
    /// parameter list (e.g., `newtype Box[A] <U> : A & U`).
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

    /// `TypeArgList ::= '(' ','* Type (','+ Type)* ','* ')'`
    fn type_arg_list(&mut self) -> Result<(), ParseError> {
        self.expect_kind(&TokenKind::LParen, "`(`")?;
        self.skip_commas();
        if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            return self.err("type-argument list cannot be empty");
        }
        self.type_()?;
        loop {
            let saw_comma = self.skip_commas();
            if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                break;
            }
            if !saw_comma {
                return self.err("expected `,` or `)`");
            }
            self.type_()?;
        }
        self.expect_kind(&TokenKind::RParen, "`)` closing type-argument list")?;
        Ok(())
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
        // Surface-form rejection (better error message than the generic
        // "expected expression").
        if self.ident_named("if") && self.if_or_do_construct_starts_here("if") {
            return self.err("`if`/`else` is surface sugar — not in Kio'");
        }
        if self.ident_named("do") && self.if_or_do_construct_starts_here("do") {
            return self.err("`do` block is surface sugar — not in Kio'");
        }
        self.expr_postfix()
    }

    /// `ExprPostfix ::= ValuePath CallSuffix* | ExprAtom CallSuffix*`
    ///
    /// Dotted segments belong only to an identifier-led value path and are
    /// consumed before any call suffix. They are not a generic expression
    /// suffix: `(f).g` and `f().g` are outside both Kio and Kio'. A trailing
    /// `!` is caught explicitly as a Kio-only elaborator-form rejection.
    fn expr_postfix(&mut self) -> Result<(), ParseError> {
        let identifier_led = matches!(self.peek_kind(), Some(TokenKind::Ident(_)));
        self.expr_atom()?;
        if identifier_led {
            while self.peek_kind().is_some_and(|kind| kind.is_sym(".")) {
                self.advance();
                let (name, span) = self.expect_ident_with_span("identifier after `.`")?;
                Self::validate_path_segment_name(&name, span)?;
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
                Some(k) if k.is_sym("!") => {
                    return self
                        .err("`<name>!(…)` is Kio-surface elaborator-form syntax — not in Kio'");
                }
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
        Ok(())
    }

    /// `'(' ','* (CallArg (','+ CallArg)* ','*)? ')'` in suffix position.
    /// `CallArg ::= Expr | Type` — type arguments and value arguments
    /// share one positional list; the typer pairs them against the
    /// callee's signature.
    fn call_args(&mut self) -> Result<(), ParseError> {
        self.expect_kind(&TokenKind::LParen, "`(`")?;
        self.skip_commas();
        if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            self.advance();
            return Ok(());
        }
        self.call_arg()?;
        loop {
            let saw_comma = self.skip_commas();
            if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                break;
            }
            if !saw_comma {
                return self.err("expected `,` or `)`");
            }
            self.call_arg()?;
        }
        self.expect_kind(&TokenKind::RParen, "`)` closing call argument list")?;
        Ok(())
    }

    /// One call argument. Tries `Expr` first; if that fails — or
    /// succeeds but doesn't land on a list-continuation token (`,` /
    /// `)`) — backtracks and parses as `Type`. Slots whose surface
    /// spelling is admissible as both (bare identifier, parametric
    /// call) take the expr reading; type-only slots (`.`, `!`,
    /// paren-wrapped binary types, function types) fall through.
    fn call_arg(&mut self) -> Result<(), ParseError> {
        self.validate_call_arg_path_names()?;
        let saved = (self.pos, self.current_residual.clone());
        let expr_result = self.expr();
        if expr_result.is_ok()
            && matches!(self.peek_kind(), Some(TokenKind::Comma | TokenKind::RParen))
        {
            return Ok(());
        }
        (self.pos, self.current_residual) = saved;
        match self.type_() {
            Ok(()) => Ok(()),
            // Both arms rejected. Prefer the expr-side diagnostic —
            // the call-arg position reads as an expression, so its
            // message lines up with what the user wrote. (Matters
            // for `_`, which both arms reject with slightly
            // different wording.)
            Err(type_err) => Err(expr_result.err().unwrap_or(type_err)),
        }
    }

    fn expr_atom(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::IntLit | TokenKind::FloatLit) => {
                // A numeric literal. Kio' mandates the trailing
                // `(Type)` annotation (specs/grammar.md § Kio' grammar
                // — `LiteralCall ::= LITERAL '(' Type ')'`). The
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
                // trailing `(Type)` annotation is mandatory in Kio'.
                self.advance();
                self.skip_adjacent_str_lits();
                self.expect_literal_annotation("string")
            }
            Some(TokenKind::LParen) => self.paren_expr(),
            Some(TokenKind::SymbolRun(run)) if run == "." || run.starts_with(".[") => {
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
                        self.advance();
                        self.expect_literal_annotation("boolean")
                    }
                    Some(TokenKind::LParen) if self.lookahead_is_empty_or_value_group() => {
                        self.fn_after_dot()
                    }
                    Some(TokenKind::LParen) => self.err("expected `.t`, `.f`, or `.(...) { ... }`"),
                    _ if self.peek_starts_type_param() => self.fn_after_dot(),
                    _ => self.err("expected `.t`, `.f`, or `.(...) { ... }`"),
                }
            }
            Some(TokenKind::Ident(s)) if s == "_" => {
                // `_` in expression position is surface sugar for elided
                // type-arg slots at a polymorphic call (`__left__(_, _, x)`).
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

    /// Consume the trailing `'(' Type ')'` annotation of a
    /// `LiteralCall`. Kio' makes the annotation mandatory (see
    /// `specs/grammar.md` § Kio' grammar — `LiteralCall ::= LITERAL
    /// '(' Type ')'`); a bare literal is rejected here so the user
    /// sees a Kio'-specific diagnostic rather than the generic
    /// "expected `,` or `)`" from the surrounding call-args parser.
    fn expect_literal_annotation(&mut self, kind: &str) -> Result<(), ParseError> {
        if !matches!(self.peek_kind(), Some(TokenKind::LParen)) {
            return self.err(format!(
                "a {kind} literal must carry a `(Type)` annotation in Kio' \
                 (specs/grammar.md § Kio' grammar — `LiteralCall ::= LITERAL '(' Type ')'`); \
                 surface tier-2 / tier-3 resolution does not apply"
            ));
        }
        self.expect_kind(&TokenKind::LParen, "`(` opening literal annotation")?;
        self.type_()?;
        self.expect_kind(&TokenKind::RParen, "`)` closing literal annotation")?;
        Ok(())
    }

    /// `(  )` (unit), `( Expr )` (grouping). Comma-runs around zero
    /// or one expression collapse to the same shapes; `(a, b, …)`
    /// n-ary tuple literals are surface sugar and rejected.
    fn paren_expr(&mut self) -> Result<(), ParseError> {
        let open_span = self.tokens.get(self.pos).map(|t| t.span).unwrap_or(Span {
            start: self.src_len,
            end: self.src_len,
        });
        self.expect_kind(&TokenKind::LParen, "`(`")?;
        self.skip_commas();
        if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            self.advance();
            return Ok(());
        }
        let mut item_count = 1usize;
        self.expr()?;
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
        Ok(())
    }

    /// After consuming `.`: `SignatureGroups ('->' Type)? Block`
    fn fn_after_dot(&mut self) -> Result<(), ParseError> {
        self.signature_groups(false)?;
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
    fn empty_module_decl_only() {
        ok("module foo;");
    }

    #[test]
    fn rejects_env_block() {
        // The `env { … }` block is surface sugar that was removed; host
        // items are ordinary module items (`host type` / `host fn`).
        // See specs/prime.md § Host declarations — there is no `env` block.
        err("module foo; \
             env { type String role(str); type Box[A]; fn read(x: Box(String)) -> String; } \
             fn id(x: String) -> String { x }");
        err("module foo; \
             env { type Token; fn print(s: String) -> .; } \
             type String = .;");
    }

    #[test]
    fn host_items_as_module_items() {
        ok("module foo; \
             host type String role(str); host type Box[A]; \
             host fn read(x: Box(String)) -> String; \
             fn id(x: String) -> String { x }");
    }

    #[test]
    fn host_type_owned_block() {
        ok("module foo; host type Handle { owned }; type Alias = .;");
    }

    #[test]
    fn fn_def_id() {
        ok("module m; fn id[A](x: A) -> A { x }");
    }

    #[test]
    fn pure_fn_modifier() {
        ok("module m; pure fn id[A](x: A) -> A { x }");
    }

    #[test]
    fn pure_type_modifier_rejected() {
        let message = err("module m; pure type Pair[A][B] = (A & B);");
        assert!(
            message.contains("`pure`") && message.contains("`type`"),
            "{message}"
        );
    }

    #[test]
    fn pure_newtype_modifier_rejected() {
        let message = err(
            "module m; pure newtype Wrap[A] : A { pub constructor mk_wrap; pub projector un_wrap; };",
        );
        assert!(
            message.contains("`pure`") && message.contains("`newtype`"),
            "{message}"
        );
    }

    #[test]
    fn pure_and_visibility_modifiers_accept_either_order() {
        ok("module m; pub pure fn f() -> . { () }");
        ok("module m; pure pub fn f() -> . { () }");
        ok("module m; pub(scope/path) pure fn f() -> . { () }");
        ok("module m; pure pub(scope/path) fn f() -> . { () }");
    }

    #[test]
    fn duplicate_pure_modifier_rejected() {
        let message = err("module m; pure pure fn f() -> . { () }");
        assert!(message.contains("duplicate `pure` modifier"), "{message}");
    }

    #[test]
    fn pure_host_and_elaborator_declarations_rejected() {
        let host_fn = err("module m; pure host fn f() -> .;");
        assert!(
            host_fn.contains("`pure` means that a function does not call host functions"),
            "{host_fn}"
        );

        let host_type = err("module m; pure host type T;");
        assert!(
            host_type.contains("`pure` is valid only on ordinary `fn` declarations"),
            "{host_type}"
        );

        let elaborator = err("module m; pure elab e() -> . { () }");
        assert!(elaborator.contains("`elab`"), "{elaborator}");
    }

    #[test]
    fn rejects_angle_universal_binder() {
        // Universals are `[A]`-shaped; `<A>` is reserved for
        // existentials in their paren-wrapped position.
        let m = err("module m; fn id<A>(x: A) -> A { x }");
        assert!(
            m.contains("`[`")
                || m.contains("[type-param]")
                || m.contains("type-parameter")
                || m.contains("parameter name")
                || m.contains("explicit value group"),
            "msg = {m}"
        );
    }

    #[test]
    fn type_alias_bracket_binder() {
        ok("module m; type Pair[A][B] = (A & B);");
    }

    #[test]
    fn type_binder_name_class_matches_kio_parser() {
        for name in ["A", "_A", "_Foo_bar", "Foo123_bar4", "_Foo__"] {
            ok(&format!(
                "module m; fn id[{name}](x: {name}) -> {name} {{ x }}"
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
            let message = err(&format!("module m; fn id[{name}](x: .) -> . {{ x }}"));
            assert!(
                message.contains("must match _?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*"),
                "{message}"
            );
        }
        let reserved = err("module m; fn id[__Type__](x: .) -> . { x }");
        assert!(reserved.contains("may not begin with `__`"), "{reserved}");
    }

    #[test]
    fn type_declaration_heads_enforce_type_name_contract() {
        ok("module m; \
             host type Host; host type _Host; \
             type Alias = .; type _Alias = .; \
             newtype Box : . { constructor mk_box; projector un_box; }; \
             newtype _Box : . { constructor mk_marked; projector un_marked; };");

        for (name, source) in [
            ("_foo", "module m; type _foo = .;"),
            ("_1Foo", "module m; host type _1Foo;"),
            (
                "_FooBar",
                "module m; newtype _FooBar : . { constructor mk_box; projector un_box; };",
            ),
            ("__Foo", "module m; type __Foo = .;"),
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
        let msg = err("module m; type T = string;");
        assert!(msg.contains("type name `string`"), "msg = {msg}");
    }

    #[test]
    fn type_expr_reserved_internal_path_accepted() {
        ok("module m; type T = __Type__;");
    }

    #[test]
    fn type_expr_qualified_type_path_accepted() {
        ok("module m; type T = pkg/types.Box(A);");
    }

    #[test]
    fn newtype_bracket_binder() {
        ok("module m; newtype Wrap[A] : A { pub constructor mk_wrap; pub projector un_wrap; };");
    }

    #[test]
    fn function_type_bracket_binder() {
        ok("module m; type Id = [A] A -> A;");
    }

    #[test]
    fn parenthesized_grouped_function_type() {
        ok("module m; type Id = ([A] A -> A);");
        ok("module m; type Pair = (& ([A] A -> A) & (. -> .));");
    }

    #[test]
    fn comma_binder_group_accepts_shorthand() {
        ok("module m; type Id = [A, B] A -> B;");
    }

    #[test]
    fn binder_run_requires_body_type() {
        let m = err("module m; type F = [A] -> R;");
        assert!(
            m.contains("type-binder run must be followed by a body type"),
            "msg = {m}"
        );
    }

    #[test]
    fn comma_after_function_type_binder_rejected() {
        let m = err("module m; type Id = ([A], A) -> A;");
        assert!(m.contains("expected") || m.contains("grouped"), "msg = {m}");
    }

    #[test]
    fn comma_function_type_value_group_rejected() {
        let m = err("module m; type F = (A, B) -> C;");
        assert!(
            m.contains("commas are not product type syntax"),
            "msg = {m}"
        );
    }

    #[test]
    fn standalone_existential_type_rejected() {
        // Standalone existential type expressions no longer parse —
        // existentials live on a `newtype` header.
        let m = err("module m; type Pack[A] = (<U>, (A & U));");
        assert!(
            m.contains("standalone existential") || m.contains("newtype` header"),
            "msg = {m}"
        );
    }

    #[test]
    fn newtype_universal_with_header_existential() {
        // Universals in brackets, existentials trailing as `<X>` atoms.
        ok(
            "module m; newtype Pack[A] <U> : (A & U) { pub constructor mk_pack; pub projector un_pack; };",
        );
        ok(
            "module m; newtype _Pack[_A] <_Hidden> : (_A & _Hidden) { pub constructor mk_pack; pub projector un_pack; };",
        );
        let message = err(
            "module m; newtype Pack <_hidden> : . { pub constructor mk_pack; pub projector un_pack; };",
        );
        assert!(
            message.contains("must match _?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*"),
            "{message}"
        );
    }

    // ---- Greedy-fusion parsing ------------------------------------------
    //
    // The lexer fuses adjacent operator characters into one greedy
    // `SymbolRun` (matching the main kio-rs lexer); the parser peels a
    // structural delimiter (`->`, `>`, the unit `.`) off the front of a
    // fused run where the grammar needs it, and rejects fused spellings
    // that kio-rs also rejects (e.g. `type A=.;`, whose fused `=.` run
    // never yields the `=` the production needs).

    #[test]
    fn flush_equals_led_run_rejected() {
        // `A=.` fuses `=.` into one run, so the `=` the type-alias
        // production needs never appears — the same rejection the
        // kio-rs lexer's greedy fusion produces.
        assert!(
            err("module m; type A=.;").contains("expected `=`"),
            "fused `=.` never yields the `=` the type alias needs"
        );
        // The spaced spelling stays accepted.
        ok("module m; type A = .;");
    }

    #[test]
    fn fused_kind_star_run_accepted() {
        // `**` / `***` lex as one greedy all-stars run; the binder
        // consumes the run and reads the following name.
        ok("module m; type T[*F] = .;");
        ok("module m; type T[**G] = .;");
        ok("module m; type T[***H] = .;");
    }

    #[test]
    fn compact_forall_brackets_peel_from_maximal_runs() {
        ok("module m; fn higher[A][*F][**G](x: A) -> A { x }");
        ok("module m; type Nested = [A][B] A -> B;");
        ok("module m; fn lambda() -> . { .[*F](x: F(.)) -> F(.) { x } }");
    }

    #[test]
    fn malformed_or_nested_forall_binders_stay_rejected() {
        for source in [
            "module m; fn missing[A(x: A) -> A { x }",
            "module m; fn nameless[*](x: .) -> . { x }",
            "module m; fn nested[[A]](x: A) -> A { x }",
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
        ok("module m; newtype Single[A] <U>: (A & U) { pub constructor mk; pub projector un; };");
        ok(
            "module m; newtype Pair[A] <U><V>: (A & U & V) { pub constructor mk; pub projector un; };",
        );
    }

    #[test]
    fn fused_unit_dot_before_chain_op_peeled() {
        // `.&` / `.|` fuse the unit atom with the chain separator; the
        // parser peels the `.` — mirrors kio-rs's `at_unit_type_dot`.
        ok("module m; type P = (.& A);");
        ok("module m; type S = (.| A);");
        ok("module m; type P2 = (A &.);");
    }

    #[test]
    fn fused_arrow_after_unit_dot_peeled() {
        // `.->` fuses the unit domain with the function arrow; peeling
        // the `.` leaves `->` for the arrow consumer.
        ok("module m; type F = .-> .;");
    }

    #[test]
    fn fused_amp_run_still_one_separator() {
        // All-`&` / all-`|` runs collapse to one chain separator
        // whether spaced (`& & &`) or fused (`&&&`).
        ok("module m; type P = (A && B);");
        ok("module m; type P2 = (&&& A &&& B &&&);");
        ok("module m; type S = (A || B);");
    }

    #[test]
    fn fused_mixed_op_run_rejected() {
        // `&|` is neither an all-`&` nor an all-`|` run — mixing
        // still rejects, fused or spaced.
        let m = err("module m; type P = (A &| B);");
        assert!(m.contains("mixing") || m.contains("expected"), "msg = {m}");
    }

    #[test]
    fn host_type_and_fn_module_body_items() {
        // Opaque host declarations live in the module body: a `host
        // type` with a `role(...)` annotation and a signature-only
        // `host fn` whose value parameter references it.
        ok("module m; host type String role(str); host fn print(p0: String) -> .;");
    }

    #[test]
    fn host_type_generic_and_owned() {
        // A generic host type, a host type with the source-compatible
        // redundant `{ owned }` block, and a host fn taking a parametric host
        // type.
        ok("module m; \
             host type Box[T]; \
             host type File { owned }; \
             host fn read_string(x: Box(String)) -> String;");
    }

    #[test]
    fn host_items_accept_redundant_pub() {
        // Host items are always public; a `pub` before or after `host`
        // is admitted but redundant.
        ok("module m; pub host type Token;");
        ok("module m; host pub type Token;");
        ok("module m; pub host fn print(p0: String) -> .;");
        ok("module m; host pub fn print(p0: String) -> .;");
    }

    #[test]
    fn host_keyword_is_contextual() {
        // `host` is only the host-declaration modifier when followed by
        // `type` / `fn`. A function *named* `host` parses as an
        // ordinary definition, and `host` is usable as a value
        // identifier inside a body.
        ok("module m; fn host() -> . { () }");
        ok("module m; fn f() -> . { host } fn host() -> . { () }");
    }

    #[test]
    fn host_fn_requires_arrow() {
        // A `host fn` is signature-only and must declare its return
        // type with `->`; the body-bearing `{ ... }` shape is not a
        // host declaration.
        let m = err("module m; host fn print(p0: String) { () }");
        assert!(m.contains("`->`"), "msg = {m}");
    }

    #[test]
    fn host_fn_with_type_binder() {
        // Type-parameter binders precede the value group, as in any
        // function signature.
        ok("module m; host fn identity[A](x: A) -> A;");
    }

    #[test]
    fn rejects_mismatched_binder_brackets() {
        // `[A>` mismatches bracket opener with angle closer — the
        // closer expectation complains.
        let m = err("module m; fn id[A>(x: A) -> A { x }");
        assert!(m.contains("`]`"), "msg = {m}");
    }

    #[test]
    fn fn_def_unit_returns_unit() {
        ok("module m; fn main() -> . { () }");
    }

    #[test]
    fn fn_def_unit_return_elided() {
        // The `-> .` annotation may be omitted (one optional type
        // position; see specs/language.md § Function definitions).
        ok("module m; fn main() { () }");
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
        ok("module x; fn f() -> . { ;;; ();;; ();;; }");
    }

    #[test]
    fn final_semicolon_after_block_expression_is_ignored() {
        ok("module x; fn f() -> . { (); }");
    }

    #[test]
    fn empty_block_has_no_final_expression() {
        // A block with no final expression is a parse error — the `;`
        // runs never supply the mandatory trailing expression.
        assert!(
            err("module x; fn f() -> . { }").contains("no final expression"),
            "empty block rejected"
        );
        assert!(
            err("module x; fn f() -> . { ;; }").contains("no final expression"),
            "`;`-only block rejected"
        );
        assert!(
            err("module x; fn f() -> . { let x = (); }").contains("no final expression"),
            "block ending in a statement rejected"
        );
    }

    #[test]
    fn type_alias_with_params() {
        ok("module m; type Pair[A][B] = (A & B);");
    }

    #[test]
    fn label_forwarding_is_not_kio_prime_syntax() {
        for source in [
            "module m; type {forward} = {count};",
            "module m; pub type {forward} = {count};",
            "module m; pub(scope/path) type {forward} = {origin.count};",
            "module m; rec { type {forward} = {count}; type Alias = .; }",
        ] {
            let message = err(source);
            assert!(message.contains("name after `type`"), "{source}: {message}");
        }
        ok("module m; type Alias = .; pub(scope/path) type Scoped[A] = A;");
        ok("module m; fn type(forward: .) -> . { forward }");
    }

    #[test]
    fn rejects_literal_side_alias() {
        let m = err("module m; literal greeting = \"hi\";");
        assert!(m.contains("surface sugar"), "msg = {m}");
    }

    #[test]
    fn newtype_nullary() {
        ok("module m; newtype Wrap : . { pub constructor mk_wrap; pub projector un_wrap; };");
    }

    #[test]
    fn outer_braced_semicolons_are_optional_and_single() {
        for declaration in [
            "fn f() -> . { () }",
            "newtype Wrap : . { constructor wrap; projector unwrap }",
            "rec newtype Loop : . | Loop { constructor wrap; projector unwrap }",
            "rec { type A = B; type B = A }",
        ] {
            ok(&format!("module m; {declaration} fn next() -> . {{ () }}"));
            ok(&format!("module m; {declaration}; fn next() -> . {{ () }}"));
            err(&format!("module m; {declaration};;"));
        }
        for declaration in ["type A = .", "host type A { owned }", "host fn f() -> ."] {
            err(&format!("module m; {declaration}"));
            ok(&format!("module m; {declaration};"));
        }
        err("module m; ; fn f() -> . { () }");
    }

    #[test]
    fn newtype_semicolon_edges_preserve_member_policy() {
        for body in [
            "constructor wrap; projector unwrap",
            "; constructor wrap; projector unwrap;",
            ";; projector unwrap;; constructor wrap;;",
        ] {
            ok(&format!("module m; newtype Wrap : . {{ {body} }}"));
        }
        for body in [
            "constructor wrap projector unwrap",
            ";",
            "constructor wrap; constructor again; projector unwrap",
            "constructor wrap",
        ] {
            err(&format!("module m; newtype Wrap : . {{ {body} }}"));
        }
    }

    #[test]
    fn recursive_group_semicolons_belong_to_the_group() {
        for body in [
            "type A = B; type B = A",
            "; type A = B; type B = A;",
            "; newtype Box : A { constructor wrap; projector unwrap }; type A = Box;",
        ] {
            ok(&format!("module m; rec {{ {body} }}"));
        }
        for body in [
            "type A = B type B = A",
            "newtype Box : A { constructor wrap; projector unwrap } type A = Box",
            ";; type A = B; type B = A",
            "type A = B;; type B = A",
            "type A = B; type B = A;;",
            ";",
            "; type A = A;",
        ] {
            err(&format!("module m; rec {{ {body} }}"));
        }
    }

    #[test]
    fn newtype_recursive_self_ref() {
        ok(
            "module m; rec newtype List[A] : (. | (A & List(A))) { pub constructor cons; pub projector un_list; };",
        );
    }

    #[test]
    fn mutually_recursive_type_group_is_kio_prime_syntax() {
        ok("module m; rec { \
             type Loop = Box; \
             newtype Box : . | Loop { pub constructor mk_box; pub projector un_box; }; \
             }");
    }

    #[test]
    fn recursive_type_forms_keep_the_kio_prime_boundary_narrow() {
        assert!(err("module m; rec type Loop = Loop;").contains("not a declaration form"));
        assert!(err("module m; rec labels Loop = { next: Loop };").contains("surface sugar"));
        assert!(
            err("module m; pub rec { type A = B; type B = A; }").contains("no leading visibility")
        );
        assert!(
            err("module m; rec { fn f() -> . { () } type A = .; }")
                .contains("expected `type` or `newtype`")
        );
    }

    #[test]
    fn newtype_with_existential_binder() {
        ok(
            "module m; newtype Pack[A] <U> : (A & U) { pub constructor mk_pack; pub projector un_pack; };",
        );
    }

    #[test]
    fn newtype_with_multiple_existential_binders() {
        ok(
            "module m; newtype Pack[A] <L> <R> : (A & L & R) { pub constructor mk_pack; pub projector un_pack; };",
        );
    }

    #[test]
    fn newtype_with_existentials_no_universals() {
        ok("module m; newtype Pack <U> : U { pub constructor mk_pack; pub projector un_pack; };");
    }

    #[test]
    fn newtype_members_reversed_order() {
        ok("module m; newtype Wrap[A] : A { pub projector un_wrap; pub constructor mk_wrap; };");
    }

    #[test]
    fn newtype_duplicate_constructor_rejected() {
        let m = err(
            "module m; newtype W : . { pub constructor c1; pub constructor c2; pub projector p; };",
        );
        assert!(m.contains("duplicate `constructor`"), "got: {m}");
    }

    #[test]
    fn newtype_missing_projector_rejected() {
        let m = err("module m; newtype W : . { pub constructor c; };");
        assert!(m.contains("missing `projector`"), "got: {m}");
    }

    #[test]
    fn import_intrinsics() {
        ok("module m; import __intrinsics__;");
    }

    #[test]
    fn import_selection_requires_separators() {
        let m = err("module m; import provider(type String);");
        assert!(
            m.contains("expected `,` or `)` in the import list"),
            "got: {m}"
        );
    }

    #[test]
    fn import_qualified_with_alias() {
        ok("module m; import a/b as q;");
    }

    #[test]
    fn prime_only_internal_qualified_import_is_rejected() {
        for source in [
            "module m; import __internal__ a/b as __reserved__;",
            "module m; import __internal__ as q;",
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
        ok("module a/b/c; import x/y/z(f);");
    }

    #[test]
    fn braced_label_import_is_not_kio_prime() {
        for source in [
            "module m; import origin({field});",
            "module m; import origin(Field, {field});",
        ] {
            let message = err(source);
            assert!(message.contains("braced label imports are surface syntax"));
        }
    }

    #[test]
    fn import_selection_accepts_a1_layout_and_contextual_names() {
        for source in [
            "module m; import provider(foo);",
            "module m; import provider(\n  , Foo\n  , foo\n  );",
            "module m; import provider(,, Foo,, foo,,);",
            "module m; import provider(op, varop, variadic, import, type);",
            "module m; import provider(op);",
            "module m; import provider(\n  // first item\n  Foo // separator\n  , foo\n);",
        ] {
            ok(source);
        }
    }

    #[test]
    fn empty_or_incomplete_import_selection_is_rejected() {
        for source in [
            "module m; import provider();",
            "module m; import provider(,,);",
            "module m; import provider(",
            "module m; import provider(foo,",
            "module m; import provider(foo;",
            "module m; import provider(foo)",
            "module m; import provider;",
        ] {
            assert!(!err(source).is_empty(), "accepted `{source}`");
        }
    }

    #[test]
    fn surface_operator_imports_and_variadic_declarations_are_rejected() {
        for source in [
            "module m; import syntax(op _ <+> _);",
            "module m; import syntax(Foo, op _ ? _ : ___);",
            "module m; import syntax(varop [* *]);",
        ] {
            assert!(err(source).contains("operator imports are surface syntax"));
        }
        assert!(
            err("module m; pub varop [* *] { foldr1 step seed; };")
                .contains("operator declarations are Kio surface sugar")
        );
    }

    #[test]
    fn builtin_imports_keep_their_phase_and_statement_boundaries() {
        assert!(err("module m; import __comptime__;").contains("compile-time surface syntax"));
        for source in [
            "module m; import __intrinsics__(__pair__);",
            "module m; import __intrinsics__ as core;",
            "module m; fn f() -> . { () } import provider(foo);",
        ] {
            assert!(!err(source).is_empty(), "accepted `{source}`");
        }
    }

    #[test]
    fn doc_comment_cannot_attach_to_an_import() {
        for import in ["import __intrinsics__;", "import provider(foo);"] {
            let message = err(&format!("module m;\n/// Documentation\n{import}"));
            assert!(message.contains("`import` clauses are not documented"));
        }
        ok("module m; import provider(foo);\n/// Documentation\nfn f() -> . { () }");
        ok("module m; fn f(import: .) -> . {\n/// Parameter\nimport\n}");
        ok("module m; fn\n/// Name trivia\nimport() -> . { () }");
        ok("module\n/// Module-name trivia\nimport;");
    }

    #[test]
    fn match_identifier_is_not_a_surface_construct() {
        ok("module m; fn match() -> . { () } fn f() -> . { match() }");
    }

    #[test]
    fn rejects_if_else() {
        let m = err("module m; fn f() -> . { if x { y } else { z } }");
        assert!(m.contains("`if`"), "msg = {m}");
    }

    #[test]
    fn rejects_do_block() {
        // The `do` keyword in expression position trips the
        // surface-form rejection in `expr`. The body content is
        // irrelevant — the rejection fires at the `do` lookup
        // before we descend into the brace block. We use a body
        // that uses only Kio' grammar (no `<-`, which Kio''s
        // lexer doesn't recognize) so the error reaches `do`
        // rather than failing earlier in the lexer.
        let m = err("module m; fn f() -> . { do bind { () } }");
        assert!(m.contains("`do`"), "msg = {m}");
    }

    #[test]
    fn rejects_labels() {
        let m = err("module m; labels { f : . };");
        assert!(m.contains("labels"), "msg = {m}");
    }

    #[test]
    fn rejects_elaborator_bang_call() {
        // Any `<name>!(…)` shape — iso!, into!, onto!, match, etc.
        // — uses the elaborator, which Kio' doesn't have. The verifier
        // rejects all of them with one form-agnostic message.
        let m = err("module m; fn f() -> . { into!(x) }");
        assert!(m.contains("elaborator-form"), "msg = {m}");
    }

    #[test]
    fn dotted_value_paths_are_identifier_led_and_precede_calls() {
        ok("module m; fn f(x: .) -> . { module_alias.member(x) }");
        for source in [
            "module m; fn f() -> . { (value).member }",
            "module m; fn f() -> . { value().member }",
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
        ok("module m; type T = (A & B & C);");
    }

    #[test]
    fn accepts_chained_pipe() {
        ok("module m; type T = (A | B | C);");
    }

    #[test]
    fn accepts_leading_amp() {
        // Leading-operator multi-line layout (the formatter's
        // canonical shape for chains too long to fit on one line).
        ok("module m; type T = ( & A & B & C );");
    }

    #[test]
    fn accepts_repeated_amp_and_pipe() {
        ok("module m; type P = ( &&& A &&& B &&& );");
        ok("module m; type S = ( ||| A ||| B ||| );");
    }

    #[test]
    fn accepts_unary_and_empty_type_delimiter_runs() {
        ok("module m; type P = & A;");
        ok("module m; type S = | A;");
        ok("module m; type P = &;");
        ok("module m; type S = |;");
        ok("module m; type P = (&);");
        ok("module m; type S = (|);");
        ok("module m; type P = (A &);");
        ok("module m; type S = (A |);");
    }

    #[test]
    fn rejects_comma_product_type_delimiter_runs() {
        for source in [
            "module m; type P = (A, B);",
            "module m; type P = (, A ,);",
            "module m; type P = (,,,);",
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
        let m = err("module m; type T = (A & B | C);");
        assert!(m.contains("mixing"), "msg = {m}");
        let m = err("module m; type T = A & B | C;");
        assert!(m.contains("mixing"), "msg = {m}");
        let m = err("module m; type T = A | B & C;");
        assert!(m.contains("mixing"), "msg = {m}");
    }

    #[test]
    fn accepts_comma_framed_value_grouping() {
        ok("module m; fn f(x: A) -> A { (x,) }");
        ok("module m; fn f(x: A) -> A { (, x) }");
        ok("module m; fn f() -> . { (,,,) }");
    }

    #[test]
    fn rejects_n_ary_tuple_value() {
        let m = err("module m; fn f() -> . { (a, b) }");
        assert!(m.contains("n-ary tuple"), "msg = {m}");
    }

    #[test]
    fn bare_literal_rejected() {
        // Kio' mandates the `(Type)` annotation on every literal
        // (specs/grammar.md § Kio' grammar — `LiteralCall ::= LITERAL
        // '(' Type ')'`); the bare-literal form is surface-only.
        let m = err("module m; fn f() -> . { 42; () }");
        assert!(m.contains("annotation") || m.contains("Kio'"), "msg = {m}");
        let m = err("module m; fn f() -> . { 3.14; () }");
        assert!(m.contains("annotation") || m.contains("Kio'"), "msg = {m}");
        let m = err("module m; fn f() -> . { \"hi\"; () }");
        assert!(m.contains("annotation") || m.contains("Kio'"), "msg = {m}");
        let m = err("module m; fn f() -> . { .t; () }");
        assert!(m.contains("annotation") || m.contains("Kio'"), "msg = {m}");
    }

    #[test]
    fn literal_call_annotation_accepted() {
        // The trailing `(Type)` annotation form — `42(I32)`,
        // `"hi"(String)`, `.t(Bool)` — is mandatory on every Kio'
        // literal.
        ok("module m; fn f() -> . { 42(I32); () }");
        ok("module m; fn f() -> . { 3.14(F64); () }");
        ok("module m; fn f() -> . { \"hi\"(String); () }");
        ok("module m; fn f() -> . { .t(Bool); () }");
        ok("module m; fn f() -> . { .f(Bool); () }");
    }

    #[test]
    fn rejects_suffix_literal() {
        // The old lexer-side width suffix (`42i32`) is gone: the
        // digits lex as a bare literal and the `i32` as a trailing
        // identifier the parser can't place.
        err("module m; fn f() -> . { 42i32 }");
    }

    #[test]
    fn fn_without_annotations() {
        // `fn` value-params and return type may be left bare — the
        // type is filled by bidirectional flow from the use site.
        ok("module m; fn a() -> . { .(x) { x }(()) }");
    }

    #[test]
    fn fn_value_param_annotation_accepted() {
        // The `: T` value-parameter annotation is optional but
        // admissible — load-bearing for a `let`-bound lambda.
        ok("module m; fn a() -> . { .(x: .) { x }(()) }");
    }

    #[test]
    fn fn_return_annotation_accepted() {
        // The `-> R` return-type annotation is optional but admissible.
        ok("module m; fn a() -> . { .(x) -> . { x }(()) }");
    }

    #[test]
    fn let_binder_annotation_is_not_kio_prime_grammar() {
        let msg = err("module m; fn a() -> . { let y: . = (); y }");
        assert!(
            msg.contains("expected `}` closing block")
                || msg.contains("expected end of file")
                || msg.contains("expected expression"),
            "let binder annotation must fail by grammar mismatch, not an annotation-specific branch: {msg}"
        );
    }

    #[test]
    fn nested_function_type() {
        ok("module m; fn a(t: . -> .) -> . { t(()) }");
    }

    #[test]
    fn accepts_bare_unary_function_type() {
        ok("module m; type F = A -> B;");
        ok("module m; type F = A -> B -> C;");
        ok("module m; type F = A -> B & C;");
        ok("module m; type F = (A -> B);");
    }

    #[test]
    fn rejects_bare_product_or_sum_arrow_lhs() {
        let msg = err("module m; type F = A & B -> C;");
        assert!(
            msg.contains("left of `->`") || msg.contains("parenthesized"),
            "msg = {msg}"
        );
        let msg = err("module m; type F = A | B -> C;");
        assert!(
            msg.contains("left of `->`") || msg.contains("parenthesized"),
            "msg = {msg}"
        );
    }

    #[test]
    fn call_arg_accepts_compound_product_type() {
        // `(String & String)` at call-arg position is type-only — the
        // expr-first attempt fails on `&`, parser backtracks to type.
        ok("module m; import __intrinsics__; fn a(p: (. & .)) -> . { __snd__(., ., p) }");
    }

    #[test]
    fn call_arg_accepts_compound_sum_type() {
        ok(
            "module m; import __intrinsics__; fn a(s: (. | .)) -> . { __either__(., ., ., s, .(_l) { () }, .(_r) { () }) }",
        );
    }

    #[test]
    fn call_arg_accepts_function_type() {
        // `. -> .` at call-arg position is type-only.
        ok(
            "module m; import __intrinsics__; fn a() -> . { __pair__((. -> .), ., .(_x) { () }, ()) }",
        );
    }

    #[test]
    fn call_arg_accepts_bottom_type() {
        ok("module m; import __intrinsics__; fn a(b: !) -> . { __absurd__(., b) }");
    }

    #[test]
    fn call_arg_bare_identifier_still_works() {
        // The most common case: a bare type-arg like `String` at a
        // call-arg slot parses as Expr (single ident) and is
        // reinterpreted by the typer.
        ok("module m; fn a() -> . { f(A, b) }");
    }

    #[test]
    fn call_args_accept_exact_name_roles_only() {
        ok("module ok; fn id[A](x: A) -> A { x } \
             fn ordinary[A](value: A) -> A { id(A, value) } \
             fn marked[_A](_value: _A) -> _A { id(_A, _value) } \
             fn qualified[_A](_value: _A) -> _A { \
                 id(m._Foo, _value); \
                 let _ = id(_A, Formtype.member(_A, _value)); \
                 let _ = id(_A, m._value1.member(_value)); \
                 id(_A, m._Foo.member(_value)) \
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
                "module bad; fn id[A](x: A) -> A {{ x }} \
                 fn f[_A](x: .) -> . {{ id({argument}, x) }}"
            ));
            assert!(message.contains(invalid_name), "{argument}: {message}");
        }
    }

    #[test]
    fn expression_paths_accept_exact_name_roles_only() {
        ok("module ok; fn f[_A](_value: .) -> . { m._Foo.member(_A, _value) }");
        ok("module ok; fn f[_A](_value: .) -> . { __pair__(_A, _A, _value, _value) }");
        ok("module ok; fn f() -> . { __Foo.member(__value) }");
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
            let message = err(&format!("module bad; fn f() -> . {{ {expression} }}"));
            assert!(message.contains(invalid_name), "{expression}: {message}");
        }
    }

    #[test]
    fn letterless_reserved_shapes_are_not_names() {
        for name in ["__", "___", "____", "__1__"] {
            let expression = err(&format!("module bad; fn f() -> . {{ {name} }}"));
            assert!(!expression.is_empty(), "expression path accepted `{name}`");

            let ty = err(&format!("module bad; type T = {name};"));
            assert!(!ty.is_empty(), "type path accepted `{name}`");
        }

        ok(
            "module good; import __intrinsics__; type T = __Type__; fn f(x: ., y: .) -> . { __pair__(., ., x, y) }",
        );
    }

    #[test]
    fn selective_imports_accept_exact_name_roles_only() {
        ok("module m; import provider(Foo, foo, _Foo, _foo, _foo1); fn a() -> . { () }");
        for name in ["_FooBar", "_1Foo", "FooBar", "__Foo"] {
            let message = err(&format!(
                "module m; import provider({name}); fn a() -> . {{ () }}"
            ));
            assert!(message.contains(name), "{name}: {message}");
        }
    }

    #[test]
    fn marked_type_names_are_rejected_in_value_only_roles() {
        for source in [
            "module m; type T = m._Foo;",
            "module m; type T = m.Foo;",
            "module m; fn f(_: .) -> . { let _ = (); .(_: .) { () }(()) }",
        ] {
            ok(source);
        }
        for (role, invalid, valid) in [
            ("module head", "module _Foo;", "module _foo;"),
            ("module tail", "module m/_Foo;", "module m/_foo;"),
            (
                "qualified import source",
                "module m; import _Foo as dep; fn f() -> . { () }",
                "module m; import _foo as dep; fn f() -> . { () }",
            ),
            (
                "qualified import source tail",
                "module m; import provider/_Foo as dep; fn f() -> . { () }",
                "module m; import provider/_foo as dep; fn f() -> . { () }",
            ),
            (
                "qualified import alias",
                "module m; import provider as _Foo; fn f() -> . { () }",
                "module m; import provider as _foo; fn f() -> . { () }",
            ),
            (
                "function name",
                "module m; fn _Foo() -> . { () }",
                "module m; fn _foo() -> . { () }",
            ),
            (
                "host function name",
                "module m; host fn _Foo() -> .;",
                "module m; host fn _foo() -> .;",
            ),
            (
                "function parameter",
                "module m; fn f(_Foo: .) -> . { () }",
                "module m; fn f(_foo: .) -> . { () }",
            ),
            (
                "host function parameter",
                "module m; host fn f(_Foo: .) -> .;",
                "module m; host fn f(_foo: .) -> .;",
            ),
            (
                "lambda parameter",
                "module m; fn f() -> . { .(_Foo: .) { () } }",
                "module m; fn f() -> . { .(_foo: .) { () } }",
            ),
            (
                "let binder",
                "module m; fn f() -> . { let _Foo = (); () }",
                "module m; fn f() -> . { let _foo = (); () }",
            ),
            (
                "constructor",
                "module m; newtype Box : . { constructor _Foo; projector un_box; };",
                "module m; newtype Box : . { constructor _foo; projector un_box; };",
            ),
            (
                "projector",
                "module m; newtype Box : . { constructor mk_box; projector _Foo; };",
                "module m; newtype Box : . { constructor mk_box; projector _foo; };",
            ),
            (
                "type-path module qualifier",
                "module m; type T = _Foo.Bar;",
                "module m; type T = _foo.Bar;",
            ),
            (
                "type-path module qualifier tail",
                "module m; type T = provider/_Foo.Bar;",
                "module m; type T = provider/_foo.Bar;",
            ),
        ] {
            let message = err(invalid);
            assert!(message.contains("_Foo"), "{role}: {message}");
            ok(valid);
        }
    }

    #[test]
    fn slash_only_qualified_type_paths_are_rejected() {
        for source in ["module m; type T = m._Foo;", "module m; type T = m.Foo;"] {
            ok(source);
        }
        for source in ["module m; type T = m/_Foo;", "module m; type T = m/Foo;"] {
            let message = err(source);
            assert!(
                message.contains("qualified type paths use"),
                "{source}: {message}"
            );
        }
    }

    #[test]
    fn failed_dot_lambda_call_arg_does_not_forge_a_forall_type() {
        // The expr-first attempt peels `.` from the maximal `.[` run, then
        // rejects the malformed polymorphic lambda. Restoring the call-arg
        // checkpoint must restore that residual too: otherwise the type
        // fallback sees a forged `[A] A` and accepts a Prime-only spelling.
        let _message = err("module m; fn a() -> . { f(.[A] A) }");
    }

    #[test]
    fn function_type_leading_binder_run_accepted() {
        // `[A][B] T -> R` — all binders at the leading position,
        // then a value group.
        ok("module m; fn a(_f: [A][B] A -> B) -> . { () }");
    }

    #[test]
    fn function_return_forall_accepted() {
        // `P0 -> [A] P1 -> R` is `Function(P0, Forall(A,
        // Function(P1, R)))`: the arrow return can be any type,
        // including a `forall`.
        ok("module m; fn a(_f: A -> [B] B -> A) -> . { () }");
    }

    #[test]
    fn nested_function_return_forall_accepted() {
        ok("module m; fn a(_f: A -> B -> [C] C -> C) -> . { () }");
    }

    #[test]
    fn function_type_forall_value_group_domain_accepted() {
        // A value group's domain may itself be a forall (Rank-N):
        // `([U] U -> R) -> R`, the shape an existential newtype's
        // CPS projector takes as a continuation. `specs/prime.md`
        // § Kind grammar / § Mid-position binders admit Rank-N.
        ok("module m; fn a(_f: ([U] U -> .) -> .) -> . { () }");
    }

    #[test]
    fn function_type_existential_projector_value_accepted() {
        // The full first-class value type of an existential newtype's
        // CPS projector. This is what `recover_to_low` eta-expands a
        // value-position projector reference into, so the kio-prime
        // build-target round-trip must re-verify it.
        ok("module m; \
            newtype Pack <U> : . & U { constructor mk; projector get; }; \
            fn a(_f: Pack -> [R] ([U] (Pack & U) -> R) -> R) -> . { () }");
    }
}
