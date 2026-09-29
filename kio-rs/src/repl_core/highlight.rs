//! Kio-aware ANSI highlighting for the `kio repl` inspector.
//!
//! Every chunk of Kio the REPL prints — a synthesized type, an item
//! signature, a `kio fmt` source span, a code span inside a rendered
//! Kiodoc snippet — runs through [`highlight`]. The classifier is the
//! same lexer-driven token walk `kio debug tokens` and the LSP
//! semantic-tokens provider use ([`crate::tokens::dump`]): one
//! [`crate::tokens::TokenKind`] per token, mapped here to an
//! ANSI SGR colour.
//!
//! Highlighting is opt-in per call site via a [`Palette`]. The REPL
//! builds one with [`Palette::detect`] at startup: [`Palette::TrueColor`]
//! when the terminal advertises 24-bit colour via `$COLORTERM`,
//! [`Palette::Ansi`] for a colour TTY without that advertisement, and
//! [`Palette::Plain`] otherwise (non-TTY output — a pipe, a file, a CI
//! capture — `$NO_COLOR`, or a dumb terminal). A plain palette emits
//! the input verbatim, so call sites never branch on TTY-ness.
//!
//! Two colour palettes, one classifier. [`Palette::Ansi`] keeps the
//! original small 8-colour mapping (legible on any colour terminal,
//! the universal fallback). [`Palette::TrueColor`] un-collapses the
//! classifier's finer kinds — declaration / control / elaborator keywords
//! get distinct hues, type names diverge from module paths, bracket
//! and separator punctuation is de-emphasised rather than left
//! uncoloured — using a One-Dark-derived 24-bit palette plus italic /
//! underline attributes. Both share [`Palette::style_for_kind`], the
//! single kind → SGR source the printed-output [`highlight`] and the
//! REPL's live input-line highlighter both consult.

use crate::tokens::{ClassifiedToken, TokenKind, dump};

/// ANSI SGR (Select Graphic Rendition) reset sequence. Closes every
/// open colour / attribute run opened by a [`Palette`] escape; the
/// two-line REPL prompt closes its styled context line with it so the
/// input line inherits no residual colour.
pub const RESET: &str = "\x1b[0m";

/// The dim (faint) SGR attribute — `\x1b[2m`. The two-line prompt's
/// "no module loaded" hint renders in it (de-emphasised guidance, not
/// content), the same faint attribute the classifier gives comments.
/// Closed by [`RESET`] at the call site.
pub const DIM: &str = "\x1b[2m";

/// The bracket-match emphasis style: the matched pair, both halves,
/// rendered in bold gold so the structure pops while the cursor sits
/// on one of them. Used by the live input-line highlighter.
pub const BRACKET_MATCH: &str = "\x1b[1m\x1b[38;2;229;192;123m";

/// The bracket-mismatch warning style: the cursor sits on a bracket
/// with no matching partner (unbalanced input mid-edit), rendered in
/// red so the imbalance is visible. Used by the live input-line
/// highlighter.
pub const BRACKET_MISMATCH: &str = "\x1b[1m\x1b[38;2;224;108;117m";

/// A highlighting palette: a colour mapping, or the plain (no-op)
/// palette.
///
/// `kio repl` picks the variant with [`Palette::detect`]. The plain
/// palette makes [`highlight`] an identity function, so call sites
/// never branch on TTY-ness themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// Emit 24-bit truecolor SGR escapes (`\x1b[38;2;R;G;Bm`) plus
    /// italic / underline attributes around each classified token.
    /// Chosen when the terminal advertises truecolor via `$COLORTERM`.
    TrueColor,
    /// Emit the standard 8-colour ANSI SGR escapes around each
    /// classified token. The fallback for a colour TTY that does not
    /// advertise truecolor.
    Ansi,
    /// Emit the input verbatim — no escapes. Used for non-TTY output
    /// and dumb terminals.
    Plain,
}

impl Palette {
    /// The truecolor palette.
    pub fn truecolor() -> Self {
        Palette::TrueColor
    }

    /// The ANSI-colour palette.
    pub fn ansi() -> Self {
        Palette::Ansi
    }

    /// The plain (no-colour) palette.
    pub fn plain() -> Self {
        Palette::Plain
    }

