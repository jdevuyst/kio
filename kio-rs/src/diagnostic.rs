//! Rendering for the structured [`Error`](crate::error::Error)
//! diagnostic — the caret/snippet layout specified in
//! `specs/diagnostics.md`.
//!
//! A diagnostic is drawn as a header (`path:line:col: message`), then a
//! source-snippet block for the primary span and each secondary label
//! (the offending line with a caret/underline beneath it), then a
//! trailer of `help` / `note` / `suggestion` lines. The structured
//! content lives on [`Diagnostic`](crate::error::Diagnostic); this
//! module is purely the presentation layer.
//!
//! The renderer is **in-tree and dependency-free** on purpose. A caret
//! renderer needs only byte→line/col arithmetic and string assembly;
//! pulling `ariadne` / `miette` / `codespan-reporting` would add a tree
//! of transitive deps (and `wasm`-unfriendly ones) for output this
//! module produces in a few hundred lines. Color is emitted by writing
//! ANSI escapes directly — no `termcolor`/`owo-colors` — and is
//! suppressed for non-terminal streams and under `NO_COLOR`, so the
//! styling is incidental and captured output (goldens) is always plain
//! (`specs/diagnostics.md` § Color and TTY).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::error::{Applicability, Diagnostic, Error, SecondaryLabel};
use crate::path_display::display_path_from_root;

#[cfg(test)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct WorkspaceRenderWork {
    display_path_derivations: usize,
    related_source_views: usize,
}

#[cfg(test)]
thread_local! {
    static WORKSPACE_RENDER_WORK: std::cell::Cell<WorkspaceRenderWork> =
        std::cell::Cell::new(WorkspaceRenderWork::default());
}

#[cfg(test)]
fn update_workspace_render_work(update: impl FnOnce(&mut WorkspaceRenderWork)) {
    WORKSPACE_RENDER_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

#[cfg(test)]
fn reset_workspace_render_work() {
    WORKSPACE_RENDER_WORK.with(|work| work.set(WorkspaceRenderWork::default()));
}

#[cfg(test)]
fn workspace_render_work() -> WorkspaceRenderWork {
    WORKSPACE_RENDER_WORK.with(std::cell::Cell::get)
}

pub(crate) struct SecondaryLabelFileGroup<'a> {
    pub(crate) display_file: String,
    pub(crate) file: &'a Path,
    pub(crate) labels: Vec<&'a SecondaryLabel>,
}

pub(crate) struct GroupedSecondaryLabels<'a> {
    pub(crate) primary: Vec<&'a SecondaryLabel>,
    pub(crate) foreign: Vec<SecondaryLabelFileGroup<'a>>,
}

/// Classify exact secondary-label ownership once for both CLI and LSP.
///
/// Primary labels retain insertion order for LSP compatibility. Foreign
/// groups cache their display path before sorting, and labels within one exact
/// file are ordered by span. Callers may apply their own established ordering
/// to the returned primary slice without changing file ownership.
pub(crate) fn group_secondary_labels<'a>(
    labels: &'a [SecondaryLabel],
    primary_file: Option<&Path>,
    display_root: Option<&Path>,
) -> GroupedSecondaryLabels<'a> {
    let mut primary = Vec::new();
    let mut foreign: BTreeMap<&Path, Vec<&SecondaryLabel>> = BTreeMap::new();
    for label in labels {
        match label.file.as_deref() {
            None => primary.push(label),
            Some(file) if primary_file.is_some_and(|primary| file == primary) => {
                primary.push(label);
            }
            Some(file) => foreign.entry(file).or_default().push(label),
        }
    }
    let mut foreign = foreign
        .into_iter()
        .map(|(file, mut labels)| {
            labels.sort_by_key(|label| (label.span.start, label.span.end));
            #[cfg(test)]
            update_workspace_render_work(|work| work.display_path_derivations += 1);
            SecondaryLabelFileGroup {
                display_file: display_path_from_root(file, display_root),
                file,
                labels,
            }
        })
        .collect::<Vec<_>>();
    foreign.sort_by(|left, right| {
        left.display_file
            .cmp(&right.display_file)
            .then_with(|| left.file.cmp(right.file))
    });
    GroupedSecondaryLabels { primary, foreign }
}

