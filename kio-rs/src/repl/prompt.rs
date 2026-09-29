//! The reedline [`Prompt`] for the `kio repl` inspector.
//!
//! [`KioPrompt`] renders the module-aware **two-line** prompt: the
//! current module's slash path on its own line above a bare `kio> `
//! input symbol —
//!
//! ```text
//! exec_factorial/main
//! kio> _
//! ```
//!
//! — or, when no module is current, a dim guidance line above the
//! symbol:
//!
//! ```text
//! (no module — :load <module-path> to begin)
//! kio> _
//! ```
//!
//! reedline owns the [`Prompt`] (it reads it on every redraw), so the
//! prompt cannot hold a `&Session`; instead it reads a shared
//! `Arc<Mutex<Option<String>>>` current-module cell the REPL loop
//! refreshes after each state-changing command (see
//! [`refresh_session_state`](super::refresh_session_state)).
//!
//! The context line is styled from the same per-[`Palette`] table the
//! classifier output and the live input highlighter use
//! ([`crate::repl_core::highlight`]): the module name renders in the
//! `entity.name.module` colour, so it matches its appearance
//! everywhere else in the REPL, and the no-module hint renders dim.
//! Every escape the context line opens is closed with
//! [`highlight::RESET`] before the trailing newline, so the input line
//! picks up the input highlighter's colours cleanly, with no residual
//! prompt colour leaking onto what the user types.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use reedline::{Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};

use crate::repl_core::highlight::{self, Palette};
use crate::tokens::TokenKind;

/// The input symbol on the prompt's second line — a bare `kio> `, no
/// module decoration (the module name lives on the line above).
const INPUT_SYMBOL: &str = "kio> ";

/// The maximum width, in characters, of the module path shown on the
/// context line. A longer slash path
/// (`abcdef/ghijkl/mnopqr/stuvwx/…`) would wrap on a narrow terminal
/// and push the input symbol off the visible line; past this width the
/// leading segments are elided with a `…`. The full path is always
/// available via `:mods`.
const MAX_MODULE_PATH_WIDTH: usize = 60;

/// The shared current-module cell type: the slash path of the current
/// module, or `None` when none is current. The REPL loop owns one and
/// hands a clone to [`KioPrompt`]; the loop overwrites the inner value
/// after every state-changing command so the next prompt render
/// reflects the new current module.
pub type CurrentModule = Arc<Mutex<Option<String>>>;

/// The module-aware reedline prompt. Reads the shared current-module
/// cell on every render; the context line above the `kio> ` symbol
/// tells the user which scope a bare-name query resolves against.
pub struct KioPrompt {
    current_module: CurrentModule,
    /// The active palette — the [`Palette::detect`] result the REPL
    /// loop computed at startup, so the prompt's module-name colour
    /// matches the classifier and live-highlighter output. A
    /// [`Palette::Plain`] palette renders the two lines with no
    /// escapes (the smoke / unit tests assert on the plain shape).
    palette: Palette,
}

impl KioPrompt {
    /// Construct a prompt backed by the shared current-module cell,
    /// rendering its context line under `palette`.
    pub fn new(current_module: CurrentModule, palette: Palette) -> Self {
        Self {
            current_module,
            palette,
        }
    }

    /// The slash module path, truncated to [`MAX_MODULE_PATH_WIDTH`]
    /// characters by eliding leading segments with a `…` so a long path
    /// never wraps the prompt. A path within the cap is returned
    /// unchanged.
    fn truncate_module_path(path: &str) -> Cow<'_, str> {
        if path.chars().count() <= MAX_MODULE_PATH_WIDTH {
            return Cow::Borrowed(path);
        }
        // Keep the trailing `MAX - 1` characters (room for the leading
        // `…`), then snap the cut to the next char boundary so we never
        // split a multibyte char. `char_indices` gives boundary offsets.
        let keep = MAX_MODULE_PATH_WIDTH.saturating_sub(1);
        let total = path.chars().count();
        let skip = total - keep;
        let cut = path
            .char_indices()
            .nth(skip)
            .map(|(i, _)| i)
            .unwrap_or(path.len());
        Cow::Owned(format!("…{}", &path[cut..]))
    }

    /// The styled context line for a current module named `path`: the
    /// (possibly truncated) module path in the `entity.name.module`
    /// colour, closed by [`highlight::RESET`]. Under [`Palette::Plain`]
    /// the path is returned bare (no escapes).
    fn styled_module_name(&self, path: &str) -> String {
        let shown = Self::truncate_module_path(path);
        match self.palette.style_for_kind(TokenKind::EntityNameModule) {
            Some(sgr) => format!("{sgr}{shown}{reset}", reset = highlight::RESET),
            None => shown.into_owned(),
        }
    }

    /// The styled context line shown when no module is current: a dim
    /// hint pointing at `:load`, closed by [`highlight::RESET`]. Under
    /// [`Palette::Plain`] the hint is returned bare (no escapes).
    fn styled_no_module_hint(&self) -> String {
        const HINT: &str = "(no module — :load <module-path> to begin)";
        match self.palette {
            Palette::Plain => HINT.to_owned(),
            _ => format!(
                "{dim}{HINT}{reset}",
                dim = highlight::DIM,
                reset = highlight::RESET
            ),
        }
    }
}