    /// Detect the right palette for the current process.
    ///
    /// - [`Palette::Plain`] when `stdout` is not a terminal, when
    ///   `$NO_COLOR` is set to a non-empty value (the `NO_COLOR`
    ///   convention), or when `$TERM` is `dumb`.
    /// - [`Palette::TrueColor`] on a colour TTY whose `$COLORTERM`
    ///   advertises 24-bit colour (`truecolor` or `24bit`).
    /// - [`Palette::Ansi`] on any other colour TTY.
    ///
    /// Truecolor detection stays strict on `$COLORTERM`: a
    /// false-negative degrades gracefully to the 8-colour palette,
    /// whereas a false-positive would emit truecolor escapes a real
    /// 8-colour terminal renders as garbage.
    pub fn detect() -> Self {
        Self::classify_env(
            stdout_is_terminal(),
            std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false),
            terminal_advertises_truecolor(),
        )
    }

    /// The pure palette decision behind [`Palette::detect`], factored
    /// out so it can be tested without a real TTY or mutating process
    /// environment. `is_terminal` is stdout's TTY-ness, `no_color` the
    /// `$NO_COLOR` convention, `dumb` whether `$TERM` is `dumb`, and
    /// `truecolor` whether `$COLORTERM` advertised 24-bit colour.
    fn classify_env(is_terminal: bool, no_color: bool, dumb: bool, truecolor: bool) -> Self {
        if no_color || dumb || !is_terminal {
            return Palette::Plain;
        }
        if truecolor {
            Palette::TrueColor
        } else {
            Palette::Ansi
        }
    }

    /// The full SGR escape sequence for a [`TokenKind`] under this
    /// palette — colour plus any attribute codes (italic, underline)
    /// — or `None` for kinds left uncoloured.
    ///
    /// This is the single source of truth for the kind → SGR mapping:
    /// the printed-output [`highlight`] and the REPL's live input-line
    /// highlighter both call it, so the two never drift. The returned
    /// sequence is closed by [`RESET`] at each call site.
    ///
    /// `None` covers two cases. [`Palette::Plain`] returns `None` for
    /// every kind (highlighting is a no-op). The colour palettes
    /// return `None` for the kinds they leave at the terminal's
    /// default foreground — plain identifiers, slots,
    /// and (under [`Palette::Ansi`] only) bracket / separator
    /// punctuation, matching the semantic-tokens provider's "not a
    /// semantic token" set.
    pub fn style_for_kind(self, kind: TokenKind) -> Option<&'static str> {
        match self {
            Palette::Plain => None,
            Palette::Ansi => Self::ansi_style(kind),
            Palette::TrueColor => Self::truecolor_style(kind),
        }
    }

    /// The kind → [`nu_ansi_term::Style`] mapping for reedline's live
    /// input-line highlighter, the structured-segment analogue of
    /// [`style_for_kind`](Self::style_for_kind).
    ///
    /// reedline's [`Highlighter`](reedline::Highlighter) returns a
    /// [`StyledText`](reedline::StyledText) — a list of
    /// `(nu_ansi_term::Style, String)` segments — rather than a string
    /// of embedded SGR escapes. The printed-output path keeps emitting
    /// SGR escapes via [`style_for_kind`](Self::style_for_kind); this
    /// method mirrors the *same colour decisions* in `Style` form so
    /// the two surfaces stay in lockstep. The
    /// `reedline_style_agrees_with_sgr_on_which_kinds_colour` test
    /// pins that agreement: a kind is `None` here exactly when it is
    /// `None` there.
    ///
    /// `None` means "leave at the terminal default" — the same kinds
    /// [`style_for_kind`](Self::style_for_kind) leaves uncoloured.
    #[cfg(feature = "repl")]
    pub fn style_for_kind_reedline(self, kind: TokenKind) -> Option<nu_ansi_term::Style> {
        match self {
            Palette::Plain => None,
            Palette::Ansi => Self::ansi_reedline_style(kind),
            Palette::TrueColor => Self::truecolor_reedline_style(kind),
        }
    }

    /// The 8-colour reedline `Style` mapping — the structured analogue
    /// of [`ansi_style`](Self::ansi_style). Same hue choices, expressed
    /// as `nu_ansi_term` 8-colour names rather than SGR codes.
    #[cfg(feature = "repl")]
    fn ansi_reedline_style(kind: TokenKind) -> Option<nu_ansi_term::Style> {
        use nu_ansi_term::{Color, Style};
        let style = match kind {
            // Keywords + boolean literals — magenta.
            TokenKind::KeywordControl
            | TokenKind::KeywordDeclaration
            | TokenKind::KeywordElaborator
            | TokenKind::LiteralBool => Style::new().fg(Color::Magenta),
            // Comments — dim.
            TokenKind::CommentLine | TokenKind::CommentDoc => Style::new().dimmed(),
            // Strings — green.
            TokenKind::LiteralString => Style::new().fg(Color::Green),
            // Numbers — cyan.
            TokenKind::LiteralNumber => Style::new().fg(Color::Cyan),
            // Operators — yellow.
            TokenKind::OperatorBuiltin | TokenKind::OperatorUser => Style::new().fg(Color::Yellow),
            // Function definition / call names — blue.
            TokenKind::EntityNameFunction | TokenKind::EntityNameFunctionReference => {
                Style::new().fg(Color::Blue)
            }
            // Type definition names — cyan.
            TokenKind::EntityNameType => Style::new().fg(Color::Cyan),
            // Module path segments — cyan.
            TokenKind::EntityNameModule => Style::new().fg(Color::Cyan),
            // Label names — yellow.
            TokenKind::EntityNameLabel
            | TokenKind::EntityNameLabelReference
            | TokenKind::EntityNameQualifiedLabelReference => Style::new().fg(Color::Yellow),
            // Parameter binders — red.
            TokenKind::VariableParameter => Style::new().fg(Color::Red),
            // Plain identifiers and the unclassified punctuation set
            // stay uncoloured.
            TokenKind::Identifier
            | TokenKind::PunctuationBracket
            | TokenKind::PunctuationSeparator
            | TokenKind::Slot => return None,
        };
        Some(style)
    }

    /// The 24-bit reedline `Style` mapping — the structured analogue of
    /// [`truecolor_style`](Self::truecolor_style). The same
    /// One-Dark-derived RGB hues and italic / underline attributes,
    /// expressed as [`nu_ansi_term::Color::Rgb`] + `.italic()` /
    /// `.underline()` rather than SGR escape strings.
    #[cfg(feature = "repl")]
    fn truecolor_reedline_style(kind: TokenKind) -> Option<nu_ansi_term::Style> {
        use nu_ansi_term::{Color, Style};
        let style = match kind {
            // Control keywords — One-Dark purple.
            TokenKind::KeywordControl => Style::new().fg(Color::Rgb(198, 120, 221)),
            // Declaration keywords — gold.
            TokenKind::KeywordDeclaration => Style::new().fg(Color::Rgb(229, 192, 123)),
            // Elaborator keywords — teal.
            TokenKind::KeywordElaborator => Style::new().fg(Color::Rgb(86, 182, 194)),
            // Boolean literals — orange.
            TokenKind::LiteralBool => Style::new().fg(Color::Rgb(209, 154, 102)),
            // String literals — green.
            TokenKind::LiteralString => Style::new().fg(Color::Rgb(152, 195, 121)),
            // Number literals — orange.
            TokenKind::LiteralNumber => Style::new().fg(Color::Rgb(209, 154, 102)),
            // Comments — comment-grey, italic.
            TokenKind::CommentLine | TokenKind::CommentDoc => {
                Style::new().fg(Color::Rgb(92, 99, 112)).italic()
            }
            // Operators — coral.
            TokenKind::OperatorBuiltin | TokenKind::OperatorUser => {
                Style::new().fg(Color::Rgb(224, 108, 117))
            }
            // Function definition / call names — blue.
            TokenKind::EntityNameFunction | TokenKind::EntityNameFunctionReference => {
                Style::new().fg(Color::Rgb(97, 175, 239))
            }
            // Type definition names — teal.
            TokenKind::EntityNameType => Style::new().fg(Color::Rgb(86, 182, 194)),
            // Module path segments — foreground-grey, underline.
            TokenKind::EntityNameModule => Style::new().fg(Color::Rgb(171, 178, 191)).underline(),
            // Label names — gold.
            TokenKind::EntityNameLabel
            | TokenKind::EntityNameLabelReference
            | TokenKind::EntityNameQualifiedLabelReference => {
                Style::new().fg(Color::Rgb(229, 192, 123))
            }
            // Parameter binders — coral, italic.
            TokenKind::VariableParameter => Style::new().fg(Color::Rgb(224, 108, 117)).italic(),
            // Bracket / separator punctuation — de-emphasised comment-grey.
            TokenKind::PunctuationBracket | TokenKind::PunctuationSeparator => {
                Style::new().fg(Color::Rgb(92, 99, 112))
            }
            // Plain identifiers and slots stay at the
            // terminal default.
            TokenKind::Identifier | TokenKind::Slot => return None,
        };
        Some(style)
    }

    /// The 8-colour mapping. Chosen to be legible on both dark and
    /// light terminals (the standard 8-colour palette, no bright
    /// codes). Kept as-is as the truecolor-unsupported fallback.
    fn ansi_style(kind: TokenKind) -> Option<&'static str> {
        let code = match kind {
            // Keywords + boolean literals — magenta.
            TokenKind::KeywordControl
            | TokenKind::KeywordDeclaration
            | TokenKind::KeywordElaborator
            | TokenKind::LiteralBool => "\x1b[35m",
            // Comments — dim.
            TokenKind::CommentLine | TokenKind::CommentDoc => "\x1b[2m",
            // Strings — green.
            TokenKind::LiteralString => "\x1b[32m",
            // Numbers — cyan.
            TokenKind::LiteralNumber => "\x1b[36m",
            // Operators — yellow.
            TokenKind::OperatorBuiltin | TokenKind::OperatorUser => "\x1b[33m",
            // Function definition / call names — blue.
            TokenKind::EntityNameFunction | TokenKind::EntityNameFunctionReference => "\x1b[34m",
            // Type definition names — cyan.
            TokenKind::EntityNameType => "\x1b[36m",
            // Module path segments — cyan.
            TokenKind::EntityNameModule => "\x1b[36m",
            // Label names — yellow.
            TokenKind::EntityNameLabel
            | TokenKind::EntityNameLabelReference
            | TokenKind::EntityNameQualifiedLabelReference => "\x1b[33m",
            // Parameter binders — red.
            TokenKind::VariableParameter => "\x1b[31m",
            // Plain identifiers and the unclassified punctuation set
            // stay uncoloured.
            TokenKind::Identifier
            | TokenKind::PunctuationBracket
            | TokenKind::PunctuationSeparator
            | TokenKind::Slot => return None,
        };
        Some(code)
    }

    /// The 24-bit mapping — a One-Dark-derived hue set that
    /// un-collapses the classifier's finer kinds. Each entry is a
    /// pre-composed `\x1b[38;2;R;G;Bm` colour escape, optionally
    /// preceded by an attribute escape (`\x1b[3m` italic, `\x1b[4m`
    /// underline); the trailing [`RESET`] at the call site closes both.
    fn truecolor_style(kind: TokenKind) -> Option<&'static str> {
        let code = match kind {
            // Control keywords (`if` / `else` / `do`) —
            // One-Dark purple.
            TokenKind::KeywordControl => "\x1b[38;2;198;120;221m",
            // Declaration keywords (`fn` / `module` / `pub` / …) —
            // gold, distinct from control flow.
            TokenKind::KeywordDeclaration => "\x1b[38;2;229;192;123m",
            // Elaborator keywords (`iso!` / `reorder_*` / `fit!` / …) —
            // teal, distinct again.
            TokenKind::KeywordElaborator => "\x1b[38;2;86;182;194m",
            // Boolean literals — orange.
            TokenKind::LiteralBool => "\x1b[38;2;209;154;102m",
            // String literals — green.
            TokenKind::LiteralString => "\x1b[38;2;152;195;121m",
            // Number literals — orange.
            TokenKind::LiteralNumber => "\x1b[38;2;209;154;102m",
            // Comments — comment-grey, italic.
            TokenKind::CommentLine | TokenKind::CommentDoc => "\x1b[3m\x1b[38;2;92;99;112m",
            // Operators — coral.
            TokenKind::OperatorBuiltin | TokenKind::OperatorUser => "\x1b[38;2;224;108;117m",
            // Function definition / call names — blue.
            TokenKind::EntityNameFunction | TokenKind::EntityNameFunctionReference => {
                "\x1b[38;2;97;175;239m"
            }
            // Type definition names — teal (paired with elaborator kinds,
            // both type-flavoured).
            TokenKind::EntityNameType => "\x1b[38;2;86;182;194m",
            // Module path segments — foreground-grey, underline, so a
            // dotted path reads as a path without competing with the
            // names around it.
            TokenKind::EntityNameModule => "\x1b[4m\x1b[38;2;171;178;191m",
            // Label names — gold.
            TokenKind::EntityNameLabel
            | TokenKind::EntityNameLabelReference
            | TokenKind::EntityNameQualifiedLabelReference => "\x1b[38;2;229;192;123m",
            // Parameter binders — coral, italic.
            TokenKind::VariableParameter => "\x1b[3m\x1b[38;2;224;108;117m",
            // Bracket / separator punctuation — de-emphasised in the
            // comment-grey rather than left at the default foreground,
            // so structure recedes visually.
            TokenKind::PunctuationBracket | TokenKind::PunctuationSeparator => {
                "\x1b[38;2;92;99;112m"
            }
            // Plain identifiers and slots stay at the
            // terminal default.
            TokenKind::Identifier | TokenKind::Slot => return None,
        };
        Some(code)
    }
}

