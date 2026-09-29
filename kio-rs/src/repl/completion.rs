//! reedline line-editor glue for the `kio repl` inspector.
//!
//! reedline takes each line-editor behaviour as a separate boxed trait
//! object, so the helper bundle is split into four small types:
//! [`ReplCompleter`] (context-aware, fuzzy-ranked completion),
//! [`ReplHighlighter`] (live input highlighting), [`ReplHinter`] (inline
//! ghost-text prediction) and [`ReplValidator`] (the multi-line
//! continuation gate). The REPL loop boxes each into
//! `Reedline::with_completer` / `with_highlighter` / `with_hinter` /
//! `with_validator`.
//!
//! The candidate set, fuzzy ranking, shape routing, and bracket-balance
//! predicate all live in the terminal-free
//! [`crate::repl_core::completion`] core so the browser wasm wrapper
//! offers the same candidates; this module only renders that core's
//! output into the reedline shapes.
//!
//! ## Inline hints — ghost-text prediction
//!
//! [`ReplHinter`] surfaces a fish-style ghost-text hint: as the user
//! types `:t fac`, the predicted completion (`torial`) renders in dim
//! text after the cursor; right-arrow / Tab accepts it, any other key
//! dismisses it without touching the buffer. The hint reads the same
//! [`NameSet`] and the same
//! [`CompletionShape`](crate::repl_core::commands::CompletionShape)
//! routing the completer consults — [`shape_for_position`] resolves what
//! the cursor's argument position completes to, [`candidates_for_shape`]
//! produces the prefix-matching candidate strings, and
//! [`longest_common_completion`] reduces them to the suffix every
//! candidate shares. Above
//! [`HINT_MAX_CANDIDATES`] matches the shared suffix is tiny and noisy,
//! so the hinter stays silent and lets the popup menu do the work.
//!
//! The hint pool is **strict prefix**, deliberately so even though the
//! completion *menu* fuzzy-matches: ghost text from a fuzzy match is too
//! speculative to render as a confident prediction the user accepts
//! blind. The asymmetry — fuzzy completion menu, strict-prefix hints —
//! is intentional.
//!
//! An incomplete input (an unclosed bracket — `:normalize if true {`)
//! suppresses the hint. [`input_is_complete`] is the bracket-balance
//! predicate; the multi-line session reuses it to drive the validator's
//! continuation decision, so the two surfaces agree on what "complete"
//! means.
//!
//! ## Live input highlighting
//!
//! [`ReplHighlighter`] colours the line at the prompt as the user types,
//! using the same [`crate::repl_core::highlight`] classifier the printed
//! output uses. A [`Palette::Plain`] palette emits a single unstyled
//! segment carrying the raw line.

use std::sync::{Arc, Mutex};

use reedline::{Completer, Highlighter, Hinter, Span, StyledText, Suggestion, Validator};
use reedline::{History, ValidationResult};

use crate::error::Error;
use crate::pass::lexer::{TokenKind, lex};
use crate::pass::parser::ExpressionParseContext;
use crate::repl_core::commands::{CompletionShape, all_command_spellings};
use crate::repl_core::completion::{
    AstScopeProvider, Candidate, ScopeProvider, complete, expression_input_start,
    expression_target, first_token, item_allowed_for_command, shape_for_position, word_start,
};
use crate::repl_core::highlight::{self, Palette};

pub use crate::repl_core::completion::NameSet;

/// Render one core [`Candidate`] into a reedline [`Suggestion`] replacing
/// `span`. No trailing whitespace is appended: a completed module path
/// or name is typically the end of the argument, and the user adds their
/// own separator when chaining.
fn to_suggestion(candidate: Candidate, span: Span) -> Suggestion {
    Suggestion {
        value: candidate.label,
        display_override: None,
        description: candidate.description,
        style: None,
        extra: None,
        span,
        append_whitespace: false,
        match_indices: Some(candidate.match_indices),
    }
}

/// The reedline [`Completer`] for the REPL line reader. Holds a shared
/// handle to the [`NameSet`] the REPL keeps current.
pub struct ReplCompleter {
    names: Arc<Mutex<NameSet>>,
}

impl ReplCompleter {
    /// Construct a completer backed by `names`. The REPL keeps a clone of
    /// the same `Arc` and overwrites the inner [`NameSet`] after each
    /// state-changing command.
    pub fn new(names: Arc<Mutex<NameSet>>) -> Self {
        Self { names }
    }

    /// The completion routing, factored out of the trait method so it is
    /// callable from unit tests without the `&mut self` the trait
    /// requires (the routing reads, never mutates).
    fn route(&self, line: &str, pos: usize) -> Vec<Suggestion> {
        let names = self.names.lock().expect("name-set mutex not poisoned");
        let provider = AstScopeProvider::new(&names);
        let completion = complete(&names, &provider, line, pos);
        let span = Span::new(completion.replace.start, completion.replace.end);
        completion
            .candidates
            .into_iter()
            .map(|candidate| to_suggestion(candidate, span))
            .collect()
    }
}