/// Whether the renderer should emit ANSI color.
///
/// Color is incidental styling, never load-bearing: the plain-text
/// rendering is unambiguous on its own. We emit color only when the
/// destination is an interactive terminal *and* `NO_COLOR` is unset,
/// per `specs/diagnostics.md` § Color and TTY.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ColorMode {
    Always,
    Never,
}

impl ColorMode {
    /// Resolve the color mode for stderr from the environment: never
    /// when `NO_COLOR` is set (any value) or when stderr is not a
    /// terminal, always otherwise.
    pub fn for_stderr() -> Self {
        use std::io::IsTerminal;
        if std::env::var_os("NO_COLOR").is_some() || !std::io::stderr().is_terminal() {
            ColorMode::Never
        } else {
            ColorMode::Always
        }
    }

    /// Resolve the color mode for stdout — the happy-path status /
    /// result channel (`kio test`, `kio build`, `kio fmt`). Same
    /// `NO_COLOR` + TTY gate as [`for_stderr`](Self::for_stderr), keyed
    /// to stdout's terminal-ness so piped / captured output (every
    /// stdout golden) stays plain.
    pub fn for_stdout() -> Self {
        use std::io::IsTerminal;
        if std::env::var_os("NO_COLOR").is_some() || !std::io::stdout().is_terminal() {
            ColorMode::Never
        } else {
            ColorMode::Always
        }
    }
}

/// Style the inline `` `code` `` spans in a line of CLI prose — the
/// shared styler the diagnostic renderer and the happy-path status
/// output (`kio test`, `kio build`, …) both use, so a backtick run
/// reads identically wherever it appears. Under [`ColorMode::Never`]
/// this is the identity (backticks kept, no escapes); under
/// [`ColorMode::Always`] the run inside each backtick pair is wrapped
/// in the inline-code style, the backticks staying in place.
pub fn style_inline_code(s: &str, color: ColorMode) -> String {
    Palette::new(color).style_code(s)
}

/// Wrap `s` in the success style (bold green) when color is active, or
/// return it unchanged under [`ColorMode::Never`]. For happy-path status
/// words like `pass` / `ok`.
pub fn style_pass(s: &str, color: ColorMode) -> String {
    match color {
        ColorMode::Always => format!("{GREEN}{BOLD}{s}{RESET}"),
        ColorMode::Never => s.to_owned(),
    }
}

/// Wrap `s` in the failure style (bold red) when color is active, or
/// return it unchanged under [`ColorMode::Never`]. For status words like
/// `fail`.
pub fn style_fail(s: &str, color: ColorMode) -> String {
    match color {
        ColorMode::Always => format!("{BOLD_RED}{s}{RESET}"),
        ColorMode::Never => s.to_owned(),
    }
}

// SGR escape sequences. Kept as plain constants — the renderer wraps
// spans in these only when `ColorMode::Always`, so a `ColorMode::Never`
// render contains none of them.
const RESET: &str = "\x1b[0m";
const BOLD_RED: &str = "\x1b[1;31m";
const BOLD: &str = "\x1b[1m";
const BLUE: &str = "\x1b[34m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
// Inline `code` spans in message / help / note prose render in bold
// cyan when color is active, matching the rustc convention. The
// backticks themselves stay in place so the plain-text rendering (and
// every stderr golden, which captures `ColorMode::Never`) is unchanged.
const CODE: &str = "\x1b[1;36m";

/// Render an [`Error`] against the source it was raised in.
///
/// `path` is the display path for the header (already relativized by
/// the caller); `source` is the full text of that file. Returns the
/// multi-line rendering, no trailing newline — the caller adds one (via
/// `eprintln!`) so the output composes with surrounding lines.
pub fn render(path: &str, source: &str, err: &Error, color: ColorMode) -> String {
    render_diagnostic(path, None, source, err.diagnostic(), color, None, None)
}

/// Render an [`Error`] with the exact related sources carried by its
/// file-qualified secondary labels. The existing [`render`] entry point stays
/// the primary-file-only compatibility wrapper; this entry never resolves a
/// label by spelling or reads the filesystem.
pub fn render_with_sources(
    path: &Path,
    source: &str,
    related_sources: &HashMap<PathBuf, String>,
    display_root: Option<&Path>,
    err: &Error,
    color: ColorMode,
) -> String {
    let display_path = display_path_from_root(path, display_root);
    render_diagnostic(
        &display_path,
        Some(path),
        source,
        err.diagnostic(),
        color,
        Some(related_sources),
        display_root,
    )
}