/// Highlight a chunk of Kio source for terminal display.
///
/// Runs the [`crate::tokens`] classifier over `src` and wraps
/// each classified token in an ANSI colour escape per `palette`.
/// Bytes between tokens (whitespace, uncoloured punctuation) are
/// emitted verbatim. When `palette` is [`Palette::Plain`], or when
/// the lexer rejects `src` outright (so no token stream is
/// available), the input is returned unchanged — highlighting is
/// always a presentational nicety, never load-bearing.
pub fn highlight(src: &str, palette: Palette) -> String {
    render(src, palette, &BracketEmphasis::none())
}

/// Highlight a standalone type rendered by the REPL. The explicit type entry
/// point lets the parser classify forall delimiters without guessing whether
/// an arbitrary bracket-bearing snippet is a type or an ordinary operator
/// expression.
pub fn highlight_type(src: &str, palette: Palette) -> String {
    if palette == Palette::Plain {
        return src.to_owned();
    }
    let Ok(tokens) = crate::tokens::dump_type_fragment(src) else {
        return src.to_owned();
    };
    render_tokens(src, palette, &BracketEmphasis::none(), &tokens)
}

/// Render `text` in the dim (faint) attribute — de-emphasised guidance,
/// not content (a self-advertising footer, a discoverability trailer).
/// Under [`Palette::Plain`] the text is returned bare; under any colour
/// palette it is wrapped in [`DIM`] … [`RESET`], the same faint
/// attribute the two-line prompt's no-module hint uses.
pub fn dim(text: &str, palette: Palette) -> String {
    match palette {
        Palette::Plain => text.to_owned(),
        _ => format!("{DIM}{text}{RESET}"),
    }
}

