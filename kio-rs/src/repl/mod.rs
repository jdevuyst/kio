//! `kio repl` — the typecheck-only module inspector.
//!
//! `kio repl` opens an interactive prompt for inspecting the Kio
//! modules in a directory: load them by module path, query types, read
//! doc-comments, print canonical source, browse, and navigate
//! cross-references. It is a thin presentation layer over the Rust
//! frontend, the LSP analysis services, and the Kiodoc renderer — it
//! runs each module through parse / desugar / label lowering / typecheck /
//! elaborator-discharge, but never executes it against a host.
//!
//! Each turn is either a `:`-prefixed meta-command or a bare Kio
//! expression. A bare *name* routes to `:doc` (its doc-comment +
//! signature); a bare *compound expression* routes to `:normalize`
//! (its residual normal form). Either way a dim footer advertises the
//! other commands applicable to the input. The REPL never runs code —
//! an expression query returns the compiler's normalized residual form.
//!
//! ## Shared Core
//!
//! - [`crate::repl_core::session`] — the loaded-module set, the current pointer, and
//!   the cached whole-package analysis.
//! - [`crate::repl_core::commands`] — meta-command parsing, the synonym table, and the
//!   per-command handlers.
//! - [`crate::repl_core::expr_query`] — the expression-query entry point:
//!   wraps a queried expression in a synthetic function, type-checks
//!   it through an overlay, and returns the typed `Prime` AST.
//! - [`crate::repl_core::highlight`] — Kio-aware ANSI highlighting of
//!   REPL output.
//!
//! ## Terminal Submodules
//!
//! - [`completion`] — the reedline [`Completer`](reedline::Completer)
//!   / [`Highlighter`](reedline::Highlighter) /
//!   [`Hinter`](reedline::Hinter) / [`Validator`](reedline::Validator)
//!   driving tab completion and live input highlighting.
//! - [`prompt`] — the reedline [`Prompt`](reedline::Prompt) rendering
//!   the module-aware `kio[<module>]> ` prompt.
//! - [`watch`] — the debounced package-directory watcher behind
//!   auto-reload.
//!
//! ## Loop
//!
//! 1. Read the current directory; the REPL runs on its module tree
//!    regardless of whether a `*.pkg.kio` package file is present.
//! 2. Branch on whether stdin is a terminal. An **interactive** stdin
//!    opens reedline with the completer / highlighter / hinter /
//!    validator installed and history loaded; a **piped** stdin (a
//!    transcript fed over a pipe — smoke tests, CI harnesses, agents
//!    driving the REPL) reads lines with `BufRead` directly, since
//!    reedline unconditionally tries to put the terminal in raw mode
//!    and errors on a pipe.
//! 3. Read a line. Once a line is in hand — but before the command
//!    runs — drain the auto-reload watcher: a `.kio` file that
//!    changed while the user was at the prompt is re-typechecked
//!    first, so the command sees current contents.
//! 4. Parse the line into a [`commands::Command`] and run it. Both
//!    branches share the [`dispatch_line`] helper so they cannot
//!    drift.
//! 5. Loop until `:quit`, Ctrl-D, or EOF.

pub mod completion;
pub mod prompt;
pub mod watch;

use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use reedline::{
    DescriptionMenu, Emacs, FileBackedHistory, KeyCode, KeyModifiers, MenuBuilder, Reedline,
    ReedlineEvent, ReedlineMenu, Signal, default_emacs_keybindings,
};

use crate::ExitCode;

use crate::repl_core::commands::{Command, parse_command};
use crate::repl_core::highlight::Palette;
use crate::repl_core::session::Session;
use completion::{NameSet, ReplCompleter, ReplHighlighter, ReplHinter, ReplValidator};
use prompt::{CurrentModule, KioPrompt};
use watch::Watcher;

/// The capacity (max entries) of the file-backed REPL history, the
/// reedline analogue of rustyline's default history length.
const HISTORY_CAPACITY: usize = 1000;

/// The name the completion menu is registered under. The `Tab`
/// keybinding triggers the menu by this name (reedline matches the
/// `ReedlineEvent::Menu` name against the registered menu), so the two
/// must agree.
const COMPLETION_MENU_NAME: &str = "completion_menu";