impl Completer for ReplCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        self.route(line, pos)
    }
}

/// The reedline [`Highlighter`] for the REPL line reader. Holds the
/// active [`Palette`] the live input highlighter renders under.
pub struct ReplHighlighter {
    palette: Palette,
    names: Arc<Mutex<NameSet>>,
}

impl ReplHighlighter {
    /// Construct a highlighter rendering under `palette` — the
    /// [`Palette::detect`] result the REPL loop computed at startup.
    pub fn new(palette: Palette, names: Arc<Mutex<NameSet>>) -> Self {
        Self { palette, names }
    }
}

impl Highlighter for ReplHighlighter {
    /// Colour the live input line under the active palette. A plain
    /// palette yields a single unstyled segment; any colour palette runs
    /// the shared [`highlight::styled_input`] classifier, which also
    /// overlays the cursor's bracket match.
    fn highlight(&self, line: &str, cursor: usize) -> StyledText {
        let context = self
            .names
            .lock()
            .expect("name-set mutex not poisoned")
            .expression_parse_context
            .clone();
        highlight::styled_input_with_context(line, cursor, self.palette, context.as_deref())
    }
}

/// The strict-prefix `:command` spellings (canonical names and short
/// synonyms) that start with `word` — the value-level line-start pool the
/// hinter consults. The completion *menu*'s command pool fuzzy-matches;
/// the hint pool stays strict prefix (see the module docs).
fn command_candidates(word: &str) -> Vec<String> {
    all_command_spellings()
        .into_iter()
        .filter(|c| c.starts_with(word))
        .collect()
}

/// The candidate **strings** a `shape`-position argument completes to,
/// filtered to those starting with `prefix` — the strict-prefix,
/// description-free sibling of the core's fuzzy candidate builders the
/// hinter's ghost-text prediction consults.
fn candidates_for_shape(
    names: &NameSet,
    shape: CompletionShape,
    command: &str,
    prefix: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    match shape {
        CompletionShape::None | CompletionShape::Expression => {}
        CompletionShape::ModulePath => {
            for m in &names.modules {
                if m.path.starts_with(prefix) {
                    out.push(m.path.clone());
                }
            }
        }
        CompletionShape::NameOrFqn
        | CompletionShape::NameOrFqnOrBuiltin
        | CompletionShape::NameFqnOrOp
        | CompletionShape::NameFqnOpOrBuiltin => {
            for it in &names.items {
                if !item_allowed_for_command(it.kind, command) {
                    continue;
                }
                if it.short_name.starts_with(prefix) {
                    out.push(it.short_name.clone());
                }
                if it.fqn.starts_with(prefix) {
                    out.push(it.fqn.clone());
                }
            }
            if matches!(
                shape,
                CompletionShape::NameFqnOrOp | CompletionShape::NameFqnOpOrBuiltin
            ) {
                for op in &names.operators {
                    if op.grammar.starts_with(prefix) {
                        out.push(op.grammar.clone());
                    }
                }
            }
            if matches!(
                shape,
                CompletionShape::NameOrFqnOrBuiltin | CompletionShape::NameFqnOpOrBuiltin
            ) {
                for builtin in &names.builtins {
                    if builtin.name.starts_with(prefix) {
                        out.push(builtin.name.clone());
                    }
                }
            }
        }
    }
    out
}

/// The ghost-text suffix to predict after `word`, given the
/// prefix-matched `candidates`:
///
/// - exactly one candidate → its suffix after `word`;
/// - several candidates → the suffix every candidate shares past `word`
///   (their longest common prefix, minus `word`), so accepting is always
///   safe;
/// - no candidate → the empty string.
fn longest_common_completion(candidates: &[String], word: &str) -> String {
    let Some((first, rest)) = candidates.split_first() else {
        return String::new();
    };
    let mut common_len = first.len();
    for cand in rest {
        common_len = common_prefix_len(&first[..common_len], cand);
    }
    first[word.len()..common_len].to_owned()
}

/// The byte length of the longest common prefix of `a` and `b` that lands
/// on a `char` boundary.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.bytes()
        .zip(b.bytes())
        .take_while(|(x, y)| x == y)
        .count()
        .min(a.len())
        .min(b.len())
}

/// Whether `line` is a complete input — every parenthesis, brace, and
/// structural forall binder it opens is closed, and every string literal is
/// terminated. The hinter suppresses its ghost text on an incomplete line,
/// and the multi-line validator reports continuation on one, so the two
/// surfaces agree on what "complete" means.
///
/// Reads the bracket characters off the lexer's token stream, so a closed
/// string literal (one [`TokenKind::StrLit`] token) keeps any brackets
/// *inside* it (`:normalize "foo {"`) out of the counter.
///
/// Square brackets are ordinary operator characters at the lexer boundary.
/// Only a parser-confirmed forall-binder prefix contributes square delimiter
/// facts; a bare `[` or a run such as `[!` is an operator and must not strand
/// an otherwise complete input in the continuation prompt.
///
/// Lexer rejection splits two ways: an **unterminated string at end of
/// input** (`:normalize "foo`) is *incomplete* (a closing quote would
/// complete it); **any other lex rejection** is *complete* (there is no
/// token stream to reason about, and the parser must get to surface the
/// error).
#[cfg(test)]
pub(crate) fn input_is_complete(line: &str) -> bool {
    input_is_complete_with_context(line, None)
}