/// Highlight the live input line at the prompt for terminal display.
///
/// Same token colouring as [`highlight`], plus a **bracket-match
/// overlay**: when the cursor at byte offset `pos` sits on a paired
/// bracket (`(` `)` `[` `]` `{` `}`), that bracket and its match
/// render in [`BRACKET_MATCH`] (bold gold). When the bracket under
/// the cursor has no partner — unbalanced input mid-edit — only that
/// bracket renders, in [`BRACKET_MISMATCH`] (bold red), surfacing the
/// imbalance. With the cursor off any bracket the result is identical
/// to [`highlight`].
///
/// As with [`highlight`], a [`Palette::Plain`] palette or a lexer
/// rejection returns the input unchanged.
pub fn highlight_input(src: &str, pos: usize, palette: Palette) -> String {
    if palette == Palette::Plain {
        return src.to_owned();
    }
    let Ok(tokens) = repl_display_tokens(src, None) else {
        return src.to_owned();
    };
    let emphasis = bracket_emphasis_at_tokens(src, pos, &tokens);
    render_tokens(src, palette, &emphasis, &tokens)
}

fn display_tokens(src: &str) -> Result<Vec<ClassifiedToken>, crate::error::Error> {
    dump(src)
}

fn repl_display_tokens(
    src: &str,
    context: Option<&crate::pass::parser::ExpressionParseContext>,
) -> Result<Vec<ClassifiedToken>, crate::error::Error> {
    if let Some(expression_start) = super::completion::expression_input_start(src) {
        return crate::tokens::dump_repl_input(src, expression_start, context);
    }
    dump(src)
}

/// Whether a [`BracketEmphasis`] mark style is the match style (bold
/// gold) rather than the mismatch style (bold red). The reedline
/// styled-segment path keys on this to pick the right
/// [`nu_ansi_term::Style`]. Compared by value — the two emphasis
/// constants are distinct strings.
#[cfg(feature = "repl")]
fn mark_is_match(style: &str) -> bool {
    style == BRACKET_MATCH
}

/// The reedline `Style` for a bracket the cursor emphasises: bold gold
/// for a matched pair, bold red for an unmatched bracket — the
/// structured analogue of [`BRACKET_MATCH`] / [`BRACKET_MISMATCH`].
#[cfg(feature = "repl")]
fn bracket_emphasis_style(is_match: bool) -> nu_ansi_term::Style {
    use nu_ansi_term::{Color, Style};
    if is_match {
        Style::new().fg(Color::Rgb(229, 192, 123)).bold()
    } else {
        Style::new().fg(Color::Rgb(224, 108, 117)).bold()
    }
}