/// The parser needs the full input and produces absolute replacement spans.
fn completion_menu() -> DescriptionMenu {
    DescriptionMenu::default()
        .with_name(COMPLETION_MENU_NAME)
        .with_only_buffer_difference(false)
}

/// The emacs keybindings the REPL runs under: reedline's defaults plus
/// a `Tab` binding that opens the completion menu (and cycles to the
/// next candidate once it is open).
///
/// reedline's default emacs map supplies `Enter` to accept the highlighted
/// candidate and `Esc` to dismiss the menu. `Tab` is unbound by default.
fn menu_keybindings() -> reedline::Keybindings {
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(COMPLETION_MENU_NAME.to_owned()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keybindings
}

const HELP_TEMPLATE: &str = "\
Usage: kio repl [<selector>...]

Open the Kio module inspector — an interactive prompt for loading
modules from the current directory by their module path (FQN) and
querying types, docs, and cross-references.

`kio repl` runs on the module tree rooted at the current directory; a
`*.pkg.kio` package file is optional. It opens on a blank slate — no
modules are loaded at startup. Use `:load <module>` to bring a module
in by its module path (a module name like `op/main`); pass one or more
`<selector>` arguments to load specific modules at startup instead.
Type `:help` at the prompt for the command list. The inspector
type-checks each loaded module but does not execute it against a host.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on clean exit (`:quit`
or end-of-input); 1 (internal error) when the interactive terminal
could not be opened or the current directory could not be read; 2 on
CLI usage error (unknown flag, or a startup selector that matches no
module).

See {base}/specs/cli.md#kio-repl-selector for the documented surface.";

/// The current module of `session` as the [`CurrentModule`] cell
/// carries it: the slash path when a module is current, `None`
/// otherwise.
///
/// reedline owns the [`KioPrompt`], so the prompt cannot read the
/// session directly each turn the way the rustyline-era
/// `render_prompt(&Session)` did. Instead the loop refreshes the
/// shared cell (via [`refresh_session_state`]) after every
/// state-changing command, and [`KioPrompt`] reads the cell. This
/// helper is the one place the session's notion of "current" becomes
/// the cell's value, so the bracketed `kio[<module>]> ` shape stays
/// identical.
fn current_module_of(session: &Session) -> Option<String> {
    session.current().map(str::to_owned)
}

/// Entry point for `kio repl`.
///
/// Parses flags and runs the interactive loop on the current
/// directory's module tree (a `*.pkg.kio` package file is optional;
/// modules load by their FQN with no package gate). Returns
/// [`ExitCode::Success`] on a clean exit (`:quit` or EOF),
/// [`ExitCode::Usage`] for a bad invocation (unknown flag, or a startup
/// selector that matches no module), and [`ExitCode::Internal`] when
/// the terminal or current directory cannot be read.
pub fn run(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    // Reject unknown options (anything starting with `-`); every other
    // positional is treated as a module selector.
    let mut selectors: Vec<String> = Vec::new();
    for a in args {
        if let Some(flag) = a.strip_prefix('-') {
            // `--` and `-h`/`--help` are the only flag shapes accepted;
            // `-h`/`--help` is handled above, so any flag here is an
            // unknown option.
            eprintln!("error: `kio repl` does not accept the option `-{flag}`");
            return ExitCode::Usage;
        }
        selectors.push(a.clone());
    }

    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };

    run_loop(cwd, &selectors)
}