pub(crate) fn input_is_complete_with_context(
    line: &str,
    context: Option<&ExpressionParseContext>,
) -> bool {
    let tokens = match lex(line) {
        Ok(tokens) => tokens,
        Err(e) => return !is_unterminated_string_at_eof(&e),
    };
    let mut paren: i32 = 0;
    let mut curly: i32 = 0;
    for tok in &tokens {
        match &tok.kind {
            TokenKind::LParen => paren += 1,
            TokenKind::RParen => paren -= 1,
            TokenKind::LBrace => curly += 1,
            TokenKind::RBrace => curly -= 1,
            _ => {}
        }
    }
    let has_square = tokens
        .iter()
        .any(|token| matches!(&token.kind, TokenKind::SymbolRun(run) if run.contains('[')));
    let unclosed_forall = has_square
        && expression_input_start(line).is_some_and(|expr_start| {
            crate::pass::parser::expression_has_unclosed_structural_forall(
                &line[expr_start..],
                context,
            )
        });
    paren <= 0 && !unclosed_forall && curly <= 0
}

/// Whether a lexer [`Error`] is the *unterminated-string-at-end-of-input*
/// case — a string literal opened but not yet closed, with no intervening
/// newline. That input is *incomplete*: a closing quote would complete
/// it.
///
/// The lexer raises two distinct unterminated-string messages: a plain
/// `"unterminated string literal"` when input runs out mid-literal (the
/// continuation case), and `"unterminated string literal (newline)"` when
/// a raw newline breaks the literal (a hard error no continuation fixes).
/// Only the former is a continuation; distinguishing them on the message
/// keeps this predicate from stranding the user in a string they can
/// never close.
fn is_unterminated_string_at_eof(error: &Error) -> bool {
    let (_, message) = error.diag();
    message == "unterminated string literal"
}

/// The maximum number of prefix-matching candidates for which the hinter
/// predicts ghost text. Above this the longest common suffix is usually
/// empty or a single character — too little signal to render as a
/// confident prediction — so the hinter stays silent and lets the popup
/// completion menu present the full set.
const HINT_MAX_CANDIDATES: usize = 5;

/// The reedline [`Hinter`] for the REPL: a fish-style ghost-text
/// predictor. As the user types in a command-argument position, the
/// hinter offers the suffix the typed word would gain from the agreed-on
/// completion, rendered in dim text after the cursor; right-arrow / Tab
/// accepts it.
pub struct ReplHinter {
    names: Arc<Mutex<NameSet>>,
    /// The unformatted hint computed by the last [`Hinter::handle`] call
    /// — the text reedline commits when the user accepts the hint.
    current_hint: String,
}

impl ReplHinter {
    /// Construct a hinter backed by `names`. The REPL keeps a clone of
    /// the same `Arc` (shared with the completer).
    pub fn new(names: Arc<Mutex<NameSet>>) -> Self {
        Self {
            names,
            current_hint: String::new(),
        }
    }

    /// Recompute the ghost-text hint for the cursor at `pos` in `line`,
    /// independent of reedline's `&mut self` / history / colouring — the
    /// pure decision the unit tests pin. Returns the unformatted suffix
    /// to predict (empty for "no hint").
    ///
    /// The pool mirrors the completer's routing so the two stay in
    /// lockstep, but stays strict-prefix (a fuzzy match is too speculative
    /// to render as ghost text): the `:command` table at line start,
    /// [`candidates_for_shape`] in a command argument, and the in-scope
    /// identifiers at an expression position (a bare line or a
    /// `:normalize` argument).
    fn predict(&self, line: &str, pos: usize) -> String {
        let context = self
            .names
            .lock()
            .expect("name-set mutex not poisoned")
            .expression_parse_context
            .clone();
        if !input_is_complete_with_context(line, context.as_deref()) {
            return String::new();
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            return String::new();
        }
        let names = self.names.lock().expect("name-set mutex not poisoned");
        if !trimmed.starts_with(':') {
            return identifier_hint(&names, line, pos);
        }
        let word_start = word_start(line, pos);
        let word = &line[word_start..pos];
        if word.is_empty() {
            return String::new();
        }
        if word_start == 0 {
            return hint_from(command_candidates(word), word);
        }
        let shape = shape_for_position(line);
        if shape == CompletionShape::Expression {
            return identifier_hint(&names, line, pos);
        }
        hint_from(
            candidates_for_shape(&names, shape, first_token(line), word),
            word,
        )
    }
}