impl Prompt for KioPrompt {
    /// The left prompt: a two-line shape — the current module's slash
    /// path (or the dim no-module hint) on its own line, then the bare
    /// `kio> ` input symbol. A poisoned lock falls back to the
    /// no-module hint rather than panicking; the prompt is
    /// presentational, never load-bearing.
    fn render_prompt_left(&self) -> Cow<'_, str> {
        let context = match self.current_module.lock() {
            Ok(guard) => match guard.as_deref() {
                Some(path) => self.styled_module_name(path),
                None => self.styled_no_module_hint(),
            },
            Err(_) => self.styled_no_module_hint(),
        };
        Cow::Owned(format!("{context}\n{INPUT_SYMBOL}"))
    }

    /// No right prompt — the two-line shape carries everything on the
    /// left.
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    /// No edit-mode indicator — the context line above the input symbol
    /// is the only prompt decoration this REPL ships.
    fn render_prompt_indicator(&self, _edit_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    /// The multiline continuation indicator. Single-line input never
    /// reaches a continuation today (the validator reports every line
    /// [`Complete`](reedline::ValidationResult::Complete)); the
    /// conventional `... ` is in place for the multi-line session.
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("... ")
    }

    /// The reverse-history-search indicator, mirroring reedline's
    /// default `(reverse-search: …)` framing so Ctrl-R reads naturally.
    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!(
            "({}reverse-search: {}) ",
            prefix, history_search.term
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prompt over a fresh current-module cell holding `current`,
    /// rendered under `palette`.
    fn prompt(current: Option<&str>, palette: Palette) -> KioPrompt {
        let cell: CurrentModule = Arc::new(Mutex::new(current.map(str::to_owned)));
        KioPrompt::new(cell, palette)
    }

    /// The rendered left prompt for `current` under `palette`.
    fn rendered(current: Option<&str>, palette: Palette) -> String {
        prompt(current, palette).render_prompt_left().into_owned()
    }

    #[test]
    fn prompt_renders_two_lines_with_module_name() {
        // A current module renders its slash path on the first line,
        // the bare `kio> ` symbol on the second — newline-separated,
        // with no escapes under the plain palette.
        let out = rendered(Some("exec_factorial/main"), Palette::plain());
        assert_eq!(out, "exec_factorial/main\nkio> ");
    }

    #[test]
    fn prompt_renders_no_module_hint_when_unset() {
        // No current module → the dim guidance line above the symbol.
        let out = rendered(None, Palette::plain());
        assert!(
            out.contains("(no module —"),
            "the no-module hint should render, got: {out:?}"
        );
        assert!(
            out.ends_with("\nkio> "),
            "the input symbol should sit on the second line, got: {out:?}"
        );
    }

    #[test]
    fn prompt_colors_module_name_when_palette_is_active() {
        // Under a colour palette the module name is wrapped in an SGR
        // escape and closed with the reset, so it matches the
        // classifier's `entity.name.module` colour elsewhere.
        let out = rendered(Some("exec_factorial/main"), Palette::truecolor());
        assert!(
            out.contains("\x1b["),
            "module name should be styled: {out:?}"
        );
        assert!(
            out.contains(highlight::RESET),
            "the styled run should be reset before the input line: {out:?}"
        );
        // The reset closes the colour before the newline, so the input
        // line inherits nothing.
        let reset_then_newline = format!("{}\n{INPUT_SYMBOL}", highlight::RESET);
        assert!(
            out.ends_with(&reset_then_newline),
            "the escape must close before the newline, got: {out:?}"
        );
        // The path text is still present, unbroken.
        assert!(out.contains("exec_factorial/main"), "got: {out:?}");
    }

    #[test]
    fn prompt_dims_the_no_module_hint_under_colour() {
        // The no-module hint is de-emphasised guidance — rendered dim
        // and reset under a colour palette.
        let out = rendered(None, Palette::truecolor());
        assert!(out.contains(highlight::DIM), "hint should be dim: {out:?}");
        assert!(out.contains(highlight::RESET), "hint should reset: {out:?}");
        assert!(out.ends_with(&format!("{}\n{INPUT_SYMBOL}", highlight::RESET)));
    }

    #[test]
    fn long_module_path_is_truncated_with_ellipsis() {
        // A path past the width cap elides its leading segments so the
        // input symbol stays on the line; the tail (the most specific
        // segments) survives.
        let long = "abcdef.ghijkl.mnopqr.stuvwx.yzabcd.efghij.klmnop.qrstuv.wxyzab.cdefgh.ijklmn";
        assert!(long.chars().count() > MAX_MODULE_PATH_WIDTH);
        let out = rendered(Some(long), Palette::plain());
        let context = out.strip_suffix(&format!("\n{INPUT_SYMBOL}")).unwrap();
        assert!(
            context.starts_with('…'),
            "should elide the head: {context:?}"
        );
        assert!(
            context.ends_with("ijklmn"),
            "the tail (most-specific segments) should survive: {context:?}"
        );
        // The shown context fits within the cap (the `…` plus the kept
        // tail).
        assert!(
            context.chars().count() <= MAX_MODULE_PATH_WIDTH,
            "truncated path should fit the cap, got {} chars: {context:?}",
            context.chars().count()
        );
    }

    #[test]
    fn module_path_at_the_cap_is_not_truncated() {
        // A path exactly at the cap renders verbatim — no `…`.
        let at_cap = "a".repeat(MAX_MODULE_PATH_WIDTH);
        let out = rendered(Some(&at_cap), Palette::plain());
        assert_eq!(out, format!("{at_cap}\n{INPUT_SYMBOL}"));
    }
}