/// The REPL loop.
///
/// Shared startup (palette, session, name set, current-module cell,
/// watcher, initial load, banner) runs once; then the loop branches on
/// whether stdin is a terminal. An **interactive** stdin gets the full
/// reedline line editor; a **piped** stdin (a transcript over a pipe —
/// smoke tests, CI harnesses, agents driving the prompt) reads with
/// `BufRead`, because reedline unconditionally puts the terminal in
/// raw mode and errors on a pipe. The two paths share
/// [`dispatch_line`], so they cannot drift.
fn run_loop(package_root: PathBuf, selectors: &[String]) -> ExitCode {
    let palette = Palette::detect();
    let mut session = Session::new(package_root.clone());

    // The shared completion name set — the completer reads it, the loop
    // refreshes it after each state-changing command.
    let name_set = Arc::new(Mutex::new(NameSet::default()));
    // The shared current-module cell — the reedline `KioPrompt` reads
    // it, the loop refreshes it (alongside the name set) after each
    // state-changing command.
    let current_module: CurrentModule = Arc::new(Mutex::new(None));

    // The auto-reload watcher. Best effort — `None` just means the
    // session runs without auto-reload.
    let watcher = Watcher::start(&package_root);

    // Bring up the package analysis so we can resolve selectors (and
    // know what to print on the banner) before the prompt opens. A
    // selector-mismatch fails fast at the usage code (the user
    // explicitly asked for a module that isn't there); a type-check
    // failure leaves the REPL interactive so the user can fix the
    // problem and `:load` manually.
    let banner_loaded = match initial_load(&mut session, selectors) {
        Ok(loaded) => Some(loaded),
        Err(InitialLoadError::SelectorMismatch(msg)) => {
            eprintln!("error: {msg}");
            return ExitCode::Usage;
        }
        Err(InitialLoadError::PackageBroken(msg)) => {
            eprintln!("{msg}");
            None
        }
    };
    // The available-module set the banner advertises — every module the
    // directory's analysis discovered, whether loaded or not. Empty when
    // the analysis failed or the directory has no modules.
    let available = session.package_module_paths();
    print_banner(banner_loaded.as_deref(), &available);
    if banner_loaded.is_some() {
        refresh_session_state(&session, &name_set, &current_module);
    }

    if std::io::stdin().is_terminal() {
        run_loop_interactive(session, palette, name_set, current_module, watcher.as_ref())
    } else {
        run_loop_piped(session, palette, name_set, current_module, watcher.as_ref())
    }
}

/// The interactive (terminal-stdin) loop: a reedline line editor with
/// the completer / highlighter / hinter / validator installed, history
/// persisted to the XDG data dir, and the `Signal` match driving
/// Ctrl-C / Ctrl-D.
fn run_loop_interactive(
    mut session: Session,
    palette: Palette,
    name_set: Arc<Mutex<NameSet>>,
    current_module: CurrentModule,
    watcher: Option<&Watcher>,
) -> ExitCode {
    // File-backed history persists across sessions. A construction
    // failure (no writable history dir) is non-fatal — fall back to an
    // in-memory editor so the REPL still opens. The history file format
    // is reedline's, not rustyline's: an old rustyline history file at
    // the same path is overwritten, not read.
    let mut line_editor = match history_file_path()
        .and_then(|path| FileBackedHistory::with_file(HISTORY_CAPACITY, path).ok())
    {
        Some(history) => Reedline::create().with_history(Box::new(history)),
        None => Reedline::create(),
    };
    // The pop-up completion menu. `DescriptionMenu` renders the
    // candidate values in columns and the selected candidate's
    // `Suggestion::description` (the kind / source / summary metadata
    // the completer fills) in a de-emphasised description block below —
    // exactly the per-candidate "what it is, where it lives" column this
    // REPL wants. reedline's `EngineCompleter` wiring drives the menu
    // from the same `ReplCompleter`, so there is no second candidate
    // source to keep in sync.
    let completion_menu = Box::new(completion_menu());

    line_editor = line_editor
        .with_completer(Box::new(ReplCompleter::new(Arc::clone(&name_set))))
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_edit_mode(Box::new(Emacs::new(menu_keybindings())))
        .with_highlighter(Box::new(ReplHighlighter::new(
            palette,
            Arc::clone(&name_set),
        )))
        .with_hinter(Box::new(ReplHinter::new(Arc::clone(&name_set))))
        .with_validator(Box::new(ReplValidator::new(Arc::clone(&name_set))));

    let kio_prompt = KioPrompt::new(Arc::clone(&current_module), palette);

    loop {
        match line_editor.read_line(&kio_prompt) {
            Ok(Signal::Success(line)) => {
                let keep_running = dispatch_line(
                    &mut session,
                    palette,
                    &name_set,
                    &current_module,
                    watcher,
                    &line,
                );
                if !keep_running {
                    break;
                }
            }
            // Ctrl-C — abandon the current line, keep the session.
            Ok(Signal::CtrlC) => continue,
            // Ctrl-D — leave the REPL.
            Ok(Signal::CtrlD) => break,
            // Other signals (host command, external break) aren't
            // wired this session; treat them as a no-op continuation.
            Ok(_) => continue,
            Err(e) => {
                eprintln!("error: line reader failed: {e}");
                break;
            }
        }
    }

    // reedline's `FileBackedHistory` persists entries as they are
    // submitted, so there is no explicit save on the way out.
    println!("bye");
    ExitCode::Success
}