/// The strict-prefix ghost-text hint for the in-scope identifier under
/// the cursor at `pos` — the expression-position sibling of the
/// command-argument hint pool, drawn through the same
/// [`AstScopeProvider`] the completer uses.
fn identifier_hint(names: &NameSet, line: &str, pos: usize) -> String {
    let provider = AstScopeProvider::new(names);
    let (expr_start, rel_cursor, replace) = expression_target(line, pos);
    let prefix = &line[replace];
    if prefix.is_empty() {
        return String::new();
    }
    let labels: Vec<String> = provider
        .in_scope(&line[expr_start..], rel_cursor)
        .into_iter()
        .filter(|candidate| candidate.label.starts_with(prefix))
        .map(|candidate| candidate.label)
        .collect();
    hint_from(labels, prefix)
}

/// The ghost-text suffix for `word` given its strict-prefix-matching
/// `candidates`: the suffix every candidate shares past `word`, or
/// nothing when there are no candidates or too many to predict
/// confidently.
fn hint_from(candidates: Vec<String>, word: &str) -> String {
    if candidates.is_empty() || candidates.len() > HINT_MAX_CANDIDATES {
        return String::new();
    }
    longest_common_completion(&candidates, word)
}

impl Hinter for ReplHinter {
    fn handle(
        &mut self,
        line: &str,
        pos: usize,
        _history: &dyn History,
        use_ansi_coloring: bool,
        _cwd: &str,
    ) -> String {
        self.current_hint = self.predict(line, pos);
        if use_ansi_coloring && !self.current_hint.is_empty() {
            nu_ansi_term::Style::new()
                .dimmed()
                .paint(&self.current_hint)
                .to_string()
        } else {
            self.current_hint.clone()
        }
    }

    fn complete_hint(&self) -> String {
        self.current_hint.clone()
    }

    fn next_hint_token(&self) -> String {
        self.current_hint.clone()
    }
}

/// The reedline [`Validator`] for the REPL — the multi-line gate.
///
/// reedline calls [`Validator::validate`] on every `Enter`. An
/// [`ValidationResult::Incomplete`] keeps the buffer open and renders the
/// continuation prompt for the next line; a [`ValidationResult::Complete`]
/// submits the whole (possibly multi-line) buffer in one go.
///
/// The continuation decision is exactly [`input_is_complete`], the
/// bracket / string-literal balance predicate the [`ReplHinter`] also
/// consults to suppress speculative ghost text. Sharing the one
/// predicate keeps the validator and the hinter from drifting on what
/// "complete" means. A well-formed single-line command (`:t foo`) has
/// balanced brackets, so it reports complete and submits on the first
/// `Enter`; only a command whose argument genuinely opens a bracket —
/// `:normalize if true {` — continues.
pub struct ReplValidator {
    names: Arc<Mutex<NameSet>>,
}

impl ReplValidator {
    pub fn new(names: Arc<Mutex<NameSet>>) -> Self {
        Self { names }
    }
}