/// Highlight the live input line as a reedline
/// [`StyledText`](reedline::StyledText) — the structured-segment
/// analogue of [`highlight_input`], which returns embedded SGR escapes.
///
/// reedline's [`Highlighter`](reedline::Highlighter) consumes a
/// `StyledText` (a list of `(nu_ansi_term::Style, String)` segments)
/// rather than a string with escapes baked in. This walks the same
/// classifier token stream [`render`] uses and emits one styled
/// segment per run, applying the same bracket-match overlay
/// [`highlight_input`] does. A [`Palette::Plain`] palette, or a lexer
/// rejection, yields a single unstyled segment carrying the raw line —
/// the structured mirror of the "return the input unchanged"
/// behaviour.
#[cfg(feature = "repl")]
pub fn styled_input(src: &str, pos: usize, palette: Palette) -> reedline::StyledText {
    styled_input_with_context(src, pos, palette, None)
}

#[cfg(feature = "repl")]
pub(crate) fn styled_input_with_context(
    src: &str,
    pos: usize,
    palette: Palette,
    context: Option<&crate::pass::parser::ExpressionParseContext>,
) -> reedline::StyledText {
    use nu_ansi_term::Style;
    use reedline::StyledText;

    let mut styled = StyledText::new();
    if palette == Palette::Plain {
        styled.push((Style::new(), src.to_owned()));
        return styled;
    }
    let tokens = match repl_display_tokens(src, context) {
        Ok(t) => t,
        // Lexing failed — emit the raw text as one unstyled segment.
        Err(_) => {
            styled.push((Style::new(), src.to_owned()));
            return styled;
        }
    };
    let emphasis = bracket_emphasis_at_tokens(src, pos, &tokens);
    let bytes = src.as_bytes();
    let mut cursor: usize = 0;
    for tok in tokens {
        let start = tok.span.start as usize;
        let end = tok.span.end as usize;
        // Same defensive span guard as `render`: skip a malformed
        // token rather than panicking on a bad classifier span.
        if start > end || end > bytes.len() || start < cursor {
            continue;
        }
        // Gap before this token — whitespace / uncoloured punctuation —
        // emitted as an unstyled segment so the line round-trips.
        if start > cursor {
            styled.push((Style::new(), src[cursor..start].to_owned()));
        }
        let slice = src[start..end].to_owned();
        // A bracket the cursor emphasises overrides its palette colour;
        // otherwise fall back to the kind's reedline style.
        let style = match emphasis.style_at(start) {
            Some(mark) => bracket_emphasis_style(mark_is_match(mark)),
            None => palette
                .style_for_kind_reedline(tok.kind)
                .unwrap_or_default(),
        };
        styled.push((style, slice));
        cursor = end;
    }
    if cursor < src.len() {
        styled.push((Style::new(), src[cursor..].to_owned()));
    }
    styled
}

/// The bracket-emphasis overlay for the live input highlighter: the
/// byte offsets to wrap in a brighter style, and which style.
///
/// At most two offsets are ever set — the bracket under the cursor
/// and its match (or just the cursor's bracket, for a mismatch). The
/// offsets index the start byte of a one-byte bracket character.
struct BracketEmphasis {
    /// `(offset, style)` pairs keyed by bracket start byte.
    marks: Vec<(usize, &'static str)>,
}

impl BracketEmphasis {
    /// No bracket emphasis — the output-highlight path and the
    /// cursor-off-a-bracket input path.
    fn none() -> Self {
        Self { marks: Vec::new() }
    }

    /// The emphasis style for the bracket starting at byte `offset`,
    /// or `None` if that bracket is not emphasised.
    fn style_at(&self, offset: usize) -> Option<&'static str> {
        self.marks
            .iter()
            .find_map(|(o, style)| (*o == offset).then_some(*style))
    }
}

/// Render `src` under `palette`, applying any [`BracketEmphasis`]
/// overlay. Shared by [`highlight`] (no overlay) and
/// [`highlight_input`] (cursor bracket-match overlay).
///
/// Bytes between tokens (whitespace, uncoloured punctuation) are
/// emitted verbatim. When `palette` is [`Palette::Plain`], or when the
/// lexer rejects `src` outright (so no token stream is available), the
/// input is returned unchanged — highlighting is always a
/// presentational nicety, never load-bearing.
fn render(src: &str, palette: Palette, emphasis: &BracketEmphasis) -> String {
    if palette == Palette::Plain {
        return src.to_owned();
    }
    let tokens = match display_tokens(src) {
        Ok(t) => t,
        // Lexing failed — emit the raw text rather than nothing.
        Err(_) => return src.to_owned(),
    };
    render_tokens(src, palette, emphasis, &tokens)
}

fn render_tokens(
    src: &str,
    palette: Palette,
    emphasis: &BracketEmphasis,
    tokens: &[ClassifiedToken],
) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len() + tokens.len() * 8);
    let mut cursor: usize = 0;
    for tok in tokens {
        let start = tok.span.start as usize;
        let end = tok.span.end as usize;
        // Guard against any classifier span that runs past the
        // source (should not happen; the classifier spans index
        // `src`). Skip a malformed token rather than panicking.
        if start > end || end > bytes.len() || start < cursor {
            continue;
        }
        // Emit the gap before this token verbatim.
        out.push_str(&src[cursor..start]);
        let slice = &src[start..end];
        // A bracket the cursor emphasises overrides its palette
        // colour; otherwise fall back to the kind's style.
        let style = emphasis
            .style_at(start)
            .or_else(|| palette.style_for_kind(tok.kind));
        match style {
            Some(code) => {
                out.push_str(code);
                out.push_str(slice);
                out.push_str(RESET);
            }
            None => out.push_str(slice),
        }
        cursor = end;
    }
    // Trailing bytes after the last token.
    out.push_str(&src[cursor..]);
    out
}