/// The piped (non-terminal-stdin) loop: read lines with `BufRead`,
/// **gather continuation lines** the same way the interactive
/// validator does, and dispatch each completed input through the shared
/// [`dispatch_line`] helper. No reedline construction, no `Prompt`, no
/// highlighting, no history — a piped transcript has no terminal to
/// raw-mode and no useful history to persist. EOF (stdin close) breaks
/// the loop, the same as `Signal::CtrlD`; Ctrl-C is the parent
/// process's job.
///
/// ## Multi-line gathering
///
/// The interactive path lets reedline's [`ReplValidator`] decide when a
/// buffer is complete (an unclosed bracket / open string literal keeps
/// the buffer open across `Enter` presses). A pipe has no validator —
/// `BufRead` hands over one physical line at a time — so this loop
/// reconstructs the same behaviour explicitly: it accumulates physical
/// lines (joined with `\n`) until [`completion::input_is_complete`]
/// reports the gathered buffer is complete, then dispatches the whole
/// buffer in one go. The two paths therefore agree on what a single
/// "input" is — a transcript driving `:normalize if true {` across two lines
/// normalizes one expression, exactly as a user typing it interactively
/// would. EOF flushes any partial buffer so an unterminated final input
/// still reaches the parser (which surfaces the error) rather than being
/// silently dropped.
///
/// The prompt is **not** printed to stdout: the rustyline-era REPL
/// didn't print one over a pipe either (the prompt goes to the
/// terminal control stream, which is absent), and the smoke tests
/// assert on prompt-free output.
fn run_loop_piped(
    mut session: Session,
    palette: Palette,
    name_set: Arc<Mutex<NameSet>>,
    current_module: CurrentModule,
    watcher: Option<&Watcher>,
) -> ExitCode {
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    // The gathered input: physical lines joined with `\n`, accumulated
    // until `input_is_complete` reports the buffer is complete.
    let mut gathered = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        match handle.read_line(&mut line) {
            // EOF — stdin closed. Flush any partial gathered buffer so an
            // unterminated final input still reaches the parser, then
            // leave the REPL.
            Ok(0) => {
                if !gathered.trim().is_empty() {
                    dispatch_line(
                        &mut session,
                        palette,
                        &name_set,
                        &current_module,
                        watcher,
                        &gathered,
                    );
                }
                break;
            }
            Ok(_) => {
                // Append the physical line (its trailing `\n` included,
                // so the lexer sees the same byte sequence the
                // interactive buffer would carry). `read_line` keeps the
                // newline; the last line before EOF may lack one, which
                // is fine — it's still a line boundary.
                gathered.push_str(&line);

                // A still-incomplete buffer keeps gathering: the
                // continuation lines belong to the same input. The empty
                // buffer (only blank lines so far) is "complete" and
                // dispatches as the no-op `dispatch_line` already treats
                // it, so blank lines never wedge the loop open.
                let complete = {
                    let context = name_set
                        .lock()
                        .expect("name-set mutex not poisoned")
                        .expression_parse_context
                        .clone();
                    completion::input_is_complete_with_context(&gathered, context.as_deref())
                };
                if !complete {
                    continue;
                }

                let input = std::mem::take(&mut gathered);
                let keep_running = dispatch_line(
                    &mut session,
                    palette,
                    &name_set,
                    &current_module,
                    watcher,
                    &input,
                );
                if !keep_running {
                    break;
                }
            }
            Err(e) => {
                eprintln!("error: reading stdin failed: {e}");
                break;
            }
        }
    }

    println!("bye");
    ExitCode::Success
}