impl Validator for ReplValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        let context = self
            .names
            .lock()
            .expect("name-set mutex not poisoned")
            .expression_parse_context
            .clone();
        if input_is_complete_with_context(line, context.as_deref()) {
            ValidationResult::Complete
        } else {
            ValidationResult::Incomplete
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl_core::completion::{ItemEntry, ItemKind, ModuleEntry};

    fn completer_with(set: NameSet) -> ReplCompleter {
        ReplCompleter::new(Arc::new(Mutex::new(set)))
    }

    #[test]
    fn route_preserves_trailing_label_replacement_and_keyword_description() {
        let completer = completer_with(NameSet {
            current_module_src: Some("module app; elab choose : (. & .) -> . { trailing product; trailing product otherwise; impl implementation }".to_owned()),
            ..NameSet::default()
        });
        let input = "choose! { () } oth";
        let suggestions = completer.route(input, input.len());
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].value, "otherwise");
        assert_eq!(suggestions[0].span, Span::new(input.len() - 3, input.len()));
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("trailing block label")
        );
    }

    /// A one-item name set for the reedline-mapping assertions: a public
    /// `fn add_i32` in `demo/main`, plus an unloaded module `demo/main`.
    fn mapping_set() -> NameSet {
        NameSet {
            modules: vec![ModuleEntry {
                path: "demo/main".to_owned(),
                item_count: 1,
                import_count: 0,
                is_loaded: true,
            }],
            items: vec![ItemEntry {
                short_name: "add_i32".to_owned(),
                fqn: "demo/main.add_i32".to_owned(),
                kind: ItemKind::Fn,
                source_module: "demo/main".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        }
    }

    #[test]
    fn route_maps_span_and_description() {
        // The reedline `Suggestion` carries the core candidate's
        // replacement span and description column verbatim.
        let completer = completer_with(mapping_set());
        let suggestion = completer
            .route(":t add", ":t add".len())
            .into_iter()
            .find(|s| s.value == "add_i32")
            .expect("add_i32 offered");
        assert_eq!(suggestion.span, Span::new(":t ".len(), ":t add".len()));
        assert_eq!(
            suggestion.description,
            Some("pub fn — demo/main".to_owned())
        );
        assert!(!suggestion.append_whitespace);
    }

    #[test]
    fn route_maps_match_indices() {
        // The fuzzy match's char positions reach the reedline suggestion,
        // for the menu's underline.
        let completer = completer_with(mapping_set());
        let suggestion = completer
            .route(":t i32", ":t i32".len())
            .into_iter()
            .find(|s| s.value == "add_i32")
            .expect("add_i32 offered as a substring match");
        assert_eq!(suggestion.match_indices, Some(vec![4, 5, 6]));
    }

    #[test]
    fn route_command_at_line_start_maps_span() {
        let completer = completer_with(NameSet::default());
        let suggestions = completer.route(":lo", 3);
        assert_eq!(suggestions[0].span.start, 0);
        assert!(suggestions.iter().any(|s| s.value == ":load"));
    }

    #[test]
    fn highlighter_styles_colored_segments_for_truecolor_palette() {
        let hl = ReplHighlighter::new(
            Palette::truecolor(),
            Arc::new(Mutex::new(NameSet::default())),
        );
        let styled = hl.highlight("scope! { () }", 0);
        assert!(
            styled
                .buffer
                .iter()
                .any(|(style, _)| *style != nu_ansi_term::Style::new()),
            "expected a coloured segment: {:?}",
            styled.buffer
        );
        let ordinary = hl.highlight("fn x", 0);
        assert!(
            ordinary
                .buffer
                .iter()
                .all(|(style, _)| *style == nu_ansi_term::Style::new())
        );
        assert_eq!(
            ordinary
                .buffer
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<String>(),
            "fn x"
        );
        let joined: String = styled.buffer.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(joined, "scope! { () }");
    }

    #[test]
    fn highlighter_unstyled_for_plain_palette() {
        let hl = ReplHighlighter::new(Palette::plain(), Arc::new(Mutex::new(NameSet::default())));
        let styled = hl.highlight("fn x", 0);
        assert_eq!(styled.buffer.len(), 1);
        assert_eq!(styled.buffer[0].0, nu_ansi_term::Style::new());
        assert_eq!(styled.buffer[0].1, "fn x");
    }

    fn hinter_with(set: NameSet) -> ReplHinter {
        ReplHinter::new(Arc::new(Mutex::new(set)))
    }

    fn hint_for(hinter: &ReplHinter, line: &str) -> String {
        hinter.predict(line, line.len())
    }

    fn validates_complete(line: &str) -> bool {
        let validator = ReplValidator::new(Arc::new(Mutex::new(NameSet::default())));
        matches!(validator.validate(line), ValidationResult::Complete)
    }

    fn validates_complete_with_context(line: &str, context: ExpressionParseContext) -> bool {
        let names = NameSet {
            expression_parse_context: Some(Arc::new(context)),
            ..NameSet::default()
        };
        let validator = ReplValidator::new(Arc::new(Mutex::new(names)));
        matches!(validator.validate(line), ValidationResult::Complete)
    }

    fn local_expression_context(module_src: &str) -> ExpressionParseContext {
        let module = crate::pass::parser::parse(module_src).expect("parse context module");
        crate::pass::parser::expression_parse_context(&module)
            .expect("build expression parse context")
    }

    fn imported_bracket_operator_names() -> NameSet {
        let root = std::path::PathBuf::from("/kio-repl-tests/bracket-operator-context");
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            root.join("app.pkg.kio"),
            "package app;\n\nbridge {\n  app/**;\n}\n".to_owned(),
        );
        files.insert(
            root.join("app/syntax.kio"),
            "module app/syntax;\n\
             pub fn bracket(left: ., right: .) -> . { left }\n\
             pub fn empty() -> . { () }\n\
             pub varop [* *] { foldl bracket empty }\n"
                .to_owned(),
        );
        files.insert(
            root.join("app/main.kio"),
            "module app/main;\n\
             import app/syntax(varop [* *]);\n\
             pub fn run() -> . { [* (), () *] }\n"
                .to_owned(),
        );
        let mut session = crate::repl_core::session::Session::new_in_memory(root, files);
        let loaded = crate::repl_core::commands::Command::Load("app/main".to_owned())
            .run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );
        let names = NameSet::from_session(&session);
        assert!(names.expression_parse_context.is_some());
        names
    }

    #[test]
    fn validator_completes_single_line_command() {
        assert!(validates_complete(":t foo"));
        assert!(validates_complete(":load pkg/a"));
        assert!(validates_complete(":source run"));
    }

    #[test]
    fn validator_completes_single_line_expression() {
        assert!(validates_complete("1 + 2"));
        assert!(validates_complete("factorial(3)"));
    }

    #[test]
    fn validator_marks_unclosed_brace_incomplete() {
        assert!(!validates_complete("if true {"));
        assert!(!validates_complete(":normalize if true {"));
    }

    #[test]
    fn validator_marks_unclosed_paren_incomplete() {
        assert!(!validates_complete("factorial(3"));
        assert!(!validates_complete(":t foo(x"));
    }

    #[test]
    fn validator_does_not_balance_ordinary_bracket_operators() {
        assert!(validates_complete("a [! b"));
        assert!(validates_complete("a [ b"));
        assert!(validates_complete("a [ B"));
        assert!(validates_complete("xs[0"));
        assert!(validates_complete("["));
    }

    #[test]
    fn validator_uses_the_loaded_operator_context() {
        let plus = local_expression_context(
            "module x; fn add(a: A, b: A) -> A { a } op _ + _ { impl add; };",
        );
        assert!(!validates_complete_with_context(":t value + .[A", plus,));

        let dollar = local_expression_context(
            "module x; fn apply(a: A, b: A) -> A { a } op _ $ _ { impl apply; };",
        );
        assert!(validates_complete(":t lhs $ .[A"));
        assert!(!validates_complete_with_context(":t lhs $ .[A", dollar,));
    }

    #[test]
    fn validator_uses_imported_operator_context_from_session_names() {
        let validator = ReplValidator::new(Arc::new(Mutex::new(imported_bracket_operator_names())));
        assert!(matches!(
            validator.validate(":t [* .[A"),
            ValidationResult::Incomplete
        ));
    }

    #[test]
    fn highlighter_uses_imported_operator_context_for_bracket_roles() {
        let highlighter = ReplHighlighter::new(
            Palette::truecolor(),
            Arc::new(Mutex::new(imported_bracket_operator_names())),
        );
        let styled = highlighter.highlight(":t [* .[A", 0);
        let bracket_styles: Vec<_> = styled
            .buffer
            .iter()
            .filter_map(|(style, text)| text.starts_with('[').then_some(*style))
            .collect();
        assert_eq!(bracket_styles.len(), 2, "{:?}", styled.buffer);
        assert_eq!(
            bracket_styles[0],
            Palette::truecolor()
                .style_for_kind_reedline(crate::tokens::TokenKind::OperatorUser)
                .expect("operator style")
        );
        assert_eq!(
            bracket_styles[1],
            Palette::truecolor()
                .style_for_kind_reedline(crate::tokens::TokenKind::PunctuationBracket)
                .expect("structural bracket style")
        );

        let fqn_command = highlighter.highlight(":load .[A", 0);
        let fqn_bracket = fqn_command
            .buffer
            .iter()
            .find_map(|(style, text)| text.contains('[').then_some(*style))
            .expect("bracket segment");
        assert_eq!(fqn_bracket, bracket_styles[0]);
    }

    #[test]
    fn validator_completes_balanced_multiline() {
        let balanced = ":normalize if true {\n  ()\n} else {\n  ()\n}";
        assert!(validates_complete(balanced));
        let long = format!(":t {}", "wrap(".repeat(8) + "x" + &")".repeat(8));
        assert!(validates_complete(&long));
    }

    #[test]
    fn validator_completes_unlexable_input() {
        assert!(validates_complete("\x07"));
    }

    #[test]
    fn validator_marks_unterminated_string_incomplete() {
        assert!(!validates_complete(":normalize \"foo"));
        assert!(validates_complete(":normalize \"foo {\""));
    }

    #[test]
    fn hinter_returns_suffix_for_single_match() {
        let set = NameSet {
            items: vec![ItemEntry {
                short_name: "factorial".to_owned(),
                fqn: "exec_factorial/main.factorial".to_owned(),
                kind: ItemKind::Fn,
                source_module: "exec_factorial/main".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        };
        let hinter = hinter_with(set);
        assert_eq!(hint_for(&hinter, ":t fac"), "torial");
    }

    #[test]
    fn hinter_returns_longest_common_suffix() {
        let set = NameSet {
            items: vec![
                ItemEntry {
                    short_name: "add_i32".to_owned(),
                    fqn: "demo/main.add_i32".to_owned(),
                    kind: ItemKind::Fn,
                    source_module: "demo/main".to_owned(),
                    vis_pub: true,
                },
                ItemEntry {
                    short_name: "add_f32".to_owned(),
                    fqn: "demo/main.add_f32".to_owned(),
                    kind: ItemKind::Fn,
                    source_module: "demo/main".to_owned(),
                    vis_pub: true,
                },
            ],
            ..NameSet::default()
        };
        let hinter = hinter_with(set);
        assert_eq!(hint_for(&hinter, ":t add_"), "");
        assert_eq!(hint_for(&hinter, ":t demo/main.a"), "dd_");
    }

    #[test]
    fn hinter_empty_above_threshold() {
        let items = (0..10)
            .map(|i| ItemEntry {
                short_name: format!("f{i}_helper"),
                fqn: format!("demo/main.f{i}_helper"),
                kind: ItemKind::Fn,
                source_module: "demo/main".to_owned(),
                vis_pub: true,
            })
            .collect();
        let hinter = hinter_with(NameSet {
            items,
            ..NameSet::default()
        });
        assert!(hint_for(&hinter, ":t f").is_empty());
    }

    #[test]
    fn hinter_empty_for_incomplete_input() {
        let set = NameSet {
            items: vec![ItemEntry {
                short_name: "xenon".to_owned(),
                fqn: "demo/main.xenon".to_owned(),
                kind: ItemKind::Fn,
                source_module: "demo/main".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        };
        let hinter = hinter_with(set);
        assert!(hint_for(&hinter, ":normalize if! .t {").is_empty());
        assert!(hint_for(&hinter, ":t foo(x").is_empty());
    }

    #[test]
    fn hinter_keeps_forwarding_selectors_in_declared_entity_commands() {
        let hinter = hinter_with(NameSet {
            items: vec![ItemEntry {
                short_name: "{field}".to_owned(),
                fqn: "pkg/api.{field}".to_owned(),
                kind: ItemKind::LabelForward,
                source_module: "pkg/api".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        });
        for command in ["t", "type", "pure", "normalize"] {
            assert_eq!(hint_for(&hinter, &format!(":{command} pkg/api.")), "");
        }
        for command in ["doc", "signature", "source", "refs", "which"] {
            assert_eq!(
                hint_for(&hinter, &format!(":{command} pkg/api.")),
                "{field}"
            );
        }
        assert_eq!(hint_for(&hinter, "fi"), "");
    }

    #[test]
    fn hinter_respects_shape() {
        let set = NameSet {
            modules: vec![ModuleEntry {
                path: "other/main".to_owned(),
                item_count: 0,
                import_count: 0,
                is_loaded: false,
            }],
            items: vec![ItemEntry {
                short_name: "factorial".to_owned(),
                fqn: "exec_factorial/main.factorial".to_owned(),
                kind: ItemKind::Fn,
                source_module: "exec_factorial/main".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        };
        let hinter = hinter_with(set);
        assert!(hint_for(&hinter, ":load fac").is_empty());
    }

    #[test]
    fn hinter_empty_for_empty_word() {
        let set = NameSet {
            items: vec![ItemEntry {
                short_name: "foobar".to_owned(),
                fqn: "demo/main.foobar".to_owned(),
                kind: ItemKind::Fn,
                source_module: "demo/main".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        };
        let hinter = hinter_with(set);
        assert!(hint_for(&hinter, ":t foo +").is_empty());
        assert!(hint_for(&hinter, "").is_empty());
    }

    #[test]
    fn hinter_predicts_command_spelling_at_line_start() {
        let hinter = hinter_with(NameSet::default());
        assert_eq!(hint_for(&hinter, ":lo"), "ad");
    }

    #[test]
    fn hinter_predicts_in_scope_identifier_for_bare_expression() {
        // A bare expression line ghost-texts the in-scope identifier: one
        // matching item `factorial`, typing `1 + fac` predicts `torial`.
        let set = NameSet {
            current_module_src: Some("module demo/main; fn factorial(x: .) -> . { x } fn add(x: ., y: .) -> . { x } op _ + _ { impl add; };".to_owned()),
            items: vec![ItemEntry {
                short_name: "factorial".to_owned(),
                fqn: "demo/main.factorial".to_owned(),
                kind: ItemKind::Fn,
                source_module: "demo/main".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        };
        let hinter = hinter_with(set);
        assert_eq!(hint_for(&hinter, "1 + fac"), "torial");
    }

    #[test]
    fn hinter_predicts_in_scope_identifier_for_normalize_argument() {
        let set = NameSet {
            current_module_src: Some("module demo/main; fn factorial(x: .) -> . { x }".to_owned()),
            items: vec![ItemEntry {
                short_name: "factorial".to_owned(),
                fqn: "demo/main.factorial".to_owned(),
                kind: ItemKind::Fn,
                source_module: "demo/main".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        };
        let hinter = hinter_with(set);
        assert_eq!(hint_for(&hinter, ":normalize fac"), "torial");
    }

    #[test]
    fn hinter_predicts_module_path_under_load_shape() {
        let set = NameSet {
            modules: vec![ModuleEntry {
                path: "demo/main".to_owned(),
                item_count: 0,
                import_count: 0,
                is_loaded: false,
            }],
            ..NameSet::default()
        };
        let hinter = hinter_with(set);
        assert_eq!(hint_for(&hinter, ":load demo/"), "main");
        assert_eq!(hint_for(&hinter, ":load demo."), "");
    }

    #[test]
    fn hinter_stores_hint_for_accept_path() {
        let set = NameSet {
            items: vec![ItemEntry {
                short_name: "factorial".to_owned(),
                fqn: "demo/main.factorial".to_owned(),
                kind: ItemKind::Fn,
                source_module: "demo/main".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        };
        let mut hinter = hinter_with(set);
        let shown = handle_no_color(&mut hinter, ":t fac");
        assert_eq!(shown, "torial");
        assert_eq!(hinter.complete_hint(), "torial");
        assert_eq!(hinter.next_hint_token(), "torial");
    }

    #[test]
    fn hinter_dims_the_shown_hint_under_colour() {
        let set = NameSet {
            items: vec![ItemEntry {
                short_name: "factorial".to_owned(),
                fqn: "demo/main.factorial".to_owned(),
                kind: ItemKind::Fn,
                source_module: "demo/main".to_owned(),
                vis_pub: true,
            }],
            ..NameSet::default()
        };
        let mut hinter = hinter_with(set);
        let history = hinter_history();
        let shown = hinter.handle(":t fac", ":t fac".len(), &history, true, "");
        assert!(
            shown.contains("\x1b["),
            "shown hint should be styled: {shown:?}"
        );
        assert!(
            shown.contains("torial"),
            "shown hint should carry the text: {shown:?}"
        );
        assert_eq!(hinter.complete_hint(), "torial");
    }

    /// A throwaway in-memory reedline history for driving `Hinter::handle`
    /// in a test (the trait method needs a `&dyn History`; the REPL's
    /// hinter never reads it, but the signature requires one).
    fn hinter_history() -> reedline::FileBackedHistory {
        reedline::FileBackedHistory::new(10).expect("in-memory history")
    }

    /// Drive `Hinter::handle` with colour off and return the shown hint.
    fn handle_no_color(hinter: &mut ReplHinter, line: &str) -> String {
        let history = hinter_history();
        hinter.handle(line, line.len(), &history, false, "")
    }

    #[test]
    fn hinter_strict_prefix_command_pool_rejects_fuzzy() {
        // The completion *menu* fuzzy-matches (`:srce` finds `:source`),
        // but the hinter's strict-prefix pool does not — the asymmetry is
        // intentional.
        assert!(command_candidates(":srce").is_empty());
        assert!(command_candidates(":lo").contains(&":load".to_owned()));
    }

    #[test]
    fn input_is_complete_balances_brackets() {
        assert!(input_is_complete(""));
        assert!(input_is_complete(":t foo"));
        assert!(input_is_complete(":t foo(x)"));
        assert!(input_is_complete(":normalize if true { a } else { a }"));
        assert!(input_is_complete(
            ":normalize if true {\n  ()\n} else {\n  ()\n}"
        ));
        assert!(!input_is_complete(":normalize if true {"));
        assert!(!input_is_complete(":t foo("));
        assert!(input_is_complete(":t a[b"));
        assert!(input_is_complete(":t foo)"));
    }

    #[test]
    fn input_is_complete_balances_only_structural_forall_binders() {
        assert!(input_is_complete(":t .[A](x) { x }"));
        assert!(input_is_complete(":t .[*F][**G](f) { f }"));
        assert!(!input_is_complete(":t .["));
        assert!(!input_is_complete(":t .[A"));
        assert!(!input_is_complete(":t .[*F"));
        // Unknown operators cannot be guessed here. The validator's context-
        // aware test above covers the same suffix after a loaded `+`.
        assert!(input_is_complete(":t value + .[A"));
        assert!(input_is_complete(":t a [ B"));
        assert!(input_is_complete(":t a [! b"));
        assert!(input_is_complete(":t ["));
        assert!(input_is_complete(":t .[! { [!1 }"));
        assert!(input_is_complete(":t .[ { [1 }"));
        assert!(input_is_complete(":load .[A"));
        assert!(input_is_complete(":source .[A"));
        assert!(!input_is_complete(":type .[A"));
        assert!(!input_is_complete(":norm .[A"));
    }

    #[test]
    fn input_is_complete_handles_string_literals() {
        assert!(input_is_complete(":normalize \"foo {\""));
        assert!(input_is_complete(":normalize \"((([\""));
        assert!(!input_is_complete(":normalize \"foo"));
        assert!(input_is_complete(":normalize \"foo\nbar"));
    }

    #[test]
    fn input_is_complete_treats_unlexable_as_complete() {
        assert!(input_is_complete("\x07"));
    }

    #[test]
    fn is_unterminated_string_at_eof_distinguishes_lex_errors() {
        let eof = lex("\"foo").expect_err("unterminated string at EOF errors");
        assert!(is_unterminated_string_at_eof(&eof));
        let nl = lex("\"foo\nbar").expect_err("newline-broken string errors");
        assert!(!is_unterminated_string_at_eof(&nl));
        let other = lex("\x07").expect_err("control byte errors");
        assert!(!is_unterminated_string_at_eof(&other));
    }

    #[test]
    fn longest_common_completion_handles_each_arity() {
        assert_eq!(longest_common_completion(&[], "fac"), "");
        assert_eq!(
            longest_common_completion(&["factorial".to_owned()], "fac"),
            "torial"
        );
        assert_eq!(
            longest_common_completion(&["add_i32".to_owned(), "add_f32".to_owned()], "add_"),
            ""
        );
        assert_eq!(
            longest_common_completion(&["fold_left".to_owned(), "fold_right".to_owned()], "fo"),
            "ld_"
        );
    }
}