/// One end of a paired bracket, located by the bracket scanner.
#[derive(Clone, Copy)]
struct Bracket {
    /// Byte offset of the bracket character in the source.
    offset: usize,
    /// Which bracket family: `()` / `[]` / `{}`.
    family: BracketFamily,
    /// Whether this is the opener (`(` `[` `{`) rather than the
    /// closer.
    is_open: bool,
}

/// The three structural bracket families Kio source uses. `(` `)` and
/// `{` `}` arrive as dedicated lexer tokens. Square brackets are ordinary
/// operator characters and join maximal symbol runs; the reference
/// classifier exposes them as `punctuation.bracket` only when the parser
/// consumed those exact bytes as forall delimiters. The scanner therefore
/// reads the family off the source byte after classification.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BracketFamily {
    Paren,
    Square,
    Curly,
}

impl BracketFamily {
    /// The family of a bracket byte, or `None` if `b` is not a
    /// bracket.
    fn of(b: u8) -> Option<(Self, bool)> {
        match b {
            b'(' => Some((BracketFamily::Paren, true)),
            b')' => Some((BracketFamily::Paren, false)),
            b'[' => Some((BracketFamily::Square, true)),
            b']' => Some((BracketFamily::Square, false)),
            b'{' => Some((BracketFamily::Curly, true)),
            b'}' => Some((BracketFamily::Curly, false)),
            _ => None,
        }
    }
}

/// Compute the bracket-match overlay for cursor offset `pos` in `src`.
///
/// Lexes `src`, collects every `punctuation.bracket` token, and finds
/// the one whose single byte the cursor sits on. Walks the bracket
/// list with a depth counter to locate the match — forward for an
/// opener, backward for a closer, counting only same-family brackets
/// so `([)]`-style interleaving doesn't false-match. On a match both
/// brackets get [`BRACKET_MATCH`]; with no match the cursor's bracket
/// alone gets [`BRACKET_MISMATCH`]. The cursor off any bracket (or a
/// lexer rejection) yields no emphasis.
#[cfg(test)]
fn bracket_emphasis_at(src: &str, pos: usize) -> BracketEmphasis {
    let bytes = src.as_bytes();
    // The cursor must sit on a one-byte bracket character. Bracket
    // bytes are ASCII, so a `pos` that lands inside a multibyte UTF-8
    // sequence never matches one (the lead/continuation bytes are all
    // ≥ 0x80) — multibyte input is safe.
    let Some(at) = bytes.get(pos).copied() else {
        return BracketEmphasis::none();
    };
    if BracketFamily::of(at).is_none() {
        return BracketEmphasis::none();
    }
    let tokens = match display_tokens(src) {
        Ok(t) => t,
        Err(_) => return BracketEmphasis::none(),
    };
    bracket_emphasis_at_tokens(src, pos, &tokens)
}

fn bracket_emphasis_at_tokens(
    src: &str,
    pos: usize,
    tokens: &[ClassifiedToken],
) -> BracketEmphasis {
    let bytes = src.as_bytes();
    let Some(at) = bytes.get(pos).copied() else {
        return BracketEmphasis::none();
    };
    if BracketFamily::of(at).is_none() {
        return BracketEmphasis::none();
    }
    // Collect bracket tokens in source order, reading family/openness
    // off the source byte at each token's start.
    let brackets: Vec<Bracket> = tokens
        .iter()
        .filter(|t| t.kind == TokenKind::PunctuationBracket)
        .filter_map(|t| {
            let offset = t.span.start as usize;
            let b = *bytes.get(offset)?;
            let (family, is_open) = BracketFamily::of(b)?;
            Some(Bracket {
                offset,
                family,
                is_open,
            })
        })
        .collect();
    // Index of the bracket the cursor sits on.
    let Some(cursor_idx) = brackets.iter().position(|b| b.offset == pos) else {
        return BracketEmphasis::none();
    };
    let cursor_bracket = brackets[cursor_idx];
    match find_match(&brackets, cursor_idx) {
        Some(match_idx) => BracketEmphasis {
            marks: vec![
                (cursor_bracket.offset, BRACKET_MATCH),
                (brackets[match_idx].offset, BRACKET_MATCH),
            ],
        },
        None => BracketEmphasis {
            marks: vec![(cursor_bracket.offset, BRACKET_MISMATCH)],
        },
    }
}