fn render_diagnostic(
    path: &str,
    primary_file: Option<&Path>,
    source: &str,
    diag: &Diagnostic,
    color: ColorMode,
    related_sources: Option<&HashMap<PathBuf, String>>,
    related_display_root: Option<&Path>,
) -> String {
    let c = Palette::new(color);
    let mut out = String::new();

    // Header: `path:line:col: error: message`. The message is bold,
    // with any inline `code` spans in bold cyan (re-opening the bold
    // after each so the rest of the message stays bold).
    let (line, col) = line_col(source, diag.span.start);
    let message = if c.on {
        format!("{BOLD}{}{RESET}", c.style_code_within(&diag.message, BOLD))
    } else {
        diag.message.clone()
    };
    out.push_str(&format!(
        "{path}:{line}:{col}: {}{}{}: {message}",
        c.error, "error", c.reset,
    ));

    // The gutter width is sized to the widest line number any block
    // prints, so every `N |` rail aligns.
    let mut grouped = group_secondary_labels(diag.secondary(), primary_file, related_display_root);
    let max_line = std::iter::once(diag.span)
        .chain(grouped.primary.iter().map(|label| label.span))
        .map(|s| line_col(source, s.start).0)
        .max()
        .unwrap_or(line);
    let gutter = max_line.to_string().len();

    // Primary snippet block (caret underline).
    if let Some(block) = snippet_block(source, diag.span, gutter, None, Underline::Primary, &c) {
        out.push('\n');
        out.push_str(&block);
    }

    // Secondary snippet blocks, in source order.
    grouped
        .primary
        .sort_by_key(|label| (label.span.start, label.span.end));
    for label in grouped.primary {
        if let Some(block) = snippet_block(
            source,
            label.span,
            gutter,
            Some(&label.text),
            Underline::Secondary,
            &c,
        ) {
            out.push('\n');
            out.push_str(&block);
        }
    }

    // Cross-file blocks are grouped by exact file identity, then ordered by
    // cached display path, exact-path tie-break, and span. Each group uses its
    // own source bytes and gutter; a missing or malformed related source is
    // named but never remapped onto the primary file.
    for SecondaryLabelFileGroup {
        display_file,
        file,
        labels,
    } in grouped.foreign
    {
        #[cfg(test)]
        update_workspace_render_work(|work| work.related_source_views += 1);
        let related_source = related_sources
            .and_then(|sources| sources.get(file))
            .map(String::as_str);
        let related_gutter = related_source
            .map(|source| {
                labels
                    .iter()
                    .filter(|label| span_fits_source(source, label.span))
                    .map(|label| line_col(source, label.span.start).0)
                    .max()
                    .unwrap_or(1)
                    .to_string()
                    .len()
            })
            .unwrap_or(1);
        let mut wrote_location = false;
        for label in labels {
            let Some(related_source) =
                related_source.filter(|source| span_fits_source(source, label.span))
            else {
                out.push('\n');
                out.push_str(&format!(
                    "  --> {display_file}: source unavailable: {}",
                    c.style_code(&label.text),
                ));
                continue;
            };
            if !wrote_location {
                let (related_line, related_col) = line_col(related_source, label.span.start);
                out.push('\n');
                out.push_str(&format!(
                    "  --> {display_file}:{related_line}:{related_col}"
                ));
                wrote_location = true;
            }
            if let Some(block) = snippet_block(
                related_source,
                label.span,
                related_gutter,
                Some(&label.text),
                Underline::Secondary,
                &c,
            ) {
                out.push('\n');
                out.push_str(&block);
            }
        }
    }

    // Trailer: help, notes, suggestion. Each on its own `= kind: …`
    // line, aligned under the gutter.
    let rail = format!("{} = ", " ".repeat(gutter));
    if let Some(help) = diag.help() {
        out.push('\n');
        out.push_str(&format!(
            "{rail}{}help{}: {}",
            c.help,
            c.reset,
            c.style_code(help)
        ));
    }
    for note in diag.notes() {
        out.push('\n');
        out.push_str(&format!(
            "{rail}{}note{}: {}",
            c.note,
            c.reset,
            c.style_code(note)
        ));
    }
    if let Some(fix) = diag.fixes().iter().find(|fix| {
        fix.applicability == Applicability::MachineApplicable
            && fix.edits.len() == 1
            && fix.edits[0].file.is_none()
            && fix.edits[0].replacement_parts.is_empty()
    }) && let Some(edit) = fix.edits.first()
    {
        out.push('\n');
        out.push_str(&format!(
            "{rail}{}suggestion{}: replace with `{}`",
            c.help,
            c.reset,
            c.bold(&edit.replacement)
        ));
    }

    out
}