/// Process one input line — the shared body of both loops.
///
/// Trims the line, drains the auto-reload watcher, parses and runs the
/// command, prints its output, and refreshes the session state after a
/// state-changing command. Returns whether the loop should keep
/// running (`false` after `:quit`). An empty line is a no-op that keeps
/// the loop running.
///
/// Factoring this out is what keeps the interactive and piped loops
/// from drifting: both read a line by their own means, then hand it
/// here for identical handling.
fn dispatch_line(
    session: &mut Session,
    palette: Palette,
    name_set: &Arc<Mutex<NameSet>>,
    current_module: &CurrentModule,
    watcher: Option<&Watcher>,
    line: &str,
) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }

    // Drain the watcher *after* the line is read but *before* the
    // command runs: a `.kio` file that changed while the user was at
    // the prompt is re-typechecked first, so the command sees current
    // contents. The `reloaded …` line prints ahead of the command's
    // own output.
    //
    // A bare watcher event isn't enough on its own —
    // `notify_debouncer_mini` fires on every metadata bump, including
    // touches and atomic re-writes of identical bytes. Cross-check
    // against the fingerprint cache so a phantom event doesn't drive a
    // redundant re-typecheck or a misleading `reloaded` line.
    if let Some(w) = watcher
        && w.drain()
    {
        let changed = session.detect_source_changes();
        if !changed.is_empty() {
            handle_auto_reload(session, palette, &changed);
            refresh_session_state(session, name_set, current_module);
        }
    }

    match parse_command(trimmed) {
        Ok(cmd) => {
            let state_changing = is_state_changing(&cmd);
            let outcome = cmd.run(session, palette);
            if !outcome.output.is_empty() {
                println!("{}", outcome.output);
            }
            if !outcome.keep_running {
                return false;
            }
            if state_changing {
                refresh_session_state(session, name_set, current_module);
            }
        }
        Err(e) => {
            eprintln!("error: {}", e.0);
        }
    }
    true
}

/// Whether a command changes session state in a way that affects the
/// completion name set or the current module (`:load` / `:unload` /
/// `:reset`).
fn is_state_changing(cmd: &Command) -> bool {
    matches!(cmd, Command::Load(_) | Command::Unload(_) | Command::Reset)
}

/// Refresh the shared session state the line editor reads: the
/// completion [`NameSet`] and the [`CurrentModule`] cell the reedline
/// [`KioPrompt`] renders. Called after each state-changing command so
/// the next completion request and the next prompt render reflect the
/// session. The name set itself is built by the terminal-free
/// [`NameSet::from_session`] the browser wrapper also uses.
fn refresh_session_state(
    session: &Session,
    name_set: &Arc<Mutex<NameSet>>,
    current_module: &CurrentModule,
) {
    // Update the current-module cell the prompt reads.
    *current_module
        .lock()
        .expect("current-module mutex not poisoned") = current_module_of(session);

    *name_set.lock().expect("name-set mutex not poisoned") = NameSet::from_session(session);
}

/// Re-typecheck after a watched file changed, printing the
/// `reloaded …` line on success or a diagnostic on failure.
///
/// `changed` is the set of slash module paths the change detector
/// identified as having had their source change since the last
/// refresh — the `reloaded …` line names *those* modules, not the
/// explicitly-loaded set, so the user sees what actually moved.
/// (When a changed module isn't currently loaded, the line still
/// prints; the package was re-typechecked and that's worth saying.)
///
/// On failure the session keeps its prior analysis — the
/// auto-reload contract is that a broken edit never silently
/// discards a working session.
fn handle_auto_reload(
    session: &mut Session,
    _palette: Palette,
    changed: &std::collections::BTreeSet<String>,
) {
    match session.refresh() {
        Ok(()) => {
            let joined: Vec<String> = changed.iter().cloned().collect();
            println!("reloaded {}", joined.join(", "));
        }
        Err(failure) => {
            let located = failure.primary_error();
            let (span, message) = located.error.diag();
            let loc = failure
                .sources
                .get(&located.file_path)
                .map(|src| {
                    let (line, col) = line_col(src, span.start);
                    format!("{}:{line}:{col}", located.file_path.display())
                })
                .unwrap_or_else(|| located.file_path.display().to_string());
            eprintln!("reload failed — keeping the previous version:");
            eprintln!("  {loc}: {message}");
        }
    }
}