/// Find the index in `brackets` of the partner of `brackets[from]`,
/// matching within the same [`BracketFamily`] with a depth counter.
/// `None` when the input is unbalanced and no partner exists.
fn find_match(brackets: &[Bracket], from: usize) -> Option<usize> {
    let start = brackets[from];
    let family = start.family;
    let mut depth: usize = 0;
    if start.is_open {
        // Forward scan for the closer.
        for (i, b) in brackets.iter().enumerate().skip(from) {
            if b.family != family {
                continue;
            }
            if b.is_open {
                depth += 1;
            } else {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
    } else {
        // Backward scan for the opener.
        for i in (0..=from).rev() {
            let b = brackets[i];
            if b.family != family {
                continue;
            }
            if b.is_open {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            } else {
                depth += 1;
            }
        }
    }
    None
}

/// Whether the process's standard output is connected to a terminal.
///
/// Wraps `std::io::IsTerminal`; isolated here so the rest of the
/// module is `IsTerminal`-import-free and the one detection point is
/// easy to find.
fn stdout_is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Whether the terminal advertises 24-bit truecolor support via
/// `$COLORTERM`. The two conventional advertisements are
/// `COLORTERM=truecolor` and `COLORTERM=24bit`; this stays strict on
/// those two (see [`Palette::detect`] for why a false-positive is
/// worse than a false-negative).
fn terminal_advertises_truecolor() -> bool {
    std::env::var("COLORTERM")
        .map(|v| v == "truecolor" || v == "24bit")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_palette_is_identity() {
        let src = "module pkg/main;\npub fn run() -> . { () }\n";
        assert_eq!(highlight(src, Palette::plain()), src);
    }

    #[test]
    fn ansi_palette_wraps_keywords() {
        let out = highlight("pub fn run() -> . { () }", Palette::ansi());
        // The `fn` keyword should be wrapped in an SGR escape + reset.
        assert!(out.contains("\x1b["));
        assert!(out.contains(RESET));
        // Stripping every escape sequence recovers the original text.
        assert_eq!(strip_ansi(&out), "pub fn run() -> . { () }");
    }

    #[test]
    fn ansi_output_preserves_source_text() {
        // Highlighting must be lossless: the visible characters,
        // ignoring escapes, equal the input exactly.
        let src = "type Pair[A][B] = (A & B);";
        let out = highlight(src, Palette::ansi());
        assert_eq!(strip_ansi(&out), src);
    }

    #[test]
    fn lexer_failure_falls_back_to_raw() {
        // A byte the lexer rejects: highlighting must not panic and
        // must return the input.
        let weird = "\u{0}\u{0}";
        let out = highlight(weird, Palette::ansi());
        assert_eq!(out, weird);
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(highlight("", Palette::ansi()), "");
        assert_eq!(highlight("", Palette::plain()), "");
    }

    #[test]
    fn truecolor_palette_wraps_keyword() {
        let out = highlight("pub fn run() -> . { () }", Palette::truecolor());
        // `fn` is a declaration keyword — gold (#e5c07b) under the
        // truecolor palette — and the run is reset afterwards.
        assert!(out.contains("\x1b[38;2;229;192;123m"));
        assert!(out.contains(RESET));
        // Lossless: stripping escapes recovers the source.
        assert_eq!(strip_ansi(&out), "pub fn run() -> . { () }");
    }

    #[test]
    fn truecolor_un_collapses_keyword_classes() {
        // The 8-colour palette maps control / declaration / elaborator
        // keywords to one magenta; truecolor gives each its own hue.
        let control = Palette::truecolor()
            .style_for_kind(TokenKind::KeywordControl)
            .expect("control keyword is coloured");
        let decl = Palette::truecolor()
            .style_for_kind(TokenKind::KeywordDeclaration)
            .expect("declaration keyword is coloured");
        let elaborator = Palette::truecolor()
            .style_for_kind(TokenKind::KeywordElaborator)
            .expect("elaborator keyword is coloured");
        assert_ne!(control, decl);
        assert_ne!(decl, elaborator);
        assert_ne!(control, elaborator);
    }

    #[test]
    fn truecolor_de_emphasises_bracket_punctuation() {
        // Brackets are uncoloured under Ansi but de-emphasised (not
        // None) under truecolor.
        assert_eq!(
            Palette::ansi().style_for_kind(TokenKind::PunctuationBracket),
            None
        );
        assert!(
            Palette::truecolor()
                .style_for_kind(TokenKind::PunctuationBracket)
                .is_some()
        );
    }

    #[test]
    fn palette_detect_picks_truecolor_when_colorterm_set() {
        // The pure decision behind `detect()`: a colour TTY whose
        // `$COLORTERM` advertises truecolor → TrueColor. (Tested via
        // `classify_env` because the test harness's stdout is not a
        // TTY, so `detect()` itself always returns Plain.)
        assert_eq!(
            Palette::classify_env(true, false, false, true),
            Palette::TrueColor
        );
    }

    #[test]
    fn palette_detect_falls_back_to_ansi_without_colorterm() {
        // Colour TTY, no truecolor advertisement → Ansi.
        assert_eq!(
            Palette::classify_env(true, false, false, false),
            Palette::Ansi
        );
    }

    #[test]
    fn palette_detect_plain_when_no_color_set() {
        // `$NO_COLOR` forces Plain even on a truecolor TTY.
        assert_eq!(
            Palette::classify_env(true, true, false, true),
            Palette::Plain
        );
        // Non-TTY and dumb terminal are Plain too.
        assert_eq!(
            Palette::classify_env(false, false, false, true),
            Palette::Plain
        );
        assert_eq!(
            Palette::classify_env(true, false, true, true),
            Palette::Plain
        );
    }

    #[test]
    fn highlight_marks_matched_bracket() {
        // Cursor on the opening `(` of `(a + b)` → both parens wrapped
        // in the bracket-match style.
        let src = "(a + b)";
        let out = highlight_input(src, 0, Palette::truecolor());
        // Two BRACKET_MATCH wraps: one for `(`, one for `)`.
        let matches = out.matches(BRACKET_MATCH).count();
        assert_eq!(matches, 2, "both brackets emphasised: {out:?}");
        assert_eq!(strip_ansi(&out), src);
    }

    #[test]
    fn highlight_marks_unmatched_bracket_as_mismatch() {
        // Cursor on a `(` with no closer → mismatch warning style on
        // that bracket alone.
        let src = "(a + b";
        let out = highlight_input(src, 0, Palette::truecolor());
        assert!(out.contains(BRACKET_MISMATCH), "mismatch marked: {out:?}");
        assert!(!out.contains(BRACKET_MATCH));
        assert_eq!(strip_ansi(&out), src);
    }

    #[test]
    fn highlight_input_no_bracket_under_cursor_matches_output() {
        // Cursor off any bracket → identical to the output highlight.
        let src = "(a + b)";
        let input = highlight_input(src, 2, Palette::truecolor());
        let output = highlight(src, Palette::truecolor());
        assert_eq!(input, output);
    }

    #[test]
    fn highlight_input_plain_palette_is_identity() {
        let src = "(a + b)";
        assert_eq!(highlight_input(src, 0, Palette::plain()), src);
    }

    #[test]
    fn bracket_match_survives_multibyte_input() {
        // A string literal with a multibyte char before the closer:
        // byte offsets must still line up. Cursor on the closing `)`.
        let src = "(\"héllo\")";
        let close = src.rfind(')').expect("closing bracket present");
        let out = highlight_input(src, close, Palette::truecolor());
        assert_eq!(out.matches(BRACKET_MATCH).count(), 2);
        // Lossless on multibyte input.
        assert_eq!(strip_ansi(&out), src);
    }

    #[test]
    fn bracket_match_respects_families() {
        // The bracket-shaped operator run is not structural punctuation:
        // cursor on the outer `(` matches the `)`, ignoring `[x]`.
        let src = "(f [x] )";
        let open = src.find('(').expect("open paren");
        let close = src.rfind(')').expect("close paren");
        let out = highlight_input(src, open, Palette::truecolor());
        // The `)` is emphasised; verify by checking the byte right
        // before the close paren style sits at `close` after strip.
        assert_eq!(out.matches(BRACKET_MATCH).count(), 2);
        assert_eq!(strip_ansi(&out), src);
        // Sanity: the matcher really pairs `(`↔`)`.
        let emphasis = bracket_emphasis_at(src, open);
        assert!(emphasis.style_at(open).is_some());
        assert!(emphasis.style_at(close).is_some());
    }

    #[test]
    fn bracket_match_uses_parser_confirmed_forall_brackets_only() {
        let forall = "module x; type Id = [A] A -> A;";
        let open = forall.find('[').expect("forall opener present");
        let close = forall.find(']').expect("forall closer present");
        let emphasis = bracket_emphasis_at(forall, open);
        assert!(emphasis.style_at(open).is_some());
        assert!(emphasis.style_at(close).is_some());

        let operator = "[x]";
        let operator_emphasis = bracket_emphasis_at(operator, 0);
        assert!(operator_emphasis.marks.is_empty());
    }

    #[cfg(feature = "repl")]
    #[test]
    fn reedline_style_agrees_with_sgr_on_which_kinds_colour() {
        // The reedline `Style` mapping and the SGR-string mapping are
        // two renderings of the *same* colour decisions; they must
        // agree on which kinds are coloured at all. A kind is `None`
        // in one exactly when it is `None` in the other, under every
        // palette.
        use TokenKind::*;
        let all = [
            KeywordControl,
            KeywordDeclaration,
            KeywordElaborator,
            LiteralBool,
            LiteralString,
            LiteralNumber,
            CommentLine,
            CommentDoc,
            OperatorBuiltin,
            OperatorUser,
            EntityNameFunction,
            EntityNameFunctionReference,
            EntityNameType,
            EntityNameModule,
            EntityNameLabel,
            EntityNameLabelReference,
            EntityNameQualifiedLabelReference,
            VariableParameter,
            PunctuationBracket,
            PunctuationSeparator,
            Identifier,
            Slot,
        ];
        for palette in [Palette::Plain, Palette::Ansi, Palette::TrueColor] {
            for kind in all {
                assert_eq!(
                    palette.style_for_kind(kind).is_some(),
                    palette.style_for_kind_reedline(kind).is_some(),
                    "SGR vs reedline disagree on whether {kind:?} is coloured under {palette:?}"
                );
            }
        }
    }

    #[cfg(feature = "repl")]
    #[test]
    fn styled_input_is_lossless_under_truecolor() {
        // Concatenating the styled segments recovers the source text,
        // the structured analogue of the SGR path's strip-recovers-src
        // invariant.
        let src = "pub fn run() -> . { () }";
        let styled = styled_input(src, 0, Palette::truecolor());
        let joined: String = styled.buffer.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(joined, src);
        // At least one segment carries a non-default style (`fn`, …).
        assert!(
            styled
                .buffer
                .iter()
                .any(|(style, _)| *style != nu_ansi_term::Style::new())
        );
    }

    #[cfg(feature = "repl")]
    #[test]
    fn styled_input_plain_palette_is_one_unstyled_segment() {
        let src = "(a + b)";
        let styled = styled_input(src, 0, Palette::plain());
        assert_eq!(styled.buffer.len(), 1);
        assert_eq!(styled.buffer[0].0, nu_ansi_term::Style::new());
        assert_eq!(styled.buffer[0].1, src);
    }

    #[cfg(feature = "repl")]
    #[test]
    fn styled_input_emphasises_matched_bracket() {
        // Cursor on the opening `(` of `(a + b)` → the bracket segments
        // carry the bold bracket-match style, distinct from the
        // ordinary punctuation style.
        let src = "(a + b)";
        let styled = styled_input(src, 0, Palette::truecolor());
        let emphasis = bracket_emphasis_style(true);
        let emphasised = styled
            .buffer
            .iter()
            .filter(|(style, text)| *style == emphasis && (text == "(" || text == ")"))
            .count();
        assert_eq!(
            emphasised, 2,
            "both brackets emphasised: {:?}",
            styled.buffer
        );
        // Lossless.
        let joined: String = styled.buffer.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(joined, src);
    }

    #[cfg(feature = "repl")]
    #[test]
    fn styled_input_lexer_failure_falls_back_to_raw_segment() {
        // A byte the lexer rejects: one unstyled segment with the raw
        // input, never a panic.
        let weird = "\u{0}\u{0}";
        let styled = styled_input(weird, 0, Palette::truecolor());
        assert_eq!(styled.buffer.len(), 1);
        assert_eq!(styled.buffer[0].1, weird);
    }

    /// Strip every ANSI SGR escape (`\x1b[...m`) from `s`, leaving
    /// the visible text. Test-only helper.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Consume up to and including the terminating `m`.
                for e in chars.by_ref() {
                    if e == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