fn span_fits_source(source: &str, span: crate::span::Span) -> bool {
    let start = span.start as usize;
    let end = span.end as usize;
    start <= end
        && end <= source.len()
        && source.is_char_boundary(start)
        && source.is_char_boundary(end)
}

#[derive(Copy, Clone)]
enum Underline {
    Primary,
    Secondary,
}

/// Render one source-line block: the offending line on a numbered
/// gutter rail, then an underline rail with a caret (`^`) or secondary
/// dash (`-`) run beneath the span, with the optional label trailing.
///
/// Returns `None` when the span's start line cannot be located in the
/// source (an empty source, or an out-of-range span), in which case the
/// header alone carries the diagnostic.
fn snippet_block(
    source: &str,
    span: crate::span::Span,
    gutter: usize,
    label: Option<&str>,
    kind: Underline,
    c: &Palette,
) -> Option<String> {
    if source.is_empty() {
        return None;
    }
    let (line_no, start_col) = line_col(source, span.start);
    let line_text = nth_line(source, line_no)?;

    // The underline runs from the span start to the span end, clamped
    // to this line's content (a multi-line span underlines to its first
    // line's end; the header line/col already named the actionable
    // start). Source columns count scalars; tabs expand only for display.
    let line_chars = line_text.chars().count();
    let end_col = {
        let (end_line, end_c) = line_col(source, span.end);
        if end_line == line_no {
            end_c
        } else {
            line_chars + 1
        }
    };
    let (line_text, pad, underline_end) = snippet_columns(
        line_text,
        start_col.saturating_sub(1),
        end_col.saturating_sub(1),
    );
    let underline_len = underline_end.saturating_sub(pad).max(1);

    let (mark, paint): (char, &str) = match kind {
        Underline::Primary => ('^', c.error),
        Underline::Secondary => ('-', c.label),
    };
    let marks: String = std::iter::repeat_n(mark, underline_len).collect();

    let blank_gutter = " ".repeat(gutter);
    let num = format!("{:>width$}", line_no, width = gutter);

    let mut block = String::new();
    // ` | ` separator line above the snippet keeps blocks visually
    // distinct and matches the rustc-style frame.
    block.push_str(&format!("{} {}|{}\n", blank_gutter, c.rail, c.reset));
    block.push_str(&format!(
        "{}{}{} {}|{} {}\n",
        c.rail, num, c.reset, c.rail, c.reset, line_text
    ));
    block.push_str(&format!(
        "{} {}|{} {}{}{}{}",
        blank_gutter,
        c.rail,
        c.reset,
        " ".repeat(pad),
        paint,
        marks,
        c.reset
    ));
    if let Some(label) = label {
        block.push_str(&format!(
            " {paint}{}{}",
            c.style_code_within(label, paint),
            c.reset
        ));
    }
    Some(block)
}

fn snippet_columns(line: &str, start: usize, end: usize) -> (Cow<'_, str>, usize, usize) {
    if !line.contains('\t') {
        return (Cow::Borrowed(line), start, end);
    }
    // Explicit spaces keep source and underline aligned independently of the
    // terminal's tab stops and the width of the line-number gutter.
    let mut expanded = String::with_capacity(line.len());
    let mut display_column = 0;
    let mut display_start = 0;
    let mut display_end = 0;
    for (source_column, character) in line.chars().enumerate() {
        let width = if character == '\t' {
            4 - display_column % 4
        } else {
            1
        };
        if source_column < start {
            display_start += width;
        }
        if source_column < end {
            display_end += width;
        }
        if character == '\t' {
            expanded.extend(std::iter::repeat_n(' ', width));
        } else {
            expanded.push(character);
        }
        display_column += width;
    }
    (Cow::Owned(expanded), display_start, display_end)
}