/// The on-disk history file path: `$XDG_DATA_HOME/kio/history`,
/// falling back to `$HOME/.local/share/kio/history`. Returns `None`
/// when neither environment variable is set (history then lives
/// only in memory for the session).
fn history_file_path() -> Option<PathBuf> {
    history_path_from(
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// Pure history-path resolution, factored out of [`history_file_path`]
/// so it can be tested without mutating process environment.
///
/// `xdg_data_home` is `$XDG_DATA_HOME` (used only when absolute, per
/// the XDG Base Directory spec — a relative value is ignored);
/// `home` is `$HOME`. Returns `None` when neither yields a usable
/// base directory.
fn history_path_from(xdg_data_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(xdg) = xdg_data_home
        && xdg.is_absolute()
    {
        return Some(xdg.join("kio").join("history"));
    }
    let home = home?;
    Some(
        home.join(".local")
            .join("share")
            .join("kio")
            .join("history"),
    )
}

/// Print the one-time opening banner.
///
/// The REPL opens on a blank slate: nothing is loaded at startup
/// unless the user passed a startup `<selector>`. `loaded` lists the
/// slash paths of every module a startup selector brought into the
/// session, in load order — empty on a no-selector launch, `None` when
/// the initial analysis failed (the error was already printed).
/// `available` is every module the directory's analysis discovered
/// (loaded or not); the banner reports its count and how to bring one
/// in, so the user knows what is there to `:load`.
fn print_banner(loaded: Option<&[String]>, available: &[String]) {
    println!("kio repl — the module inspector");
    let color = crate::diagnostic::ColorMode::for_stdout();
    match loaded {
        // A startup selector loaded a subset — name them.
        Some(modules) if !modules.is_empty() => {
            println!("loaded: {}", modules.join(", "));
        }
        // Blank-slate launch (or a failed analysis): nothing loaded.
        // Advertise what the directory has and how to bring it in.
        _ => {
            if available.is_empty() {
                println!("(no modules in this directory)");
            } else {
                let count = available.len();
                let noun = if count == 1 { "module" } else { "modules" };
                println!(
                    "{count} {noun} available — {} to bring one in ({} lists what's loaded)",
                    crate::diagnostic::style_inline_code("`:load <module>`", color),
                    crate::diagnostic::style_inline_code("`:mods`", color)
                );
            }
        }
    }
    println!(
        "type {} for commands, {} to leave",
        crate::diagnostic::style_inline_code("`:help`", color),
        crate::diagnostic::style_inline_code("`:quit`", color)
    );
}

/// Resolve a list of startup selectors against the directory's
/// modules and load them into `session`.
///
/// **Blank-slate startup.** With no selectors, nothing is loaded —
/// the REPL opens empty and the user brings modules in with `:load`.
/// With one or more selectors, each must match a module: matching is
/// by the module's slash path (`op/main`), by the filename of the
/// module's `.kio` file (with or without the `.kio` extension), or by
/// the file's stem (the filename minus `.kio`).
///
/// Returns the slash paths of every module that ended up loaded, in
/// load order (empty when no selectors were given). Errors
/// string-format the failure: the directory's modules don't
/// type-check, or one of the selectors didn't match any module. On
/// failure the session may already have loaded some modules (the
/// partial state is left in place so the user can inspect it).
fn initial_load(
    session: &mut Session,
    selectors: &[String],
) -> Result<Vec<String>, InitialLoadError> {
    // Refresh first — this analyses the directory's modules and
    // populates the available-module list (the banner reads it, and
    // selector resolution needs it). A type error leaves the REPL
    // interactive (the user can fix the source and `:load` after), but
    // skip the auto-load: it would just print the same error N times.
    if let Err(failure) = session.refresh() {
        return Err(InitialLoadError::PackageBroken(format!(
            "the directory's modules do not type-check — open the REPL with no loads:\n  {}",
            render_failure_for_banner(&failure)
        )));
    }

    // Blank slate: with no selectors, load nothing. The user brings
    // modules in with `:load`.
    if selectors.is_empty() {
        return Ok(Vec::new());
    }

    let available = session.package_module_paths();
    let targets: Vec<String> = {
        let mut chosen: Vec<String> = Vec::new();
        for sel in selectors {
            match resolve_selector(session, sel, &available) {
                Some(module_path) => {
                    if !chosen.contains(&module_path) {
                        chosen.push(module_path);
                    }
                }
                None => {
                    return Err(InitialLoadError::SelectorMismatch(format!(
                        "selector `{sel}` matches no regular module — \
                         available: {}",
                        if available.is_empty() {
                            "(none)".to_owned()
                        } else {
                            available
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                    )));
                }
            }
        }
        chosen
    };

    // Load each target. Each `:load` re-runs `refresh()`; we already
    // did that once, so the subsequent ones are mostly cache hits.
    let palette = Palette::plain();
    let mut loaded: Vec<String> = Vec::new();
    for module_path in &targets {
        // `cmd_load` returns a status string we don't display at
        // startup — the banner enumerates the result, so a per-module
        // "loaded X" line would be noise. We still need its planning
        // to run, so call it for the side effect.
        let _ = crate::repl_core::commands::cmd_load_for_startup(session, module_path, palette);
        if session.module(module_path).is_some() {
            loaded.push(module_path.clone());
        }
    }
    Ok(loaded)
}

/// Why the startup auto-load could not complete.
enum InitialLoadError {
    /// A `<selector>` argument matched no regular module. The user
    /// asked for something that isn't there; treat it like any other
    /// usage error and exit non-zero.
    SelectorMismatch(String),
    /// The package itself does not type-check. The REPL still opens
    /// (the user may want to inspect after a fix) but with nothing
    /// loaded.
    PackageBroken(String),
}

/// Match one startup selector against the regular-module set.
/// Returns the matching module's slash path or `None` if no regular
/// module matches.
fn resolve_selector(session: &Session, selector: &str, available: &[String]) -> Option<String> {
    // (a) The selector is the module path of a regular module.
    if available.iter().any(|module_path| module_path == selector) {
        return Some(selector.to_owned());
    }
    // (b) The selector matches a regular module's filename — with or
    // without the `.kio` extension.
    let stem = selector.strip_suffix(".kio").unwrap_or(selector);
    for module_path in available {
        let Some(file_path) = session.module_file(module_path) else {
            continue;
        };
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let file_stem = file_path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if file_name == selector || file_stem == stem {
            return Some(module_path.clone());
        }
    }
    None
}

/// Render the analysis failure as a single line for the startup
/// banner. The same one-line shape `cmd_load` produces.
fn render_failure_for_banner(failure: &crate::cmd::check::AnalysisFailure) -> String {
    let located = failure.primary_error();
    let (span, message) = located.error.diag();
    if let Some(src) = failure.sources.get(&located.file_path) {
        let (line, col) = line_col(src, span.start);
        format!("{}:{line}:{col}: {message}", located.file_path.display())
    } else {
        format!("{}: {message}", located.file_path.display())
    }
}

/// 1-based `(line, column)` of a byte offset within `source`.
fn line_col(source: &str, offset: u32) -> (usize, usize) {
    let off = (offset as usize).min(source.len());
    let mut line = 1;
    let mut col = 1;
    for ch in source[..off].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl_core::session::StagedModule;

    #[test]
    fn completion_menu_passes_the_full_line_and_absolute_cursor() {
        #[derive(Default)]
        struct RecordingCompleter(Vec<(String, usize)>);
        impl reedline::Completer for RecordingCompleter {
            fn complete(&mut self, line: &str, cursor: usize) -> Vec<reedline::Suggestion> {
                self.0.push((line.to_owned(), cursor));
                Vec::new()
            }
        }
        let mut editor = reedline::Editor::default();
        let mut menu = completion_menu();
        let mut completer = RecordingCompleter::default();
        for line in [":source item_", ":source item_39"] {
            editor.edit_buffer(
                |buffer| buffer.set_buffer(line.to_owned()),
                reedline::UndoBehavior::CreateUndoPoint,
            );
            reedline::Menu::update_values(&mut menu, &mut editor, &mut completer);
        }
        assert_eq!(
            completer.0,
            vec![
                (":source item_".to_owned(), 13),
                (":source item_39".to_owned(), 15)
            ]
        );
    }

    #[test]
    fn is_state_changing_classifies_commands() {
        assert!(is_state_changing(&Command::Load("x".to_owned())));
        assert!(is_state_changing(&Command::Unload("x".to_owned())));
        assert!(is_state_changing(&Command::Reset));
        // Query commands do not change the name set.
        assert!(!is_state_changing(&Command::Mods));
        assert!(!is_state_changing(&Command::Type("x".to_owned())));
        assert!(!is_state_changing(&Command::Help));
    }

    // OS-absolute path from slash-separated segments (a leading `/` is
    // not absolute on Windows, so the XDG branch would be skipped).
    fn abs(p: &str) -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(format!("C:\\{}", p.replace('/', "\\")))
        }
        #[cfg(not(windows))]
        {
            PathBuf::from(format!("/{p}"))
        }
    }

    #[test]
    fn history_path_prefers_absolute_xdg_data_home() {
        let xdg = abs("tmp/xdg-test-data");
        let p = history_path_from(Some(xdg.clone()), Some(PathBuf::from("/home/user")))
            .expect("path with XDG set");
        assert!(p.ends_with("kio/history"));
        assert!(p.starts_with(&xdg));
    }

    #[test]
    fn history_path_falls_back_to_home() {
        // A relative XDG value is ignored; HOME is used instead.
        let p = history_path_from(
            Some(PathBuf::from("relative/path")),
            Some(PathBuf::from("/home/user")),
        )
        .expect("path with HOME set");
        assert_eq!(p, PathBuf::from("/home/user/.local/share/kio/history"));
    }

    #[test]
    fn history_path_none_without_any_env() {
        assert!(history_path_from(None, None).is_none());
    }

    #[test]
    fn line_col_counts_lines_and_columns() {
        let src = "ab\ncd\nef";
        assert_eq!(line_col(src, 0), (1, 1));
        assert_eq!(line_col(src, 3), (2, 1)); // start of line 2
        assert_eq!(line_col(src, 7), (3, 2)); // 'f'
    }

    /// The two-line `<context>\nkio> ` shape the reedline `KioPrompt`
    /// renders for a given current-module cell value, under the plain
    /// palette (no escapes) so the assertions match literal text — the
    /// post-reedline analogue of the rustyline-era
    /// `render_prompt(&Session)` shape assertions. The full styled /
    /// truncation behaviour is pinned in `prompt.rs`'s own tests.
    fn rendered_prompt(current: Option<&str>) -> String {
        use reedline::Prompt;
        let cell: CurrentModule = Arc::new(Mutex::new(current.map(str::to_owned)));
        KioPrompt::new(cell, Palette::plain())
            .render_prompt_left()
            .into_owned()
    }

    #[test]
    fn current_module_of_is_none_for_fresh_session() {
        // A fresh session has no current module → the cell is `None`,
        // and the prompt's context line is the no-module hint above the
        // bare `kio> ` symbol.
        let session = Session::new(PathBuf::from("/tmp/pkg"));
        assert_eq!(current_module_of(&session), None);
        let prompt = rendered_prompt(None);
        assert!(prompt.contains("(no module —"), "got: {prompt}");
        assert!(prompt.ends_with("\nkio> "), "got: {prompt}");
    }

    #[test]
    fn current_module_of_names_the_loaded_module() {
        // Stage a module and set it current via `commit_load`; the cell
        // then carries the slash module path, and
        // the prompt names it on the context line above the `kio> `
        // symbol.
        let mut session = Session::new(PathBuf::from("/tmp/pkg"));
        let module = crate::pass::parser::parse("module pkg/demo;\npub fn run() -> . { () }\n")
            .expect("test module parses");
        let staged = StagedModule {
            path: "pkg/demo".to_owned(),
            file_path: PathBuf::from("/tmp/pkg/demo.kio"),
            module,
        };
        session.commit_load("pkg/demo", &[staged]);
        assert_eq!(current_module_of(&session), Some("pkg/demo".to_owned()));
        let prompt = rendered_prompt(Some("pkg/demo"));
        assert_eq!(prompt, "pkg/demo\nkio> ");
    }
}