/// ANSI styling, parameterized on whether color is active. Under
/// [`ColorMode::Never`] every field is the empty string, so the render
/// contains no escape sequences at all.
struct Palette {
    on: bool,
    reset: &'static str,
    error: &'static str,
    label: &'static str,
    rail: &'static str,
    help: &'static str,
    note: &'static str,
    code: &'static str,
}

impl Palette {
    fn new(mode: ColorMode) -> Self {
        let on = matches!(mode, ColorMode::Always);
        let pick = |s: &'static str| if on { s } else { "" };
        Self {
            on,
            reset: pick(RESET),
            error: pick(BOLD_RED),
            label: pick(BLUE),
            rail: pick(CYAN),
            help: pick(GREEN),
            note: pick(BLUE),
            code: pick(CODE),
        }
    }

    fn bold(&self, s: &str) -> String {
        if self.on {
            format!("{BOLD}{s}{RESET}")
        } else {
            s.to_owned()
        }
    }

    /// Style inline `` `code` `` spans in diagnostic prose. When color
    /// is off this is the identity (the backticks are the plain-text
    /// delimiter the spec's rendered layout keeps, and what every stderr
    /// golden captures). When color is on, the run *inside* each pair of
    /// backticks is wrapped in [`CODE`]; the backticks stay so the cue is
    /// legible even where the color does not survive (a pager, a copy).
    /// A lone unmatched backtick is left verbatim.
    fn style_code(&self, s: &str) -> String {
        self.style_code_within(s, "")
    }

    /// As [`style_code`](Self::style_code), but `surround` is the SGR
    /// run that styles the prose *around* the code spans (e.g. the bold
    /// of the header message). Each inline-code reset re-opens `surround`
    /// so the surrounding style resumes after the code run, rather than
    /// the inner reset clearing it for the remainder of the line.
    fn style_code_within(&self, s: &str, surround: &str) -> String {
        if !self.on || !s.contains('`') {
            return s.to_owned();
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(open) = rest.find('`') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            match after.find('`') {
                Some(close) => {
                    out.push('`');
                    out.push_str(self.code);
                    out.push_str(&after[..close]);
                    out.push_str(self.reset);
                    out.push_str(surround);
                    out.push('`');
                    rest = &after[close + 1..];
                }
                None => {
                    out.push('`');
                    out.push_str(after);
                    return out;
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// Map a byte offset to a 1-based (line, column), counting columns in
/// Unicode scalar values. Mirrors the column convention the CLI header
/// has always used (see `cmd::check`).
fn line_col(source: &str, offset: u32) -> (usize, usize) {
    let upto = (offset as usize).min(source.len());
    let prefix = &source[..upto];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = source[line_start..upto].chars().count() + 1;
    (line, col)
}

/// The text of the 1-based `line_no`-th line of `source`, without its
/// terminating newline. `None` when the source has fewer lines.
fn nth_line(source: &str, line_no: usize) -> Option<&str> {
    source.split('\n').nth(line_no - 1).map(|l| {
        // Strip a trailing `\r` so CRLF sources underline correctly.
        l.strip_suffix('\r').unwrap_or(l)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;

    fn type_err(span: Span, msg: &str) -> Error {
        Error::type_(span, msg.to_owned())
    }

    #[test]
    fn plain_render_has_no_escapes() {
        let src = "fn id(x: Foo) -> Foo { x }\nfn wrong(z: Foo) -> Foo { id }\n";
        // `id` on line 2 (the bare function value).
        let start = src.find("id }").unwrap() as u32;
        let err = type_err(Span::new(start, start + 2), "type mismatch: expected `Foo`");
        let out = render("main.kio", src, &err, ColorMode::Never);
        assert!(
            !out.contains('\x1b'),
            "plain render must carry no ANSI: {out:?}"
        );
        assert!(out.starts_with("main.kio:2:27: error: type mismatch"));
        assert!(out.contains("2 | fn wrong(z: Foo) -> Foo { id }"));
        assert!(out.contains("^^"), "caret run under the span: {out}");
    }

    #[test]
    fn color_render_has_escapes() {
        let src = "fn x() {}\n";
        let err = type_err(Span::new(3, 4), "boom");
        let out = render("m.kio", src, &err, ColorMode::Always);
        assert!(out.contains('\x1b'), "colored render must carry ANSI");
    }

    #[test]
    fn secondary_label_renders_with_dashes() {
        let src = "fn id(x: Foo) -> Foo { x }\nfn wrong(z: Foo) -> Foo { id }\n";
        let prim = src.find("id }").unwrap() as u32;
        let bind = src.find("id(x").unwrap_or(1) as u32;
        // The binding span covers `id(x` — a four-scalar run, so the
        // secondary underline is four dashes wide.
        let err = type_err(Span::new(prim, prim + 2), "type mismatch")
            .with_secondary(Span::new(bind, bind + 4), "defined here");
        let out = render("main.kio", src, &err, ColorMode::Never);
        assert!(
            out.contains("---- defined here"),
            "secondary dash run + label: {out}"
        );
    }

    #[test]
    fn tabbed_snippets_align_primary_and_secondary_spans() {
        for prefix in ["\t", " \t", "\t\t", "a\t", "a\t \t"] {
            let source = format!("{prefix}()\n{prefix}W\n");
            let primary = source.find("()").unwrap() as u32;
            let secondary = source.find('W').unwrap() as u32;
            let error = type_err(Span::new(primary, primary + 2), "type mismatch")
                .with_secondary(Span::new(secondary, secondary + 1), "annotation");
            let output = render("main.kio", &source, &error, ColorMode::Never);
            assert!(output.starts_with(&format!("main.kio:1:{}:", prefix.len() + 1)));
            assert!(
                !output.contains('\t'),
                "terminal tab stops must not affect excerpts: {output}"
            );
            let lines = output.lines().collect::<Vec<_>>();
            for (target, marker) in [("()", "^^"), ("W", "- annotation")] {
                let row = lines.iter().position(|line| line.contains(target)).unwrap();
                assert_eq!(
                    lines[row].find(target),
                    lines[row + 1].find(marker),
                    "underline must align with its source for {prefix:?}: {output}",
                );
            }
        }
    }

    #[test]
    fn tabbed_snippets_expand_tabs_inside_single_and_multiline_spans() {
        let source = "\t(\t)\n\t[\t]\n";
        let secondary = source.find('[').unwrap() as u32;
        for end in [4, source.len() as u32] {
            let error = type_err(Span::new(1, end), "type mismatch")
                .with_secondary(Span::new(secondary, secondary + 3), "annotation");
            let output = render("main.kio", source, &error, ColorMode::Never);
            assert!(output.contains("1 |     (   )\n  |     ^^^^^"), "{output}");
            assert!(
                output.contains("2 |     [   ]\n  |     ----- annotation"),
                "{output}"
            );
        }
    }

    #[test]
    fn source_aware_render_preserves_same_file_output() {
        let src = "fn id(x: Foo) -> Foo { x }\nfn wrong(z: Foo) -> Foo { id }\n";
        let primary = src.find("id }").unwrap() as u32;
        let secondary = src.find("id(x").unwrap() as u32;
        let err = type_err(Span::new(primary, primary + 2), "type mismatch")
            .with_secondary(Span::new(secondary, secondary + 2), "defined here");
        let sources = HashMap::from([(PathBuf::from("unrelated.kio"), String::new())]);

        assert_eq!(
            render("main.kio", src, &err, ColorMode::Never),
            render_with_sources(
                Path::new("main.kio"),
                src,
                &sources,
                None,
                &err,
                ColorMode::Never,
            ),
        );
    }

    #[test]
    fn source_aware_render_uses_the_related_file_bytes() {
        let caller = "fn use(value: Wrap(_)) { value }\n";
        let provider = "pub type Wrap[T] = [A] T -> A;\n";
        let primary = caller.find('_').unwrap() as u32;
        let binder = provider.find('A').unwrap() as u32;
        let provider_path = PathBuf::from("provider.kio");
        let err = type_err(Span::new(primary, primary + 1), "invalid placeholder")
            .with_secondary_in_file(
                provider_path.clone(),
                Span::new(binder, binder + 1),
                "this annotation introduces the enclosing binder",
            );
        let sources = HashMap::from([(provider_path, provider.to_owned())]);

        let out = render_with_sources(
            Path::new("caller.kio"),
            caller,
            &sources,
            None,
            &err,
            ColorMode::Never,
        );
        assert!(out.contains("--> provider.kio:1:21"), "{out}");
        assert!(out.contains("1 | pub type Wrap[T] = [A] T -> A;"), "{out}");
        assert!(
            out.contains("- this annotation introduces the enclosing binder"),
            "{out}"
        );
    }

    #[test]
    fn source_aware_render_never_maps_a_missing_source_to_primary_bytes() {
        let caller = "fn use(value: Wrap(_)) { value }\n";
        let primary = caller.find('_').unwrap() as u32;
        let err = type_err(Span::new(primary, primary + 1), "invalid placeholder")
            .with_secondary_in_file("missing-provider.kio", Span::new(0, 1), "provider binder");

        let out = render_with_sources(
            Path::new("caller.kio"),
            caller,
            &HashMap::new(),
            None,
            &err,
            ColorMode::Never,
        );
        assert!(
            out.contains("--> missing-provider.kio: source unavailable: provider binder"),
            "{out}"
        );
        assert_eq!(
            out.matches("1 | fn use(value: Wrap(_)) { value }").count(),
            1
        );
    }

    #[test]
    fn workspace_renderer_groups_exact_provider_labels() {
        let caller = "fn caller() { bad }\n";
        let alpha = "first alpha\nsecond alpha\n";
        let beta = "only beta\n";
        let caller_path = PathBuf::from("pkg/caller.kio");
        let alpha_path = PathBuf::from("pkg/alpha.kio");
        let beta_path = PathBuf::from("pkg/beta.kio");
        let err = type_err(Span::new(14, 17), "invalid placeholder")
            .with_secondary_in_file(beta_path.clone(), Span::new(0, 4), "beta label")
            .with_secondary_in_file(alpha_path.clone(), Span::new(12, 18), "second alpha label")
            .with_secondary_in_file(alpha_path.clone(), Span::new(0, 5), "first alpha label")
            .with_secondary_in_file(caller_path.clone(), Span::new(3, 9), "caller label");
        let sources = HashMap::from([(beta_path, beta.to_owned()), (alpha_path, alpha.to_owned())]);

        reset_workspace_render_work();
        let out = render_with_sources(
            &caller_path,
            caller,
            &sources,
            Some(Path::new("pkg")),
            &err,
            ColorMode::Never,
        );
        let alpha_header = out.find("--> alpha.kio:1:1").expect("alpha header");
        let caller_label = out.find("caller label").expect("caller label");
        let first_alpha = out.find("first alpha label").expect("first alpha label");
        let second_alpha = out.find("second alpha label").expect("second alpha label");
        let beta_header = out.find("--> beta.kio:1:1").expect("beta header");
        assert!(caller_label < alpha_header, "{out}");
        assert!(alpha_header < first_alpha && first_alpha < second_alpha);
        assert!(second_alpha < beta_header, "{out}");
        assert_eq!(out.matches("--> alpha.kio:").count(), 1, "{out}");
        assert_eq!(out.matches("--> beta.kio:").count(), 1, "{out}");
        assert_eq!(
            workspace_render_work(),
            WorkspaceRenderWork {
                display_path_derivations: 2,
                related_source_views: 2,
            },
        );
    }

    #[test]
    fn source_aware_render_preserves_related_blocks_when_primary_source_is_missing() {
        let provider = "pub type Wrap[T] = [A] T -> A;\n";
        let binder = provider.find('A').expect("binder") as u32;
        let provider_path = PathBuf::from("provider.kio");
        let err = type_err(Span::new(0, 1), "invalid placeholder").with_secondary_in_file(
            provider_path.clone(),
            Span::new(binder, binder + 1),
            "provider binder",
        );
        let sources = HashMap::from([(provider_path, provider.to_owned())]);

        let out = render_with_sources(
            Path::new("caller.kio"),
            "",
            &sources,
            None,
            &err,
            ColorMode::Never,
        );
        assert!(out.starts_with("caller.kio:1:1: error: invalid placeholder"));
        assert!(
            !out.contains("1 | \n"),
            "the missing primary has no snippet: {out}"
        );
        assert!(out.contains("--> provider.kio:1:"), "{out}");
        assert!(out.contains("pub type Wrap[T] = [A] T -> A;"), "{out}");
        assert!(out.contains("provider binder"), "{out}");
    }

    #[test]
    fn exact_primary_file_label_uses_primary_bytes() {
        let source = "fn caller() { bad }\n";
        let err = type_err(Span::new(14, 17), "invalid placeholder").with_secondary_in_file(
            "pkg/caller.kio",
            Span::new(3, 9),
            "same file",
        );

        let out = render_with_sources(
            Path::new("pkg/caller.kio"),
            source,
            &HashMap::new(),
            Some(Path::new("pkg")),
            &err,
            ColorMode::Never,
        );
        assert!(out.contains("same file"), "{out}");
        assert!(!out.contains("source unavailable"), "{out}");
        assert!(!out.contains("--> caller.kio:"), "{out}");
    }

    #[test]
    fn trailer_renders_help_note_suggestion() {
        let src = "fn x() {}\n";
        let span = Span::new(3, 4);
        let err = type_err(span, "boom")
            .with_help("try this instead")
            .with_note("for context")
            .with_suggestion(span, "y");
        let out = render("m.kio", src, &err, ColorMode::Never);
        assert!(out.contains("= help: try this instead"), "{out}");
        assert!(out.contains("= note: for context"), "{out}");
        assert!(out.contains("= suggestion: replace with `y`"), "{out}");
    }

    #[test]
    fn plain_render_keeps_backticks_unstyled() {
        // Inline `code` spans stay as bare backticks in plain text — the
        // stderr-golden contract — with no escape sequences injected.
        let src = "fn x() {}\n";
        let err = type_err(Span::new(3, 4), "expected `Foo`, found `Bar`")
            .with_help("declare a host type marked `role(bool)`");
        let out = render("m.kio", src, &err, ColorMode::Never);
        assert!(
            !out.contains('\x1b'),
            "plain render carries no ANSI: {out:?}"
        );
        assert!(out.contains("expected `Foo`, found `Bar`"), "{out}");
        assert!(
            out.contains("= help: declare a host type marked `role(bool)`"),
            "{out}"
        );
    }

    #[test]
    fn color_render_styles_inline_code() {
        // Under color the run *inside* each backtick pair is wrapped in
        // the code SGR; the backticks themselves stay.
        let src = "fn x() {}\n";
        let err = type_err(Span::new(3, 4), "expected `Foo`");
        let out = render("m.kio", src, &err, ColorMode::Always);
        assert!(
            out.contains(&format!("`{CODE}Foo{RESET}")),
            "styled code span: {out:?}"
        );
    }

    #[test]
    fn color_render_resumes_bold_after_inline_code() {
        // The header message is bold; a code span in the middle must not
        // leave the tail of the message unbolded — the inner reset
        // re-opens BOLD.
        let src = "fn x() {}\n";
        let err = type_err(Span::new(3, 4), "found `Bar` here");
        let out = render("m.kio", src, &err, ColorMode::Always);
        // After the code span's reset, BOLD re-opens before ` here`.
        assert!(
            out.contains(&format!("{RESET}{BOLD}` here")),
            "bold resumes: {out:?}"
        );
    }

    #[test]
    fn unmatched_backtick_left_verbatim() {
        let src = "fn x() {}\n";
        let err = type_err(Span::new(3, 4), "stray ` backtick");
        let out = render("m.kio", src, &err, ColorMode::Always);
        assert!(out.contains("stray ` backtick"), "{out}");
    }

    #[test]
    fn empty_source_renders_header_only() {
        let err = type_err(Span::new(0, 0), "no source here");
        let out = render("m.kio", "", &err, ColorMode::Never);
        assert_eq!(out, "m.kio:1:1: error: no source here");
    }

    #[test]
    fn multibyte_columns_count_scalars() {
        // `é` is two bytes; the caret must sit at the scalar column.
        let src = "// é marks\nfn x() {}\n";
        let span = Span::new(
            src.find("fn").unwrap() as u32,
            src.find("fn").unwrap() as u32 + 2,
        );
        let err = type_err(span, "boom");
        let out = render("m.kio", src, &err, ColorMode::Never);
        assert!(out.contains("2 | fn x() {}"), "{out}");
    }
}
