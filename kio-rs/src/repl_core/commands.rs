//! Meta-command parsing and dispatch for the `kio repl` inspector.
//!
//! A REPL turn is one of two things: a *meta-command* — a line
//! beginning with `:` — or a bare-input query. [`parse_command`]
//! turns a line into a [`Command`]; [`Command::run`] executes it
//! against a [`Session`](super::session::Session) and returns the text
//! to print.
//!
//! ## Bare input
//!
//! A non-`:` line is a bare-input query, [`Command::Expr`]. It is
//! classified into the kinds it matches ([`bare_input_kinds`]) — an
//! input can match more than one (a bare identifier is both a name and
//! an expression):
//!
//! - A bare *name* — a single identifier, a dotted path, a
//!   fully-qualified `mod/path.item`, an operator symbol — routes to
//!   `:doc` (its doc-comment + signature, or a module summary).
//! - A bare *compound expression* — a literal, an application, an
//!   operator expression — routes to `:normalize` (its residual normal
//!   form).
//!
//! The default action runs, then a dim footer lists every command
//! applicable to the matched kind(s). Input that matches **no** kind
//! prints a single honest invalid-input line. The explicit
//! `:normalize <expr>` forces reduction (useful on a name, which a
//! bare line would route to `:doc`). `:t <name-or-expression>` and
//! `:pure <name-or-expression>` accept either input category and print
//! its type or purity verdict, respectively.
//!
//! ## Synonyms
//!
//! Eleven commands have a short and a long spelling, both first-class
//! (and `:help` carries two short forms — `:h` and `:?`). The
//! [`canonical_name`] table maps every spelling to the canonical one.
//! Tab completion offers them all.
//!
//! ## Name resolution
//!
//! A name argument to `:t` / `:pure` / `:signature` / `:doc` / `:source`
//! / `:which` / `:refs` is resolved through the **current scope**: the
//! items the current module declares and the names it selectively imports.
//! An alias-qualified value path belongs to the expression category instead.
//! `:scope` enumerates both forms. A fully-qualified `X/a/b.name` resolves
//! directly. With no current module (right after `kio repl` opens, or after
//! `:reset`) only FQNs resolve.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::ast::{FnDef, ImportKind, Item, Module, Surface};

use super::expr_query::{
    ExprQueryError, ExprQueryResult, InputKind, ReplExprQueryError, classify_input,
    query_expr_for_repl, query_expr_purity_for_repl,
};
use super::highlight::{Palette, highlight, highlight_type};
use super::session::{Session, StagedModule, import_clause_targets};

/// One parsed REPL input. `Help` / `Quit` / `Reset` are the universal
/// session controls; the rest are the inspector surface. `Expr` is a
/// bare (non-`:`) input query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `:load <module-path>`
    Load(String),
    /// `:t <name-or-expression>` — a name's type, or an arbitrary
    /// expression's synthesized type.
    Type(String),
    /// `:pure <name-or-expression>` — whether a function declaration or
    /// expression is admitted by the compiler's pure-function rule.
    Pure(String),
    /// `:signature <name>` — a named item's declaration header.
    Signature(String),
    /// `:normalize <expression>` — an expression's residual normal form.
    Normalize(String),
    /// `:doc <name>`
    Doc(String),
    /// `:source <name>`
    Source(String),
    /// `:ls [-v] [<module-path>]` — with an argument, the named module;
    /// with none, the current module. `verbose` (`-v`) requests full
    /// signatures for declaration kinds that have them.
    Ls {
        module: Option<String>,
        verbose: bool,
    },
    /// `:mods`
    Mods,
    /// `:packages [<package-name>]` — with no argument, list the
    /// `*.pkg.kio` package files in the directory tree; with a package
    /// name (a `.pkg.kio` stem), view that package's contract.
    Packages(Option<String>),
    /// `:unload <module-path>`
    Unload(String),
    /// `:which <name>`
    Which(String),
    /// `:refs <name>`
    Refs(String),
    /// `:scope [-v]` — list everything in the current scope. `verbose`
    /// (`-v`) requests full signatures for declaration kinds that have them.
    Scope { verbose: bool },
    /// A bare (non-`:`) input query — a name (kind-aware
    /// shortcut) or a compound expression (queried in place).
    Expr(String),
    /// `:help`
    Help,
    /// `:quit`
    Quit,
    /// `:reset`
    Reset,
}

/// The outcome of running a [`Command`]: text to print plus whether
/// the session should keep running.
#[derive(Debug)]
pub struct Outcome {
    /// Text to print to the user. May be empty.
    pub output: String,
    /// `false` only for `:quit` — the REPL loop exits.
    pub keep_running: bool,
}

impl Outcome {
    fn say(output: String) -> Self {
        Self {
            output,
            keep_running: true,
        }
    }

    fn quit() -> Self {
        Self {
            output: String::new(),
            keep_running: false,
        }
    }
}

/// An error parsing a REPL line into a [`Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

/// Every canonical command name, in the order `:help` lists them.
/// The session controls come last.
pub const CANONICAL_COMMANDS: &[&str] = &[
    "load",
    "signature",
    "source",
    "t",
    "pure",
    "normalize",
    "doc",
    "ls",
    "mods",
    "packages",
    "scope",
    "unload",
    "which",
    "refs",
    "help",
    "quit",
    "reset",
];

/// One `:help` row, and the single source of truth for a command's
/// one-line summary. Each row pairs a canonical command with its
/// argument hint and the summary the popup-completion menu renders in
/// its description column; the synonym column is derived from the
/// [`SYNONYMS`] table (a command may carry more than one — `:help` has
/// `:h` and `:?`). `help_text` renders the grouped help block from these
/// rows, and [`command_summary`] looks a summary up by canonical name,
/// so the two can never drift.
struct HelpRow {
    /// The canonical command name (one of [`CANONICAL_COMMANDS`]).
    canonical: &'static str,
    /// The argument hint shown after the spellings (`<module-path>`),
    /// empty for a no-argument command.
    arg: &'static str,
    /// The one-line description rendered in `:help` and in the popup
    /// menu's description column.
    summary: &'static str,
}

/// The `:help` rows, split into the two sections `:help` prints —
/// inspection commands first, session controls last. Drives both the
/// rendered help block and the per-command popup-menu description.
const HELP_SECTIONS: &[(&str, &[HelpRow])] = &[
    (
        "Inspection",
        &[
            HelpRow {
                canonical: "load",
                arg: "<module-path>",
                summary: "load a module (and its import-closure)",
            },
            HelpRow {
                canonical: "signature",
                arg: "<name>",
                summary: "print a name's declaration header",
            },
            HelpRow {
                canonical: "source",
                arg: "<name>",
                summary: "print a name's canonical source",
            },
            HelpRow {
                canonical: "t",
                arg: "<expr>",
                summary: "print an expression's (or name's) type",
            },
            HelpRow {
                canonical: "pure",
                arg: "<name-or-expression>",
                summary: "report whether an expression or function is pure",
            },
            HelpRow {
                canonical: "normalize",
                arg: "<expr>",
                summary: "print an expression's residual normal form",
            },
            HelpRow {
                canonical: "doc",
                arg: "<name>",
                summary: "render a name's doc-comment + signature",
            },
            HelpRow {
                canonical: "ls",
                arg: "[-v] [<module>]",
                summary: "list a module's items (`-v` for full signatures)",
            },
            HelpRow {
                canonical: "mods",
                arg: "",
                summary: "list loaded modules (* marks current)",
            },
            HelpRow {
                canonical: "packages",
                arg: "[<package>]",
                summary: "list `*.pkg.kio` packages, or view one's contract",
            },
            HelpRow {
                canonical: "scope",
                arg: "[-v]",
                summary: "list everything in the current scope (`-v` for signatures)",
            },
            HelpRow {
                canonical: "unload",
                arg: "<module-path>",
                summary: "remove a loaded module",
            },
            HelpRow {
                canonical: "which",
                arg: "<name>",
                summary: "which loaded module declares a name",
            },
            HelpRow {
                canonical: "refs",
                arg: "<name>",
                summary: "every reference to a name",
            },
        ],
    ),
    (
        "Session",
        &[
            HelpRow {
                canonical: "help",
                arg: "",
                summary: "this help",
            },
            HelpRow {
                canonical: "reset",
                arg: "",
                summary: "drop every loaded module",
            },
            HelpRow {
                canonical: "quit",
                arg: "",
                summary: "leave the REPL",
            },
        ],
    ),
];

/// The one-line summary for a **canonical** command name, or `None`
/// when the name isn't a known command. The completer folds synonyms
/// through [`canonical_name`] before looking the summary up; the popup
/// menu renders the result in its description column.
pub fn command_summary(canonical: &str) -> Option<&'static str> {
    HELP_SECTIONS
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .find(|row| row.canonical == canonical)
        .map(|row| row.summary)
}

/// Every command spelling — canonical names plus short synonyms —
/// each with a leading `:`. Tab completion draws `:command`
/// candidates from this list.
pub fn all_command_spellings() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for c in CANONICAL_COMMANDS {
        v.push(format!(":{c}"));
    }
    for (short, _) in SYNONYMS {
        v.push(format!(":{short}"));
    }
    v.sort();
    v
}

/// Short → long synonym pairs. Each pair is one command under two
/// names. `canonical_name` consults this to fold a short spelling
/// onto its long form.
const SYNONYMS: &[(&str, &str)] = &[
    ("l", "load"),
    ("u", "unload"),
    ("q", "quit"),
    ("h", "help"),
    ("?", "help"),
    ("type", "t"),
    ("norm", "normalize"),
    ("sig", "signature"),
    ("src", "source"),
    ("list", "ls"),
    ("modules", "mods"),
    ("pkgs", "packages"),
    ("references", "refs"),
];

/// What a command's argument position completes to. The tab
/// completer consults this (via [`completion_shape`]) to filter the
/// candidate pool by what each command actually takes, so `:load`
/// offers module paths only and `:mods` offers nothing.
///
/// This is independent of the line reader — `repl/03`'s reedline
/// switch consults the same table unchanged; only the rendering of
/// the chosen candidates moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionShape {
    /// No argument completes — a no-arg command (`:mods` / `:help` /
    /// `:reset` / `:quit` / `:scope`).
    None,
    /// A module path — `:load` / `:unload` / `:ls`.
    ModulePath,
    /// A bare name or a fully-qualified name — `:t` / `:pure` /
    /// `:which` / `:refs`. Operators are **not** offered: `:t` and
    /// `:pure` inspect executable value bindings (an operator binding
    /// is neither — `specs/cli.md` § `kio repl`), and `:refs` keys on
    /// bound names, not operator tokens.
    NameOrFqn,
    /// A bare name, fully-qualified name, or builtin name — `:which`.
    /// Operators are not offered because `:which` keys on the bound
    /// function name, not the operator token.
    NameOrFqnOrBuiltin,
    /// A bare name, a fully-qualified name, **or an operator token** —
    /// `:signature` / `:source` / `:doc`. These are the named-item
    /// views that resolve an operator binding (`specs/cli.md` §
    /// `kio repl`: `:signature` "resolves any named declaration —
    /// `fn`, `op`, …"; `:source` / `:doc` view the same item), so the
    /// menu offers operator tokens here.
    NameFqnOrOp,
    /// A bare name, fully-qualified name, operator token, or builtin
    /// name — `:doc`.
    NameFqnOpOrBuiltin,
    /// An arbitrary expression — `:normalize`. There is no candidate
    /// set; the editor's own line-editing handles the bare expression.
    Expression,
}

/// The [`CompletionShape`] for a **canonical** command name (one of
/// [`CANONICAL_COMMANDS`]). The caller folds synonyms through
/// [`canonical_name`] first; an unrecognized name yields
/// [`CompletionShape::None`] (nothing to complete).
pub fn completion_shape(canonical_command: &str) -> CompletionShape {
    match canonical_command {
        // Module-path arguments.
        "load" | "unload" | "ls" => CompletionShape::ModulePath,
        // Named-item views that resolve an operator binding too.
        "signature" | "source" => CompletionShape::NameFqnOrOp,
        "doc" => CompletionShape::NameFqnOpOrBuiltin,
        // Name / FQN arguments without operators.
        "t" | "pure" | "refs" => CompletionShape::NameOrFqn,
        "which" => CompletionShape::NameOrFqnOrBuiltin,
        // A bare expression — no candidate set.
        "normalize" => CompletionShape::Expression,
        // No-argument commands (and anything unrecognized) complete
        // to nothing.
        _ => CompletionShape::None,
    }
}

/// The input categories accepted by an argument-taking command.
///
/// An [`Expression`](Self::Expression) slot takes only a Kio value
/// expression. A [`Fqn`](Self::Fqn) slot takes a name or fully-qualified
/// path that denotes a declared entity. [`NameOrExpression`](Self::NameOrExpression)
/// keeps those two categories distinct while accepting either: `:t`
/// and `:pure` resolve a name/FQN as a named item, and otherwise
/// type-check an expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMode {
    /// The argument is an expression to inspect — `:normalize`.
    Expression,
    /// The argument is either a name/FQN or an expression — `:type` /
    /// `:pure`.
    NameOrExpression,
    /// The argument denotes a declared entity by name or fully-qualified
    /// path — `:load` / `:unload` / `:ls` / `:signature` / `:doc` /
    /// `:source` / `:which` / `:refs`.
    Fqn,
}

impl CommandMode {
    /// Whether this mode admits a Kio expression input.
    pub(crate) const fn accepts_expression(self) -> bool {
        matches!(self, Self::Expression | Self::NameOrExpression)
    }

    /// Whether this mode admits a declared name or FQN input.
    #[cfg(test)]
    const fn accepts_name(self) -> bool {
        matches!(self, Self::Fqn | Self::NameOrExpression)
    }
}

/// The [`CommandMode`] a **canonical** command accepts for its argument,
/// or `None` for a no-argument command (`:mods` / `:scope` / the
/// session controls). The caller folds synonyms through
/// [`canonical_name`] first.
///
/// This is the single source of truth for the value-vs-declared-entity
/// seam: every command's per-site failure path looks its mode up here
/// (via [`hint_for`]) rather than naming a [`CommandMode`] literal, so
/// the seam can't drift command-to-command.
pub(crate) fn command_mode(canonical_command: &str) -> Option<CommandMode> {
    match canonical_command {
        // Dual query — resolve a name/FQN or type-check an expression.
        "t" | "pure" => Some(CommandMode::NameOrExpression),
        // Value slot — an expression to inspect.
        "normalize" => Some(CommandMode::Expression),
        // Declared-entity slots — a name or fully-qualified path.
        "load" | "unload" | "ls" | "signature" | "doc" | "source" | "which" | "refs" => {
            Some(CommandMode::Fqn)
        }
        // No-argument commands have no mode to commit.
        _ => None,
    }
}

/// The fallible sibling-command correction for a single-category command
/// that **failed in its own mode**. A [`NameOrExpression`](CommandMode::NameOrExpression)
/// command has no mismatch between these categories and never returns a hint.
///
/// Returns `Some(hint)` only when `arg` *syntactically looks like the
/// other mode* than `mode` — a cheap lexer-level guess that **only
/// gates the hint**. The caller appends the hint to the command's own
/// failure diagnostic; it never overrides a command that succeeds in
/// its own mode, and the REPL never silently re-dispatches to the
/// sibling. The order is fixed at the call sites: (1) run the argument
/// in the command's own mode; (2) only if that fails; (3) only if this
/// returns `Some`.
///
/// - An [`Expression`](CommandMode::Expression)-slot argument with
///   *FQN shape* — a slash-qualified path (`foo/bar.Foo`), which
///   denotes a declared entity rather than a value — suggests the
///   declared-entity views `:signature` / `:doc`.
/// - An [`Fqn`](CommandMode::Fqn)-slot argument that is a *compound
///   expression* (`1 + 2`, `foo()`) suggests the value views `:type` /
///   `:normalize`.
///
/// The shape test delegates to [`classify_input`], so it tracks
/// the syntactically unambiguous FQN form: a slash module path plus a
/// dot item.
fn mode_mismatch_hint(mode: CommandMode, arg: &str) -> Option<String> {
    match (mode, classify_input(arg)?) {
        // An expression slot handed a slash FQN: it denotes a declared
        // entity, not a value. Dotted-only names stay in the value /
        // member namespace and do not trip this hint.
        (CommandMode::Expression, InputKind::BareName(name))
            if name.contains('/') && name.contains('.') =>
        {
            Some(format!(
                "`{name}` looks like a qualified name — `:signature {name}` prints its \
                     declaration header, `:doc {name}` its doc-comment + signature"
            ))
        }
        // An FQN slot handed a compound expression: it's a value to
        // inspect, not a declared entity.
        (CommandMode::Fqn, InputKind::Compound(expr)) => Some(format!(
            "`{expr}` looks like an expression — `:type {expr}` prints its type, \
             `:normalize {expr}` its residual normal form"
        )),
        // The dual queries accept both categories, so neither one is a mode
        // mismatch.
        (CommandMode::NameOrExpression, _) => None,
        _ => None,
    }
}

/// Append the fallible sibling-command correction to `diagnostic` when
/// `arg` — handed to `canonical_command`, which has just failed in its
/// own mode — syntactically looks like the other mode. The command's
/// mode is looked up in [`command_mode`] (the single source of truth
/// for the value-vs-declared-entity seam); a command with no committed
/// mode, or an argument shaped for the command's own mode, yields the
/// diagnostic unchanged — the hint is purely additive.
fn hint_for(diagnostic: String, canonical_command: &str, arg: &str) -> String {
    match command_mode(canonical_command).and_then(|mode| mode_mismatch_hint(mode, arg)) {
        Some(hint) => format!("{diagnostic}\n  {hint}"),
        None => diagnostic,
    }
}

/// Fold a command spelling (without the leading `:`) onto its
/// canonical name. An already-canonical name maps to itself; an
/// unknown name maps to itself too (the caller reports it as
/// unknown, or — for the completer — folds it to
/// [`CompletionShape::None`]).
pub(crate) fn canonical_name(spelling: &str) -> &str {
    // `t` / `ls` / `mods` / `refs` are the canonical short forms;
    // their long synonyms (`type` / `list` / `modules` /
    // `references`) fold *onto* them, and `l` / `u` / `q` / `h`
    // fold onto their long canonical forms.
    for (short, long) in SYNONYMS {
        if spelling == *short {
            return long;
        }
    }
    spelling
}

/// Per-command usage hint for the "missing argument" diagnostic.
/// Lets `:doc` / `:t` / `:pure` / `:source` / `:signature` / `:normalize` /
/// `:load` / `:unload` / `:which` / `:refs` each tell the user what shape
/// they expected, rather than a generic "see `:help`" pointer.
fn usage_for(canonical: &str) -> String {
    match canonical {
        "doc" => "Usage: :doc <name> — render a name's doc-comment + signature, or a \
                  loaded module's summary"
            .to_owned(),
        "t" => "Usage: :type <name-or-expression> — print the synthesized type \
                  (`:t` is the short form)"
            .to_owned(),
        "pure" => "Usage: :pure <name-or-expression> — report whether an expression or \
                   function is pure"
            .to_owned(),
        "source" => "Usage: :source <name> — print a name's canonical source, \
                  prefixed by any preceding `//`/`///` comment block"
            .to_owned(),
        "signature" => "Usage: :signature <name> — print a name's declaration header".to_owned(),
        "normalize" => {
            "Usage: :normalize <expression> — print an expression's residual normal form".to_owned()
        }
        "load" => "Usage: :load <module-path> — load a regular module (and its \
                  import-closure)"
            .to_owned(),
        "unload" => "Usage: :unload <module-path> — remove a loaded module".to_owned(),
        "which" => "Usage: :which <name> — report which loaded module declares a name".to_owned(),
        "refs" => "Usage: :refs <name> — list every reference to a name across loaded \
                  modules"
            .to_owned(),
        other => format!("`:{other}` needs an argument — see `:help`"),
    }
}

/// Parse one REPL input line into a [`Command`].
///
/// A line beginning with `:` is a meta-command: the first
/// whitespace-delimited token after the `:` is the command name
/// (synonyms accepted); the remainder, trimmed, is the single
/// argument. Commands that take no argument reject a non-empty
/// remainder; commands that need one reject an empty remainder.
///
/// A line that does not begin with `:` is an expression query —
/// [`Command::Expr`] carries the raw line; the dispatcher classifies
/// it as a bare name or a compound expression at run time.
/// Split a `-v` (verbose) flag out of a `:ls` / `:scope` argument
/// remainder. Returns `(verbose, rest)` where `rest` is the argument
/// with the flag token removed and re-trimmed. The flag is recognised
/// in any whitespace-delimited position (`:ls -v a/b`, `:ls a/b -v`);
/// only the exact token `-v` counts, so a module named `-v`-anything is
/// untouched.
fn split_verbose_flag(arg: &str) -> (bool, String) {
    let mut verbose = false;
    let kept: Vec<&str> = arg
        .split_whitespace()
        .filter(|tok| {
            if *tok == "-v" {
                verbose = true;
                false
            } else {
                true
            }
        })
        .collect();
    (verbose, kept.join(" "))
}

pub fn parse_command(line: &str) -> Result<Command, ParseError> {
    let line = line.trim();
    let Some(rest) = line.strip_prefix(':') else {
        // A non-`:` line is a bare-input query.
        return Ok(Command::Expr(line.to_owned()));
    };
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Err(ParseError(
            "empty command — type `:help` for the command list".to_owned(),
        ));
    }
    // Split into the command word and the argument remainder.
    let (word, arg) = match rest.split_once(char::is_whitespace) {
        Some((w, a)) => (w, a.trim()),
        None => (rest, ""),
    };
    let name = canonical_name(word);

    /// Require a non-empty argument; produce a usage-hint error
    /// otherwise. The hint is per-command so the user sees what
    /// shape the command expected, rather than a generic "see
    /// `:help`" pointer.
    fn need_arg<'a>(name: &str, arg: &'a str) -> Result<&'a str, ParseError> {
        if arg.is_empty() {
            Err(ParseError(usage_for(name)))
        } else {
            Ok(arg)
        }
    }

    /// Reject an argument for a no-argument command.
    fn no_arg(name: &str, arg: &str) -> Result<(), ParseError> {
        if arg.is_empty() {
            Ok(())
        } else {
            Err(ParseError(format!(
                "`:{name}` takes no argument (got `{arg}`)"
            )))
        }
    }

    match name {
        "load" => Ok(Command::Load(need_arg("load", arg)?.to_owned())),
        "t" => Ok(Command::Type(need_arg("t", arg)?.to_owned())),
        "pure" => Ok(Command::Pure(need_arg("pure", arg)?.to_owned())),
        "signature" => Ok(Command::Signature(need_arg("signature", arg)?.to_owned())),
        "normalize" => Ok(Command::Normalize(need_arg("normalize", arg)?.to_owned())),
        "doc" => Ok(Command::Doc(need_arg("doc", arg)?.to_owned())),
        "source" => Ok(Command::Source(need_arg("source", arg)?.to_owned())),
        "ls" => {
            let (verbose, module_arg) = split_verbose_flag(arg);
            Ok(Command::Ls {
                module: if module_arg.is_empty() {
                    None
                } else {
                    Some(module_arg)
                },
                verbose,
            })
        }
        "unload" => Ok(Command::Unload(need_arg("unload", arg)?.to_owned())),
        "which" => Ok(Command::Which(need_arg("which", arg)?.to_owned())),
        "refs" => Ok(Command::Refs(need_arg("refs", arg)?.to_owned())),
        "mods" => {
            no_arg("mods", arg)?;
            Ok(Command::Mods)
        }
        "packages" => Ok(Command::Packages(if arg.is_empty() {
            None
        } else {
            Some(arg.to_owned())
        })),
        "scope" => {
            let (verbose, rest) = split_verbose_flag(arg);
            // `:scope` takes no positional argument — only the `-v` flag.
            no_arg("scope", &rest)?;
            Ok(Command::Scope { verbose })
        }
        "help" => {
            no_arg("help", arg)?;
            Ok(Command::Help)
        }
        "quit" => {
            no_arg("quit", arg)?;
            Ok(Command::Quit)
        }
        "reset" => {
            no_arg("reset", arg)?;
            Ok(Command::Reset)
        }
        other => Err(ParseError(format!(
            "unknown command `:{other}` — type `:help` for the command list"
        ))),
    }
}

impl Command {
    /// Run this command against `session`, formatting output with
    /// `palette`. The returned [`Outcome`] carries the text to print
    /// and whether the loop continues.
    pub fn run(&self, session: &mut Session, palette: Palette) -> Outcome {
        match self {
            Command::Load(path) => Outcome::say(cmd_load(session, path, palette)),
            Command::Type(arg) => Outcome::say(cmd_type(session, arg, palette)),
            Command::Pure(arg) => Outcome::say(cmd_pure(session, arg)),
            Command::Signature(name) => Outcome::say(cmd_signature(session, name, palette)),
            Command::Normalize(expr) => Outcome::say(cmd_normalize(session, expr, palette)),
            Command::Expr(input) => Outcome::say(cmd_expr(session, input, palette)),
            Command::Doc(name) => Outcome::say(cmd_doc(session, name, palette)),
            Command::Source(name) => Outcome::say(cmd_source(session, name, palette)),
            Command::Ls { module, verbose } => {
                Outcome::say(cmd_ls(session, module.as_deref(), *verbose, palette))
            }
            Command::Mods => Outcome::say(cmd_mods(session)),
            Command::Packages(name) => {
                Outcome::say(cmd_packages(session, name.as_deref(), palette))
            }
            Command::Unload(path) => Outcome::say(cmd_unload(session, path)),
            Command::Which(name) => Outcome::say(cmd_which(session, name)),
            Command::Refs(name) => Outcome::say(cmd_refs(session, name)),
            Command::Scope { verbose } => Outcome::say(cmd_scope(session, *verbose, palette)),
            Command::Help => Outcome::say(help_text()),
            Command::Reset => {
                session.reset();
                Outcome::say("session reset — no modules loaded".to_owned())
            }
            Command::Quit => Outcome::quit(),
        }
    }
}

// ── :load ───────────────────────────────────────────────────────────────────

/// `:load <module-path>` — load a module and its `import`-closure.
///
/// The whole load commits or fails atomically: the package is
/// type-checked as a unit (any type error aborts the load), the
/// module path must name a module the package defines, and every
/// local `import`-closure dependency is staged alongside. On success
/// the new modules join the session and the explicit one becomes
/// current.
/// Startup-load entry — load `module_path` and its `import`-closure into the
/// session, suppressing the per-module "loaded …" status line. Used
/// by the startup auto-load path (see [`super::initial_load`]) where
/// the banner enumerates the full result and per-module output would
/// be noise. Returns the empty string on success, or the planner's
/// failure message on failure (the caller decides how to surface it).
#[cfg(feature = "repl")]
pub fn cmd_load_for_startup(session: &mut Session, module_path: &str, palette: Palette) -> String {
    // Delegate to the user-facing entry; discard the success status
    // (the banner already enumerates loaded modules), keep the
    // failure status (a load can fail with the package half-broken,
    // and a silent failure would leave the user wondering).
    let out = cmd_load(session, module_path, palette);
    if session.module(module_path).is_some() {
        String::new()
    } else {
        out
    }
}

fn cmd_load(session: &mut Session, arg: &str, palette: Palette) -> String {
    let key = module_arg_to_key(arg);
    let surface = surface_module_path(&key);

    // A successful refresh is the prerequisite — it both type-checks
    // the package and gives us the module → file map the planner
    // needs. A type error aborts the load with the session unchanged.
    if let Err(failure) = session.refresh() {
        return format!(
            "load aborted — the package does not type-check:\n  {}",
            render_failure(&failure)
        );
    }

    // The path must name a regular module.
    if session.module_file(&key).is_none() {
        let known = session.package_module_paths();
        let mut msg = format!("no regular module `{surface}`");
        if !known.is_empty() {
            let surface_known: Vec<String> = known.iter().map(|k| surface_module_path(k)).collect();
            let _ = write!(msg, "\n  regular modules: {}", surface_known.join(", "));
        }
        // A compound-expression argument suggests the value views.
        return hint_for(msg, "load", arg);
    }

    // Stage the explicit module plus its transitive local
    // `import`-closure. Only modules present in the current package
    // analysis are session-loadable.
    let staged = match plan_load(session, &key) {
        Ok(s) => s,
        Err(msg) => return msg,
    };

    session.commit_load(&key, &staged);

    // Report what was loaded.
    let mut out = format!("loaded {}", highlight(&surface, palette));
    let implicit: Vec<String> = staged
        .iter()
        .map(|s| s.path.as_str())
        .filter(|p| *p != key)
        .map(surface_module_path)
        .collect();
    if !implicit.is_empty() {
        let _ = write!(out, "\n  pulled in: {}", implicit.join(", "));
    }
    out
}

/// Walk the local `import`-closure of `module_path`, parsing each module from
/// the cached analysis's source map. Returns the staged set (the
/// explicit module first). An unresolvable *local* dependency or a
/// parse failure aborts the whole plan — the load is atomic.
fn plan_load(session: &Session, module_path: &str) -> Result<Vec<StagedModule>, String> {
    let mut staged: Vec<StagedModule> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut frontier = vec![module_path.to_owned()];
    let package_modules: std::collections::BTreeSet<String> =
        session.package_module_paths().into_iter().collect();
    session
        .analysis()
        .ok_or_else(|| "load aborted — package analysis is unavailable".to_owned())?;

    while let Some(path) = frontier.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let file_path = match session.module_file(&path) {
            Some(f) => f,
            // Not a module in the current package analysis. Skip it:
            // only local-package modules are session-loadable.
            None => continue,
        };
        let source = match session.source_of(&file_path) {
            Some(s) => s,
            None => {
                return Err(format!(
                    "load aborted — cannot read source for module `{path}`"
                ));
            }
        };
        let module: Module<Surface> = match crate::pass::parser::parse(source) {
            Ok(m) => m,
            Err(e) => {
                let (_, msg) = e.diag();
                return Err(format!(
                    "load aborted — module `{path}` failed to parse: {msg}"
                ));
            }
        };
        // Queue this module's `import`-closure deps that are local
        // package modules.
        for dep in import_clause_targets(&module) {
            if package_modules.contains(&dep) && !seen.contains(&dep) {
                frontier.push(dep);
            }
        }
        staged.push(StagedModule {
            path,
            file_path,
            module,
        });
    }

    // Order the explicit module first for a tidy "pulled in" report;
    // `commit_load` does not depend on order.
    staged.sort_by_key(|s| (s.path != module_path, s.path.clone()));
    Ok(staged)
}

// ── :t ──────────────────────────────────────────────────────────────────────

/// `:t <name-or-expression>` — print the synthesized type.
///
/// `:t` is the pure type query. A bare *name* resolves through the
/// current scope (or by FQN) to a top-level item: a `fn`
/// reports its function type. A `newtype` / `type` / `labels` is a
/// type-level name — it binds no value — and is a kind-aware error
/// (the same message shape `kio doc check` produces for `@type`). A
/// `literal` gets its concrete type from each use site, so the query must
/// include an annotation.
/// Any other expression is type-checked and its synthesized type is
/// printed. A path-shaped bare-name token sequence can belong to either
/// category: a resolvable name/FQN or loaded module stays on the name branch,
/// while an unresolved shape that parses as a value path (such as
/// `alias.item`) or with the current module's `/` operator uses the expression
/// branch.
fn cmd_type(session: &mut Session, arg: &str, palette: Palette) -> String {
    match classify_input(arg) {
        // The lexer classifies alias-qualified value paths (`value.item`),
        // item FQNs (`left/value.item`), and slash-only operator expressions
        // (`left/right`) as bare names. An entity that actually resolves wins.
        // Otherwise, let the current expression grammar decide whether this
        // path-shaped spelling belongs to the value category before reporting
        // the name-resolution error.
        Some(InputKind::BareName(name))
            if (name.contains('.') || name.contains('/'))
                && resolve_name(session, &name).is_none()
                && resolve_module_arg(session, &name).is_none()
                && expr_parse(session, &name).is_some() =>
        {
            cmd_type_expression(session, &name, palette)
        }
        // Any other bare-name shape — including a resolvable FQN — uses
        // the declared-name branch.
        Some(InputKind::BareName(name)) => cmd_type_name(session, &name, palette),
        // A compound expression — query it and report its type.
        Some(InputKind::Compound(expr)) => cmd_type_expression(session, &expr, palette),
        // The argument does not even tokenize.
        None => invalid_bare_input(arg),
    }
}

/// The expression branch of [`cmd_type`].
fn cmd_type_expression(session: &mut Session, expr: &str, palette: Palette) -> String {
    match query_expr_for_repl(session, expr) {
        Ok(result) => render_expr_type(expr, &result, palette),
        // An input committed to this branch that does not parse is neither a
        // resolvable name nor a parseable expression — surface the same
        // kind-classified guidance a bare line gives, never a raw (or
        // wrapper-leaked) parser error.
        Err(ReplExprQueryError::Query(ExprQueryError::Syntax(_))) => invalid_bare_input(expr),
        // A parseable expression that fails to type-check (or a
        // package / no-module failure) reports its own diagnostic.
        Err(e) => hint_for(render_expr_error(&e), "t", expr),
    }
}

/// The bare-name branch of `:t`: resolve `name` to a top-level item
/// and report its type or a kind-aware error.
fn cmd_type_name(session: &Session, name: &str, palette: Palette) -> String {
    let Some(found) = resolve_name(session, name).or_else(|| resolve_operator(session, name))
    else {
        // A module path (`op/main`) is a `BareName` via the `/`-only
        // shape, but a module is not a value — `:t` has no type to
        // report. Point at `:ls`, which lists the module's items.
        if !name.contains('.')
            && let Some(module_path) = resolve_module_arg(session, name)
        {
            let surface = surface_module_path(&module_path);
            return format!(
                "`{surface}` is a module, not a value — `:ls {surface}` lists its items"
            );
        }
        // This spelling reached the name branch but did not resolve. Keep the
        // named-entity diagnostic (with any applicable view hint) instead of
        // reclassifying it after expression parsing has already declined it.
        return hint_for(unresolved(session, name), "t", name);
    };
    if matches!(
        found.doc_entry(),
        Some(crate::doc_entry::DocEntry::LabelNominal { .. })
    ) {
        return type_level_error(&found.display_name, "a `newtype`");
    }
    match &found.item {
        Item::FnDef(f) => {
            let ty = fn_def_type(f);
            format!(
                "{} : {}",
                highlight(&found.display_name, palette),
                highlight_type(&crate::pretty::pretty_type(&ty), palette)
            )
        }
        Item::RecGroup(g, _) => {
            let Some(f) = rec_group_module_members(g).find(|f| f.name == found.short_name) else {
                unreachable!(
                    "resolved rec-group item is missing member `{}`",
                    found.short_name
                );
            };
            let ty = fn_def_type(f);
            format!(
                "{} : {}",
                highlight(&found.display_name, palette),
                highlight_type(&crate::pretty::pretty_type(&ty), palette)
            )
        }
        Item::TypeRecGroup(group) => match group.members.iter().find(|member| match member {
            crate::ast::TypeRecMember::TypeAlias(alias) => alias.name == found.short_name,
            crate::ast::TypeRecMember::Newtype(newtype) => newtype.name == found.short_name,
            crate::ast::TypeRecMember::Labels(labels, _) => {
                labels.type_alias_name.as_deref() == Some(found.short_name.as_str())
            }
        }) {
            Some(crate::ast::TypeRecMember::TypeAlias(_)) => {
                type_level_error(&found.display_name, "a type alias")
            }
            Some(crate::ast::TypeRecMember::Newtype(_)) => {
                type_level_error(&found.display_name, "a `newtype`")
            }
            Some(crate::ast::TypeRecMember::Labels(_, _)) => {
                type_level_error(&found.display_name, "a `labels` declaration")
            }
            None => unreachable!(
                "resolved type-recursive group is missing member `{}`",
                found.short_name
            ),
        },
        // A type-level name binds no value — `:type` is a kind-aware
        // error, phrased the same way the Kiodoc `@type` directive is.
        Item::Newtype(_) => type_level_error(&found.display_name, "a `newtype`"),
        Item::TypeAlias(_) => type_level_error(&found.display_name, "a type alias"),
        Item::LiteralAlias(_, _) => literal_alias_type_error(&found.display_name),
        Item::Labels(_, _) => type_level_error(&found.display_name, "a `labels` declaration"),
        Item::LabelForward(_, _) => type_level_error(&found.display_name, "a label declaration"),
        Item::Op(_, _) => format!(
            "`{}` is an operator binding — use `:source {}`",
            found.display_name, found.display_name
        ),
        Item::VariadicOperator(_, _) => format!(
            "`{}` is a variadic operator binding — use `:source {}`",
            found.display_name, found.display_name
        ),
        Item::Equiv(_, _) => format!("`{}` is an `equiv` declaration", found.display_name),
        Item::Elaborator(_, _) => format!("`{}` is an elaborator declaration", found.display_name),
        // A `host fn` binds a value (its declared function type); a
        // `host type` is type-level.
        Item::HostFn(_) => match crate::pretty::pretty_item_type(&found.item) {
            Some(ty) => format!(
                "{} : {}",
                highlight(&found.display_name, palette),
                highlight_type(&ty, palette)
            ),
            None => type_level_error(&found.display_name, "a `host type`"),
        },
        Item::HostType(_) => type_level_error(&found.display_name, "a `host type`"),
    }
}

/// The kind-aware "`:type` on a type-level name" diagnostic — the
/// REPL twin of `kio doc check`'s `@type` rejection.
fn type_level_error(name: &str, kind: &str) -> String {
    format!("`{name}` is {kind}; `:type` expects a value binding (fn or host fn)")
}

fn literal_alias_type_error(name: &str) -> String {
    format!("`{name}` is a literal alias; use an annotated expression such as `{name}(Type)`")
}

// ── :pure ───────────────────────────────────────────────────────────────────────

/// `:pure <name-or-expression>` — report the compiler's purity verdict.
///
/// A resolvable name or FQN reads the declaration's purity contract. An
/// expression is checked in a synthetic `pure fn` body; a structural retry in
/// an ordinary function distinguishes an impure expression from invalid input.
/// Neither branch evaluates the queried value.
fn cmd_pure(session: &mut Session, arg: &str) -> String {
    match classify_input(arg) {
        // Keep the same category boundary as `:t`: a declaration that resolves
        // wins, while a dotted value path or `/` operator expression that the
        // current parser accepts belongs to the expression branch.
        Some(InputKind::BareName(name))
            if (name.contains('.') || name.contains('/'))
                && resolve_name(session, &name).is_none()
                && resolve_module_arg(session, &name).is_none()
                && expr_parse(session, &name).is_some() =>
        {
            cmd_pure_expression(session, &name)
        }
        Some(InputKind::BareName(name)) => cmd_pure_name(session, &name),
        Some(InputKind::Compound(expr)) => cmd_pure_expression(session, &expr),
        None => invalid_bare_input(arg),
    }
}

/// The expression branch of [`cmd_pure`].
fn cmd_pure_expression(session: &mut Session, expr: &str) -> String {
    match query_expr_purity_for_repl(session, expr) {
        Ok(verdict) => verdict.as_str().to_owned(),
        Err(ReplExprQueryError::Query(ExprQueryError::Syntax(_))) => invalid_bare_input(expr),
        Err(error) => hint_for(render_expr_error(&error), "pure", expr),
    }
}

/// The declaration branch of [`cmd_pure`].
fn cmd_pure_name(session: &Session, name: &str) -> String {
    let Some(found) = resolve_name(session, name).or_else(|| resolve_operator(session, name))
    else {
        if !name.contains('.')
            && let Some(module_path) = resolve_module_arg(session, name)
        {
            let surface = surface_module_path(&module_path);
            return format!(
                "`{surface}` is a module, not an executable value — `:ls {surface}` lists its items"
            );
        }
        return hint_for(unresolved(session, name), "pure", name);
    };
    if matches!(
        found.doc_entry(),
        Some(crate::doc_entry::DocEntry::LabelNominal { .. })
    ) {
        return purity_kind_error(&found.display_name, "a `newtype`");
    }

    match found.selected_item() {
        Item::FnDef(function) => {
            if function.purity.is_pure() {
                "pure".to_owned()
            } else {
                "impure".to_owned()
            }
        }
        // Host functions are executable values whose implementation lies
        // outside the compiler's pure-function contract.
        Item::HostFn(_) => "impure".to_owned(),
        Item::Newtype(_) => purity_kind_error(&found.display_name, "a `newtype`"),
        Item::TypeAlias(_) => purity_kind_error(&found.display_name, "a type alias"),
        Item::LiteralAlias(_, _) => purity_kind_error(&found.display_name, "a literal alias"),
        Item::Labels(_, _) => purity_kind_error(&found.display_name, "a `labels` declaration"),
        Item::LabelForward(_, _) => purity_kind_error(&found.display_name, "a label declaration"),
        Item::Op(_, _) => purity_kind_error(&found.display_name, "an operator binding"),
        Item::VariadicOperator(_, _) => {
            purity_kind_error(&found.display_name, "a variadic operator binding")
        }
        Item::Equiv(_, _) => purity_kind_error(&found.display_name, "an `equiv` declaration"),
        Item::Elaborator(_, _) => {
            purity_kind_error(&found.display_name, "an elaborator declaration")
        }
        Item::HostType(_) => purity_kind_error(&found.display_name, "a `host type`"),
        Item::RecGroup(_, _) => unreachable!("selected recursion-group item is not a function"),
        Item::TypeRecGroup(_) => {
            unreachable!("selected type-recursive-group item was not projected to its member")
        }
    }
}

fn purity_kind_error(name: &str, kind: &str) -> String {
    format!("`{name}` is {kind}; `:pure` expects an executable value binding (fn or host fn)")
}

// ── :normalize and bare expression queries ───────────────────────────────────

/// `:normalize <expression>` — print the residual normal form.
///
/// Type-checks and reduces the expression unconditionally — even a
/// bare name, which a bare-input line would otherwise treat as a
/// kind-aware shortcut. The output is the **residual NF**: the value
/// left after applying every reduction the host-independent partial
/// evaluator admits — β-reduction, `let`-unfolding, fn-call inlining,
/// `match!` / `if`/`else` / elaboration. Pure expressions
/// collapse to ground values; host items remain as opaque atoms;
/// applications that can't be reduced residualize as `callee(arg, …)`.
fn cmd_normalize(session: &mut Session, expr: &str, palette: Palette) -> String {
    match query_expr_for_repl(session, expr) {
        Ok(result) => render_normal_form(&result, palette),
        // A syntax failure can still have the separate name/FQN shape. In
        // that case report the entity-view correction directly: calling it
        // "not a name" would contradict the shared input classifier. The
        // expression attempt must still come first because `/` can be a
        // registered operator and its right operand can be dotted.
        Err(ReplExprQueryError::Query(ExprQueryError::Syntax(_))) => command_mode("normalize")
            .and_then(|mode| mode_mismatch_hint(mode, expr))
            .unwrap_or_else(|| invalid_bare_input(expr)),
        // On other failures, a qualified-name argument (which denotes a
        // declared entity, not a value to reduce) suggests the
        // declared-entity views.
        Err(e) => hint_for(render_expr_error(&e), "normalize", expr),
    }
}

/// The syntactic kinds a bare input matches — an input can match more
/// than one (a bare identifier is both a [`name`](Self::name) and an
/// [`expression`](Self::expression)). Drives both the bare-input footer
/// and the command-error redirects, so the two never disagree on what an
/// input "is".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct BareInputKinds {
    /// Lexes as an identifier / operator / dotted path / fully-qualified
    /// path (`mod/path.item`) / module path. Name-*kind* even if it does
    /// not resolve — a "not found" is a separate message.
    name: bool,
    /// Parses as a Kio expression (a literal, an application, an operator
    /// expression — and a bare identifier, which is both).
    expression: bool,
    /// A loaded module's path (a `/`-joined path or a single identifier
    /// naming a loaded module). The `:ls` / `:unload` views key on this.
    module_path: bool,
}

impl BareInputKinds {
    /// `true` when the input matched no kind — neither a name, nor an
    /// expression, nor a module path.
    fn is_invalid(self) -> bool {
        !self.name && !self.expression && !self.module_path
    }
}

/// Classify a bare input into the kinds it matches against the current
/// session — the shared classifier behind the bare-input footer and the
/// command-error redirects.
fn bare_input_kinds(session: &Session, input: &str) -> BareInputKinds {
    let mut kinds = BareInputKinds::default();
    if let Some(InputKind::BareName(name)) = classify_input(input) {
        kinds.name = true;
        // A `/`-joined path, or a single identifier, that names a loaded
        // module is module-path-kind too. A mixed FQN (`mod/path.item`)
        // carries a `.` and denotes an item, not a module, so it is not.
        if !name.contains('.') && resolve_module_arg(session, &name).is_some() {
            kinds.module_path = true;
        }
    }
    if expr_parse(session, input).is_some() {
        kinds.expression = true;
    }
    kinds
}

/// Every command applicable to the matched `kinds` — canonical names,
/// in [`CANONICAL_COMMANDS`] order (the order `:help` lists them), so
/// every surface presents commands identically. The name and
/// module-path lists are curated judgments; the expression list
/// derives from [`command_mode`], since "takes an expression" is
/// exactly the property that makes a command applicable to expression
/// input. The map is verified against the actual command table by
/// [`applicable_commands_match_the_command_table`].
fn applicable_commands(kinds: BareInputKinds) -> Vec<&'static str> {
    let mut wanted: Vec<&'static str> = Vec::new();
    if kinds.name {
        wanted.extend(["signature", "doc", "source", "t", "pure", "refs", "which"]);
    }
    if kinds.expression {
        wanted.extend(
            CANONICAL_COMMANDS
                .iter()
                .copied()
                .filter(|c| command_mode(c).is_some_and(CommandMode::accepts_expression)),
        );
    }
    if kinds.module_path {
        wanted.extend(["ls", "unload", "doc", "which"]);
    }
    CANONICAL_COMMANDS
        .iter()
        .copied()
        .filter(|c| wanted.contains(c))
        .collect()
}

/// A bare (non-`:`) input query.
///
/// **Dispatch.** Each candidate command exposes a parser ([`doc_parse`],
/// [`expr_parse`]). The router tries them in order; the FIRST parser
/// to return `Some(_)` wins, and the dispatched command's own error
/// renderer (if needed) takes over from there. There is no separate
/// "can_handle" predicate — the parser IS the can-handle check, so
/// the dispatched command's parse step cannot diverge from the
/// router's choice.
///
/// 1. **`:doc`** wins for bare names — a single identifier, a dotted
///    path, an operator symbol, or a loaded module path. The doc-
///    comment + signature for the named item (or the module summary,
///    via [`cmd_doc`]) prints.
/// 2. **The expression views** win for anything that parses as a Kio
///    expression — a literal, an application, an operator expression.
///    Both the synthesized type and the residual normal form print:
///    host-touching expressions commonly stay stuck as residual trees,
///    and the type line is the reliable summary when they do.
///
/// After the default action runs, a one-line footer lists every command
/// applicable to the matched kind(s), so the user discovers the other
/// views of the same input. When the input matches **no** kind — neither
/// a name, an expression, nor a module path — the default action is
/// dropped and only the invalid-input line prints.
fn cmd_expr(session: &mut Session, input: &str, palette: Palette) -> String {
    let kinds = bare_input_kinds(session, input);
    if kinds.is_invalid() {
        return invalid_bare_input(input);
    }
    let default_output = if let Some(name) = doc_parse(input) {
        cmd_doc(session, &name, palette)
    } else if let Some(expr_src) = expr_parse(session, input) {
        match query_expr_for_repl(session, &expr_src) {
            Ok(result) => format!(
                "{}\n{}",
                render_expr_type(&expr_src, &result, palette),
                render_normal_form(&result, palette)
            ),
            Err(e) => render_expr_error(&e),
        }
    } else {
        // `is_invalid` is false, so at least one kind matched; the only
        // way to reach here is a module-path-only input the `doc_parse` /
        // `expr_parse` pair both decline — route it to `:doc`, which
        // renders the module summary.
        cmd_doc(session, input.trim(), palette)
    };
    format!(
        "{default_output}\n{}",
        applicable_commands_footer(kinds, palette)
    )
}

/// The dim one-line footer advertising every command applicable to the
/// matched `kinds`, deduped — the bare-input twin of `:ls -v`'s
/// self-advertising trailer. Each command shows its full spelling
/// ([`display_spellings`]' lead form), the same presentation `:help`'s
/// primary column uses.
fn applicable_commands_footer(kinds: BareInputKinds, palette: Palette) -> String {
    let cmds: Vec<String> = applicable_commands(kinds)
        .into_iter()
        .map(|canonical| format!(":{}", display_spellings(canonical)[0]))
        .collect();
    super::highlight::dim(&format!("(also: {})", cmds.join("  ")), palette)
}

/// The bare-input message for input that matches no kind — neither a
/// name, an expression, nor a module path. Retires the old fixed
/// `:doc`/`:normalize` two-command dump in favour of a single honest
/// line.
fn invalid_bare_input(input: &str) -> String {
    let trimmed = input.trim();
    format!("`{trimmed}` is not a name or a Kio expression — type `:help` for the command list")
}

/// `:doc` parser. Returns `Some(name)` for input that lexes as a bare
/// name — a single identifier, a dotted path, an operator symbol, or
/// a parenthesized operator — i.e. anything `:doc <name>` could
/// resolve. The parser does **not** check that the name resolves;
/// resolution failures land inside `:doc`'s own diagnostic ([`unresolved`]),
/// not in the dispatch step.
fn doc_parse(input: &str) -> Option<String> {
    match classify_input(input)? {
        InputKind::BareName(name) => Some(name),
        InputKind::Compound(_) => None,
    }
}

/// The expression parser behind the `:normalize` / bare-expression
/// dispatch. Returns `Some(expr_src)` for input that the Kio parser
/// accepts as an expression inside the current module's body (via the
/// same synthetic-wrap scheme [`super::expr_query::query_expr`] uses). Returns `None`
/// when the input fails to parse — that is the signal for the router to
/// fall through to the invalid-input message.
///
/// **Why the current module's body.** A user operator (`op _ + __ { impl add }`)
/// is module-local, so the parser only accepts `1 + 2` inside
/// a module that declares `+`. Parsing against a vanilla skeleton
/// would falsely reject every operator expression in the user's
/// session. The wrap appends a synthetic fn to the current module's
/// own source — same shape [`super::expr_query::query_expr`] uses for the typed run.
///
/// **No current module.** Without a current module there's nothing
/// to splice into. Fall back to a vanilla skeleton: an empty wrapper
/// rejects most expressions (operators won't lex through), but
/// `:normalize` would have failed anyway — the router's job here is to
/// decide "fall through or not", and `:normalize`'s own
/// NoCurrentModule diagnostic is the right output.
///
/// The wrap parse is cheap (lex + parse only; no typecheck) and is
/// what lets the router distinguish "valid expression that may still
/// fail to type-check" (run `:normalize`, surface the type error) from
/// "syntactically not even an expression" (fall through to the
/// invalid-input message).
fn expr_parse(session: &Session, input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let wrapped = match session.current().and_then(|p| session.module(p)) {
        Some(entry) => match session.source_of(&entry.file_path) {
            Some(src) => {
                // Append the synthetic fn to the current module's source.
                // Mirrors `super::expr_query::SyntheticWrap::build` shape.
                format!("{src}\nfn _repl_parse_check_() {{\n  let _v_ = {trimmed};\n  ()\n}}\n")
            }
            // The current module is loaded but its source isn't in
            // the analysis cache — fall through to a vanilla wrap.
            None => format!(
                "module _repl_dispatch_;\nfn _repl_parse_check_() {{\n  let _v_ = {trimmed};\n  ()\n}}\n"
            ),
        },
        None => format!(
            "module _repl_dispatch_;\nfn _repl_parse_check_() {{\n  let _v_ = {trimmed};\n  ()\n}}\n"
        ),
    };
    match crate::pass::parser::parse(&wrapped) {
        Ok(_) => Some(trimmed.to_owned()),
        Err(_) => None,
    }
}

// ── :signature ──────────────────────────────────────────────────────────────

/// `:signature <name>` — print a named item's declaration header.
///
/// The header is the item's declaration line with no body — the same
/// artifact Kiodoc's `` [`@signature term`] `` directive embeds. For
/// a `fn` this is `fn name[type-params](value-params) -> Ret`; for a
/// `newtype` / `type` / `literal` / `labels` / `op` the declaration is itself
/// header-shaped.
fn cmd_signature(session: &Session, name: &str, palette: Palette) -> String {
    signature_of(session, name, palette)
}

/// Resolve `name` and render its declaration header — the body of
/// both `:signature` and the bare-name shortcut. The header renderer
/// ([`crate::pretty::pretty_item_signature`]) is the same one behind
/// Kiodoc's `@signature` directive, so the three surfaces produce
/// identical output by construction. An operator symbol resolves to
/// its `op` binding (which has no declared *name*, so `resolve_name`
/// would miss it).
fn signature_of(session: &Session, name: &str, palette: Palette) -> String {
    let Some(found) = resolve_name(session, name).or_else(|| resolve_operator(session, name))
    else {
        // It lexed as a name, so the problem is resolution, not syntax.
        // A compound-expression argument suggests the value views.
        return hint_for(unresolved(session, name), "signature", name);
    };
    let header = found.doc_entry().map_or_else(
        || crate::pretty::pretty_item_signature(&found.selected_item()),
        |entry| entry.signature(),
    );
    highlight(&header, palette)
}

/// Render an expression query's residual normal form.
///
/// `:normalize <expr>` and the bare-compound-expression query both
/// print the **residual NF** — the value left after applying every reduction
/// the host-independent partial evaluator admits, the same reduction relation
/// `specs/formal/equiv.md` pins for `equiv`-discharge. Closed pure
/// expressions reduce to ground values (`()`, `42`, `"hi"`, …);
/// host items remain as opaque atoms; applications that can't be
/// reduced residualize as `callee(arg, …)`.
fn render_normal_form(result: &ExprQueryResult, palette: Palette) -> String {
    highlight(&result.rendered_value, palette)
}

/// Render an expression query's synthesized type, prefixed by the
/// expression as the user typed it.
fn render_expr_type(expr_src: &str, result: &ExprQueryResult, palette: Palette) -> String {
    let ty = crate::pass::typecheck_core::display_type(&result.ty);
    format!(
        "{} : {}",
        highlight(expr_src, palette),
        highlight_type(&ty, palette)
    )
}

/// Render a command-facing expression-query failure as a user-facing diagnostic.
fn render_expr_error(err: &ReplExprQueryError) -> String {
    match err {
        ReplExprQueryError::Totality(message) => {
            format!("compile-time evaluation failed its Totality check: {message}")
        }
        ReplExprQueryError::Query(ExprQueryError::NoCurrentModule) => {
            "no current module — `:load <module-path>` loads one, \
             so expression queries with short names have something to resolve against"
                .to_owned()
        }
        ReplExprQueryError::Query(ExprQueryError::NoModuleSource) => {
            "the current module's source could not be read".to_owned()
        }
        ReplExprQueryError::Query(ExprQueryError::Syntax(msg)) => {
            format!("not a valid Kio expression: {msg}")
        }
        ReplExprQueryError::Query(ExprQueryError::TypeError(msg)) => {
            format!("the expression does not type-check: {msg}")
        }
        ReplExprQueryError::Query(ExprQueryError::PackageError(msg)) => {
            format!("the package does not type-check: {msg}")
        }
    }
}

// ── :doc ────────────────────────────────────────────────────────────────────

/// `:doc <name>` — render a name's `///` doc-comment plus its
/// signature; or, when `name` is a loaded module's path,
/// render the module's summary (item count + import count + the
/// module's own `///` doc-comment, when present).
///
/// The doc-comment body runs through the same Kiodoc rewriter
/// `kio doc build` uses: `` [`@signature term`] ``,
/// `` [`@source term`] ``, and `` [`@type term`] `` directives expand
/// against the owning module's items, and `` [`name`] `` references
/// flatten to code spans. Names without a doc-comment show the
/// signature alone.
fn cmd_doc(session: &Session, name: &str, palette: Palette) -> String {
    // Module-name takes precedence: `:doc op/main` on a loaded
    // module renders the module summary. A loaded module's path is
    // unambiguous — it can't simultaneously name an item.
    if let Some(module_path) = resolve_module_arg(session, name) {
        return doc_for_module(session, &module_path, palette);
    }

    let Some(found) = resolve_name(session, name).or_else(|| resolve_operator(session, name))
    else {
        // Builtins imported through `import __intrinsics__;` or
        // `import __comptime__;` have no AST declaration, but they are
        // still real scoped bindings to query.
        if let Some(builtin) = resolve_builtin(session, name) {
            return doc_for_builtin(&builtin, palette);
        }
        // A compound-expression argument suggests the value views.
        return hint_for(unresolved(session, name), "doc", name);
    };

    let mut out = String::new();

    // The doc-comment, rewritten and printed first.
    let selected_item = found.selected_item();
    let entry = found.doc_entry();
    if let Some(doc) = entry.as_ref().and_then(|entry| entry.doc()) {
        let raw = doc.lines.join("\n");
        let rendered = render_doc_prose(&raw, &found.module);
        out.push_str(&render_doc_for_terminal(&rendered, palette));
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    // The signature, always.
    let sig = entry.map_or_else(
        || crate::pretty::pretty_item_signature(&selected_item),
        |entry| entry.signature(),
    );
    out.push_str(&highlight(&sig, palette));
    out
}

/// `:doc <builtin>` body — render a compiler-provided binding in
/// scope.
fn doc_for_builtin(builtin: &ResolvedBuiltin, palette: Palette) -> String {
    let mut out = match &builtin.via_module {
        Some(via_module) => format!(
            "`{}` is a {} in scope via `{}` in {}",
            builtin.doc.name,
            builtin_kind_label(builtin.doc.module),
            builtin.doc.module.import_line(),
            surface_module_path(via_module)
        ),
        None => format!(
            "`{}` is a compiler-provided builtin import",
            builtin.doc.name
        ),
    };
    out.push('\n');
    out.push_str(&highlight(
        &builtin.doc.signature.reference_line(builtin.doc.name),
        palette,
    ));
    out.push('\n');
    out.push_str(builtin.doc.summary);
    out
}

/// `:doc <module>` body — render a loaded module's summary.
///
/// Shape: the module's own `///` doc-comment (when present) first,
/// then a header line naming the module, then a count of declared
/// items and `import`-clause imports. The doc-comment is rewritten
/// through the same Kiodoc directive rewriter `:doc <item>` uses, so
/// references and signature-directives expand against the module's
/// own items.
fn doc_for_module(session: &Session, module_path: &str, palette: Palette) -> String {
    let Some(entry) = session.module(module_path) else {
        // Defensive — `resolve_module_arg` only returns paths whose
        // `session.module` lookup succeeds. If it doesn't, treat the
        // input as an unresolved name so the user sees the same
        // diagnostic shape.
        return unresolved(session, module_path);
    };
    let module = &entry.module;
    let mut out = String::new();

    // The module's own `///` doc-comment, rewritten and printed first.
    if let Some(doc) = &module.doc {
        let raw = doc.lines.join("\n");
        let rendered = render_doc_prose(&raw, module);
        out.push_str(&render_doc_for_terminal(&rendered, palette));
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    // The module header.
    let _ = write!(
        out,
        "{}",
        highlight(
            &format!("module {}", surface_module_path(module_path)),
            palette
        )
    );

    // Counts: declared items + `import`-clause imports. Plurals follow
    // the noun.
    let item_count: usize = module
        .items
        .iter()
        .map(|item| ls_item_lines(item, false).len())
        .sum();
    let use_count = module.imports.len();
    let items_noun = if item_count == 1 { "item" } else { "items" };
    let imports_noun = if use_count == 1 { "import" } else { "imports" };
    let _ = write!(
        out,
        "\n  {item_count} declared {items_noun}, {use_count} {imports_noun}"
    );

    out
}

/// Run a doc-comment body through the Kiodoc directive rewriter.
/// The `SymbolIndex` is empty (the REPL has no doc-site URL space),
/// so `` [`name`] `` references render as plain code spans and the
/// `@signature` / `@source` directives resolve against `module`'s
/// items.
fn render_doc_prose(prose: &str, module: &Module<Surface>) -> String {
    use crate::kiodoc::render::rewrite::rewrite;
    use crate::kiodoc::render::scope::ModuleScope;
    use crate::kiodoc::render::site::SymbolIndex;

    let scope = ModuleScope::from_module(module.clone());
    let index = SymbolIndex::default();
    rewrite(
        prose,
        "",
        ".md",
        &index,
        &scope,
        &std::collections::HashSet::new(),
    )
}

/// Lightly adapt rewritten Markdown for a terminal: highlight the
/// bodies of ` ```kio ` fenced blocks, and drop Markdown link
/// syntax around code spans so `` [`name`](url) `` reads as
/// `` `name` ``. Non-fence prose is emitted as-is.
fn render_doc_for_terminal(markdown: &str, palette: Palette) -> String {
    let mut out = String::new();
    let mut in_kio_fence = false;
    let mut fence_buf = String::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if !in_kio_fence && (trimmed == "```kio" || trimmed.starts_with("```kio ")) {
            in_kio_fence = true;
            fence_buf.clear();
            continue;
        }
        if in_kio_fence && trimmed == "```" {
            in_kio_fence = false;
            // Highlight and indent the captured fence body.
            for body_line in fence_buf.lines() {
                let _ = writeln!(out, "    {}", highlight(body_line, palette));
            }
            continue;
        }
        if in_kio_fence {
            fence_buf.push_str(line);
            fence_buf.push('\n');
            continue;
        }
        // Plain prose line — flatten `[`x`](url)` → `` `x` ``.
        let _ = writeln!(out, "{}", flatten_doc_links(line));
    }
    // An unterminated fence (malformed doc-comment): emit what we have.
    if in_kio_fence {
        for body_line in fence_buf.lines() {
            let _ = writeln!(out, "    {}", highlight(body_line, palette));
        }
    }
    // Trim a trailing newline so the caller controls spacing.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Replace Markdown links whose text is a code span — `` [`x`](url) ``
/// — with the bare code span `` `x` ``. The REPL has no clickable
/// links; the code span is the useful part.
fn flatten_doc_links(line: &str) -> String {
    let mut out = String::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for `[` `` ` `` … `` ` `` `]` `(` … `)`.
        if bytes[i] == b'['
            && let Some((text, consumed)) = parse_code_link(&line[i..])
        {
            out.push('`');
            out.push_str(&text);
            out.push('`');
            i += consumed;
            continue;
        }
        let ch = line[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Parse a `` [`text`](url) `` form at the start of `s`. Returns the
/// code-span text and the total bytes consumed.
fn parse_code_link(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.len() < 5 || b[0] != b'[' || b[1] != b'`' {
        return None;
    }
    let mut j = 2;
    while j < b.len() && b[j] != b'`' && b[j] != b'\n' {
        j += 1;
    }
    if j >= b.len() || b[j] != b'`' {
        return None;
    }
    let text = s[2..j].to_owned();
    // Expect `]` `(` … `)`.
    if j + 2 >= b.len() || b[j + 1] != b']' || b[j + 2] != b'(' {
        return None;
    }
    let mut k = j + 3;
    while k < b.len() && b[k] != b')' && b[k] != b'\n' {
        k += 1;
    }
    if k >= b.len() || b[k] != b')' {
        return None;
    }
    Some((text, k + 1))
}

// ── :source ─────────────────────────────────────────────────────────────────

/// `:source <name>` — print the canonical (`kio fmt`) form of an
/// item, prefixed by any comment block immediately above the
/// declaration.
///
/// The item body itself is the pretty-printed canonical form (the
/// same `kio fmt` output Kiodoc's `` [`@source term`] `` directive
/// embeds). The prefix is whatever contiguous run of `//` and `///`
/// comment lines sits directly above the declaration on disk —
/// sliced from the source file, not pretty-printed, so it survives
/// formatter rewrites the AST throws away. The run stops at the
/// first blank line above the comment block (and at the start of
/// the file).
///
/// Block comments are not part of Kio's surface syntax (see
/// `specs/language.md` § Comments) — the walker is line-comment
/// only by design.
fn cmd_source(session: &Session, name: &str, palette: Palette) -> String {
    // `:source` is one of the named-item query trio (`:signature` /
    // `:source` / `:t`, specs/cli.md § `kio repl`), so it resolves an
    // operator token to its `op` declaration the same way `:signature`
    // does — `resolve_name` keys on declared item names and misses an
    // operator binding, which has no declared *name*.
    let Some(found) = resolve_name(session, name).or_else(|| resolve_operator(session, name))
    else {
        // A compound-expression argument suggests the value views.
        return hint_for(unresolved(session, name), "source", name);
    };
    let selected_item = found.selected_item();
    let body = found.doc_entry().map_or_else(
        || crate::pretty::pretty_item_source(&selected_item),
        |entry| entry.source(),
    );
    // Pull the source text for the file and slice out the run of
    // comment lines immediately preceding the declaration. The
    // session caches each loaded module's source in
    // `analysis.sources`; a cache miss falls back to body-only.
    let prefix = session
        .source_of(&found.file_path)
        .map(|src| preceding_comment_block(src, selected_item.span().start as usize))
        .unwrap_or_default();
    let combined = if prefix.is_empty() {
        body
    } else {
        format!("{prefix}\n{body}")
    };
    highlight(&combined, palette)
}

/// Slice the contiguous run of `//` and `///` comment lines
/// immediately above the byte offset `item_start` in `source`,
/// stopping at the first blank line above the comment block. Returns
/// the empty string when no comments precede the item (or when the
/// comment block is separated from the item by a blank line).
///
/// The implementation walks the bytes preceding `item_start` line by
/// line backward; each line is either a comment (kept), a blank line
/// (terminates the comment block), or any other code (terminates).
/// Mixed `//` + `///` runs are kept together.
fn preceding_comment_block(source: &str, item_start: usize) -> String {
    let item_start = item_start.min(source.len());
    // Walk back to the previous newline so we start from the start
    // of the line above the item's first non-trivia token.
    let mut cursor = source[..item_start].rfind('\n').unwrap_or(0);
    if cursor == 0 && item_start == 0 {
        return String::new();
    }
    // `lines` accumulates the comment lines we want to keep, in
    // **reverse** order — the topmost line ends up last; we reverse
    // at the end. Each entry is the line text *without* its trailing
    // `\n` so the join is consistent.
    let mut lines: Vec<&str> = Vec::new();
    loop {
        // `cursor` points at a `\n` (or 0 if at file start).
        let line_end = cursor;
        let line_start = source[..line_end].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line = &source[line_start..line_end];
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            // A blank line terminates the comment block — comments
            // separated by a blank line are NOT included.
            break;
        }
        if trimmed.starts_with("//") {
            lines.push(line);
        } else {
            // A non-comment, non-blank line terminates the block (we
            // hit code, the previous item, the module header, …).
            break;
        }
        // Move to the previous line. `line_start == 0` means this was
        // the file's first line — done.
        if line_start == 0 {
            break;
        }
        cursor = line_start - 1;
    }
    if lines.is_empty() {
        return String::new();
    }
    lines.reverse();
    lines.join("\n")
}

// ── :ls ─────────────────────────────────────────────────────────────────────

/// `:ls [-v] [<module-path>]` — list a loaded module's top-level items.
///
/// With an explicit `module-path` argument, lists that module — the
/// argument is a loaded module's path or, when a current
/// module is set, a module alias the current module binds via
/// `import X/foo as f;`. With no argument, lists the current module
/// (the one `:mods` marks with `*`); with no current module set,
/// prints the "no current module" diagnostic.
///
/// `verbose` (`-v`) requests full signatures for declaration kinds that
/// have them; the terse default appends a dim trailer advertising `-v`
/// when it elided a signature.
fn cmd_ls(session: &Session, arg: Option<&str>, verbose: bool, palette: Palette) -> String {
    let arg = match arg {
        Some(a) => a,
        None => match session.current() {
            Some(c) => c,
            None => {
                return "no current module — `:load <module-path>` loads one".to_owned();
            }
        },
    };
    let Some(module_path) = resolve_module_arg(session, arg) else {
        return hint_for(
            format!(
                "`{arg}` is not a loaded module — `:mods` lists loaded modules, \
                 `:load {arg}` loads one"
            ),
            "ls",
            arg,
        );
    };
    let surface = surface_module_path(&module_path);
    let Some(entry) = session.module(&module_path) else {
        return format!("module `{surface}` is not loaded — `:load {surface}` first");
    };
    if entry.module.items.is_empty() {
        return format!("module `{surface}` declares no items");
    }
    let mut out = format!("{}:", highlight(&surface, palette));
    for item in &entry.module.items {
        for line in ls_item_lines(item, verbose) {
            let _ = write!(out, "\n  {}", highlight(&line, palette));
        }
    }
    if !verbose && terse_elides_a_signature(&entry.module.items) {
        let _ = write!(out, "\n{}", verbose_footer(palette));
    }
    out
}

/// `:ls` / `:scope` lines for an item.
///
/// In **terse** mode (the default) a value- or type-level item renders
/// as its keyword + name (`fn view`, `newtype T`) — enough to see what
/// is declared without the full signature. In **verbose** mode
/// (`:ls -v`) the same arms route through
/// [`crate::pretty::pretty_item_signature`], so `fn view` becomes
/// `fn view[S][A](…) -> A`. Fixed and variadic operators follow the same split:
/// terse mode shows their canonical declaration name, while verbose mode
/// also shows the callable clauses.
///
/// The anonymous `labels { ... };` form is flag-independent and expands
/// to one `label <name>` line per entry: each entry is the queryable
/// declaration, whereas the anonymous container has no name of its own.
fn ls_item_lines(item: &Item<Surface>, verbose: bool) -> Vec<String> {
    // The anonymous-labels expansion is flag-independent; handle it first.
    // An `op` follows the same terse/verbose split as the rest: terse
    // shows the operator's complete grammar (`op _ + _`), verbose the full
    // declaration pattern (`op _ + _ { impl add; };`).
    match item {
        Item::Op(o, _) => {
            if verbose {
                return vec![crate::pretty::pretty_item_signature(&Item::Op(
                    o.clone(),
                    (),
                ))];
            }
            let vis = crate::pretty::pretty_visibility(&o.vis);
            return vec![format!("{vis}{}", crate::pass::parser::op_name(&o.body))];
        }
        Item::VariadicOperator(f, _) => {
            if verbose {
                return vec![crate::pretty::pretty_item_signature(
                    &Item::VariadicOperator(f.clone(), ()),
                )];
            }
            let vis = crate::pretty::pretty_visibility(&f.vis);
            return vec![format!("{vis}{}", crate::pass::parser::variadic_name(f))];
        }
        Item::RecGroup(g, _) => {
            return g
                .members
                .iter()
                .map(|f| {
                    if verbose {
                        crate::pretty::pretty_item_signature(&Item::FnDef(f.clone()))
                    } else {
                        let vis = if f.vis.is_pub() { "pub fn" } else { "fn" };
                        format!("{vis} {}", f.name)
                    }
                })
                .collect();
        }
        Item::TypeRecGroup(group) => {
            if verbose {
                return vec![crate::pretty::pretty_item_signature(item)];
            }
            return group
                .members
                .iter()
                .flat_map(|member| match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => {
                        vec![format!("type {}", alias.name)]
                    }
                    crate::ast::TypeRecMember::Newtype(newtype) => {
                        let vis = if newtype.vis.is_pub() {
                            "pub newtype"
                        } else {
                            "newtype"
                        };
                        vec![format!("{vis} {}", newtype.name)]
                    }
                    crate::ast::TypeRecMember::Labels(labels, _) => match &labels.type_alias_name {
                        Some(name) => vec![format!("labels {name}")],
                        None => anonymous_label_lines(labels),
                    },
                })
                .collect();
        }
        Item::Labels(t, _) if t.type_alias_name.is_none() => {
            return anonymous_label_lines(t);
        }
        _ => {}
    }
    // An `equiv` is named by its body, not a signature, so it stays
    // `equiv <name>` in both modes — there is no header form to expand.
    if verbose && !matches!(item, Item::Equiv(_, _)) {
        return vec![crate::pretty::pretty_item_signature(item)];
    }
    match item {
        Item::FnDef(f) => {
            let vis = if f.vis.is_pub() { "pub fn" } else { "fn" };
            vec![format!("{vis} {}", f.name)]
        }
        Item::RecGroup(_, _) => unreachable!("RecGroup handled before the verbose split"),
        Item::TypeRecGroup(_) => {
            unreachable!("TypeRecGroup handled before the verbose split")
        }
        Item::Newtype(n) => {
            let vis = if n.vis.is_pub() {
                "pub newtype"
            } else {
                "newtype"
            };
            vec![format!("{vis} {}", n.name)]
        }
        Item::TypeAlias(a) => vec![format!("type {}", a.name)],
        Item::LiteralAlias(l, _) => vec![format!("literal {}", l.name)],
        // Named: `labels T = { ... };` — render the alias name.
        Item::Labels(t, _) => vec![format!(
            "labels {}",
            t.type_alias_name.as_deref().unwrap_or("")
        )],
        Item::LabelForward(forward, _) => {
            let vis = if forward.vis.is_pub() {
                "pub type"
            } else {
                "type"
            };
            vec![format!("{vis} {{{}}}", forward.name)]
        }
        Item::Equiv(e, _) => vec![format!("equiv {}", e.name)],
        Item::Elaborator(s, _) => {
            let vis = if s.vis.is_pub() { "pub elab" } else { "elab" };
            vec![format!("{vis} {}", s.name)]
        }
        Item::HostType(h) => vec![format!("host type {}", h.name)],
        Item::HostFn(h) => vec![format!("host fn {}", h.name)],
        // `Op` / `Fold` (both modes) and anonymous `labels` are handled above.
        Item::Op(_, _) => unreachable!("Op handled before the verbose split"),
        Item::VariadicOperator(_, _) => unreachable!("Fold handled before the verbose split"),
    }
}

/// Render the value bindings introduced by an anonymous labels declaration.
/// The same declaration may stand alone or be one atomic member of a type
/// recursive group; both source containers expose the same entry bindings.
fn anonymous_label_lines(labels: &crate::ast::Labels<Surface>) -> Vec<String> {
    // Visibility lives at the Labels level (per-entry visibility is not part
    // of the AST), so the `pub` prefix is shared across every entry.
    let prefix = if labels.vis.is_pub() {
        "pub label"
    } else {
        "label"
    };
    labels
        .entries
        .iter()
        .map(|entry| format!("{prefix} {}", entry.name))
        .collect()
}

/// Whether a **terse** listing of `items` elided any item's full
/// signature — true when some item's terse form drops detail its
/// verbose form would show. Drives the self-advertising `-v` footer:
/// the trailer only earns its place when `-v` would actually add
/// something. Anonymous `labels` and `equiv` render identically in both
/// modes, so they do not count.
fn terse_elides_a_signature(items: &[Item<Surface>]) -> bool {
    items.iter().any(|it| match it {
        // `Op` now follows the terse/verbose split — terse shows the
        // name, `-v` the full declaration pattern — so it elides a
        // signature in terse mode like a `fn` / `newtype` / `type`.
        Item::FnDef(_)
        | Item::RecGroup(_, _)
        | Item::TypeRecGroup(_)
        | Item::Newtype(_)
        | Item::TypeAlias(_)
        | Item::LiteralAlias(_, _)
        | Item::LabelForward(_, _)
        | Item::Elaborator(_, _)
        | Item::Op(_, _)
        | Item::VariadicOperator(_, _)
        | Item::HostType(_)
        | Item::HostFn(_) => true,
        Item::Labels(t, _) => t.type_alias_name.is_some(),
        // An `equiv` has no header form distinct from its body.
        Item::Equiv(_, _) => false,
    })
}

/// The dim self-advertising trailer pointing at `:ls -v`, appended to a
/// terse listing that elided a signature. Suppressed when `-v` is
/// already in effect — there is nothing more to show.
fn verbose_footer(palette: Palette) -> String {
    super::highlight::dim("(… :ls -v for full signatures)", palette)
}

// ── :mods ───────────────────────────────────────────────────────────────────

/// `:mods` — list loaded modules in load order. The current module
/// is marked `*`; an implicit module shows its explicit referent.
fn cmd_mods(session: &Session) -> String {
    if session.is_empty() {
        return "no modules loaded — `:load <module-path>` loads one".to_owned();
    }
    let current = session.current();
    let mut out = String::new();
    for (i, m) in session.modules_in_load_order().iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let marker = if Some(m.path.as_str()) == current {
            "* "
        } else {
            "  "
        };
        let path = surface_module_path(&m.path);
        match &m.kind {
            super::session::LoadKind::Explicit => {
                let _ = write!(out, "{marker}{path}");
            }
            super::session::LoadKind::Implicit { via } => {
                let _ = write!(out, "{marker}{path} (via {})", surface_module_path(via));
            }
        }
    }
    out
}

// ── :packages ─────────────────────────────────────────────────────────────────

/// `:packages [<package-name>]` — a read-only view of the directory
/// tree's `*.pkg.kio` package files.
///
/// Packages depend on modules, not the other way around: a package
/// file is **optional** and never gates the REPL or the module load
/// path. This command exposes them as a parallel surface to `:mods`
/// without touching the loaded-module set — it walks the directory for
/// `*.pkg.kio` files and parses the named one on demand.
///
/// - No argument → list every `*.pkg.kio` file under the directory
///   root: its package name (the `.pkg.kio` stem) and the relative
///   directory it lives in.
/// - A package name → parse that package file and render its contract:
///   the package name and the `bridge { … }` globs selecting the
///   modules whose `pub` items form the host contract surface.
fn cmd_packages(session: &Session, arg: Option<&str>, palette: Palette) -> String {
    let found = discover_package_files(session);
    match arg {
        None => list_package_files(session.package_root(), &found, palette),
        Some(name) => view_package(session, &found, name.trim(), palette),
    }
}

/// One discovered `*.pkg.kio` file: its package name (the stem) and its
/// source path.
struct DiscoveredPackage {
    /// The `.pkg.kio` stem — the name a user passes to `:packages`.
    stem: String,
    /// The path to the `*.pkg.kio` file in the session's source provider.
    path: PathBuf,
}

/// Walk the current package root for `*.pkg.kio` files, sorted by stem.
/// Read-only and independent of the module load path. Complete in-memory
/// sessions use their source overlay as the virtual filesystem; disk-backed
/// sessions walk the real directory tree.
fn discover_package_files(session: &Session) -> Vec<DiscoveredPackage> {
    let mut out: Vec<DiscoveredPackage> = Vec::new();
    let root = session.package_root();
    if session.source_overlay().is_complete_for(root) {
        for path in session.source_overlay().kio_files_under(root) {
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && let Some(stem) = crate::file_kind::package_stem(name)
            {
                out.push(DiscoveredPackage {
                    stem: stem.to_owned(),
                    path,
                });
            }
        }
    } else {
        collect_package_files(root, &mut out);
    }
    out.sort_by(|a, b| a.stem.cmp(&b.stem).then_with(|| a.path.cmp(&b.path)));
    out
}

fn collect_package_files(dir: &Path, out: &mut Vec<DiscoveredPackage>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_package_files(&path, out);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && let Some(stem) = crate::file_kind::package_stem(name)
        {
            out.push(DiscoveredPackage {
                stem: stem.to_owned(),
                path: path.clone(),
            });
        }
    }
}

/// `:packages` with no argument — list the discovered package files.
fn list_package_files(root: &Path, found: &[DiscoveredPackage], palette: Palette) -> String {
    if found.is_empty() {
        return "no `*.pkg.kio` packages in this directory tree".to_owned();
    }
    let mut out = String::new();
    for (i, pkg) in found.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // The directory the package file lives in, relative to the
        // root, for orientation. The root itself shows as `.`.
        let rel_dir = pkg
            .path
            .parent()
            .and_then(|parent| parent.strip_prefix(root).ok())
            .map(|rel| {
                if rel.as_os_str().is_empty() {
                    ".".to_owned()
                } else {
                    rel.display().to_string()
                }
            })
            .unwrap_or_else(|| ".".to_owned());
        let _ = write!(
            out,
            "{}  {}",
            highlight(&pkg.stem, palette),
            dim_path(&rel_dir, palette)
        );
    }
    out
}

/// The relative-directory annotation in the `:packages` listing,
/// de-emphasised so the package name reads as the primary column.
fn dim_path(rel_dir: &str, palette: Palette) -> String {
    super::highlight::dim(&format!("({rel_dir})"), palette)
}

/// `:packages <name>` — parse and render one package file's contract.
fn view_package(
    session: &Session,
    found: &[DiscoveredPackage],
    name: &str,
    palette: Palette,
) -> String {
    let Some(pkg) = found.iter().find(|p| p.stem == name) else {
        let mut msg = format!("no package `{name}` in this directory tree");
        if !found.is_empty() {
            let names: Vec<&str> = found.iter().map(|p| p.stem.as_str()).collect();
            let _ = write!(msg, "\n  packages: {}", names.join(", "));
        }
        return msg;
    };
    let source = match session.source_overlay().read(&pkg.path) {
        Ok(s) => s,
        Err(e) => {
            return format!("cannot read `{}`: {e}", pkg.path.display());
        }
    };
    let package_file = match crate::pass::parser::parse_package_file(&source, Some(&pkg.stem)) {
        Ok(p) => p,
        Err(e) => {
            let (_, msg) = e.diag();
            return format!("package `{name}` failed to parse: {msg}");
        }
    };
    render_package(&package_file, palette)
}

/// Render a parsed package file's contract: the package name and the
/// `bridge { … }` glob list selecting the modules whose `pub` items
/// form the host contract surface. Read-only — it inspects the parsed
/// [`crate::ast::PackageFile`], never the module load path.
fn render_package(package: &crate::ast::PackageFile<Surface>, palette: Palette) -> String {
    use crate::ast::BridgeGlobSegment;

    let mut out = highlight(&format!("package {}", package.name), palette);

    if let Some(bridge) = &package.bridge {
        let _ = write!(out, "\n{}", section_header("bridge", palette));
        for glob in &bridge.globs {
            let text = glob
                .segments
                .iter()
                .map(|s| match s {
                    BridgeGlobSegment::Literal(name) => name.as_str(),
                    BridgeGlobSegment::Star => "*",
                    BridgeGlobSegment::DoubleStar => "**",
                })
                .collect::<Vec<_>>()
                .join("/");
            let _ = write!(out, "\n  {}", highlight(&text, palette));
        }
    }

    out
}

/// The dim section header in a `:packages <name>` view.
fn section_header(label: &str, palette: Palette) -> String {
    super::highlight::dim(&format!("{label}:"), palette)
}

// ── :scope ──────────────────────────────────────────────────────────────────

/// `:scope` — list everything in the current module's scope.
///
/// Six sections, in order, each suppressed when empty:
///
/// 1. **declared items** — `fn` / `newtype` / `type` / `literal` / `labels` / `op` /
///    `equiv` declarations the current module body contains.
/// 2. **imported names** — names brought in via `import` clauses, grouped
///    by source module. Both selective (`import m(a, b);`) and
///    qualified (`import m as alias;`) clauses contribute.
/// 3. **operator bindings** — module-local `op` declarations expressed
///    by their complete tagged grammar (`op _ + _`, `op _ ? _ : __`).
/// 4. **module aliases** — qualified `import` aliases bound by the
///    current module (`import m as alias;`).
/// 5. **intrinsics** — whether `import __intrinsics__;` brought the eight
///    value intrinsics into scope.
/// 6. **comptime** — whether `import __comptime__;` brought the
///    compile-time helper surface into scope.
///
/// With no current module the command prints the same diagnostic the
/// other current-module-bearing commands use. `verbose` (`-v`) requests
/// full signatures where available, the same flag `:ls` honours; the
/// terse default appends the same `-v` trailer when it elided a signature.
fn cmd_scope(session: &Session, verbose: bool, palette: Palette) -> String {
    let Some(current) = session.current() else {
        return "no current module — `:load <module-path>` loads one".to_owned();
    };
    let Some(entry) = session.module(current) else {
        return format!("module `{}` is not loaded", surface_module_path(current));
    };
    let module = &entry.module;
    let mut out = format!(
        "scope of {}:",
        highlight(&surface_module_path(current), palette)
    );

    // (1) Declared items — `fn` / `newtype` / `type` / `literal` / `labels` / `op` /
    // `equiv` declarations the module body contains.
    let mut declared_lines: Vec<String> = Vec::new();
    for item in &module.items {
        declared_lines.extend(ls_item_lines(item, verbose));
    }
    if !declared_lines.is_empty() {
        let _ = write!(out, "\n  declared:");
        for line in &declared_lines {
            let _ = write!(out, "\n    {}", highlight(line, palette));
        }
    }

    // (2) Imported names, grouped by source module. A selective `import`
    // contributes one entry per imported name (operator-pattern
    // imports surface under "operator bindings" below); a qualified
    // `import` contributes the alias (covered under "module aliases").
    let mut selective_by_source: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for u in &module.imports {
        if let ImportKind::Selective { items, from } = &u.kind {
            let from_str = module_path_key(from);
            for it in items {
                if let Some(name) = it.as_name() {
                    selective_by_source
                        .entry(from_str.clone())
                        .or_default()
                        .push(name.to_owned());
                }
            }
        }
    }
    if !selective_by_source.is_empty() {
        let _ = write!(out, "\n  imported:");
        for (from, names) in &selective_by_source {
            let _ = write!(
                out,
                "\n    from {}: {}",
                highlight(&surface_module_path(from), palette),
                names.join(", ")
            );
        }
    }

    // (3) Operator bindings — module-local `op` declarations, indexed
    // by the complete tagged grammar used in imports and named-item queries.
    // This one-line index includes fixed and variadic operators; the
    // `declared:` listing also shows their callable clauses under `-v`.
    let mut op_lines: Vec<String> = Vec::new();
    for item in &module.items {
        match item {
            Item::Op(o, _) => op_lines.push(crate::pass::parser::op_name(&o.body)),
            Item::VariadicOperator(o, _) => op_lines.push(crate::pass::parser::variadic_name(o)),
            _ => {}
        }
    }
    if !op_lines.is_empty() {
        let _ = write!(out, "\n  operators: {}", op_lines.join(", "));
    }

    // (4) Module aliases — `import m as alias;`.
    let mut alias_lines: Vec<String> = Vec::new();
    for u in &module.imports {
        if let ImportKind::Qualified { path, alias, .. } = &u.kind {
            alias_lines.push(format!(
                "{alias} -> {}",
                surface_module_path(&module_path_key(path))
            ));
        }
    }
    if !alias_lines.is_empty() {
        let _ = write!(out, "\n  module aliases:");
        for line in &alias_lines {
            let _ = write!(out, "\n    {}", highlight(line, palette));
        }
    }

    // (5) Intrinsics.
    let intrinsics_in_scope =
        module_imports_builtin(module, crate::builtin_docs::BuiltinModule::Intrinsics);
    let _ = write!(
        out,
        "\n  intrinsics: {}",
        if intrinsics_in_scope {
            "in scope"
        } else {
            "not in scope"
        }
    );
    let comptime_in_scope =
        module_imports_builtin(module, crate::builtin_docs::BuiltinModule::Comptime);
    let _ = write!(
        out,
        "\n  comptime: {}",
        if comptime_in_scope {
            "in scope"
        } else {
            "not in scope"
        }
    );

    // The same self-advertising `-v` trailer `:ls` prints, when the
    // terse declared-items section elided a signature.
    if !verbose && terse_elides_a_signature(&module.items) {
        let _ = write!(out, "\n{}", verbose_footer(palette));
    }

    out
}

// ── :unload ─────────────────────────────────────────────────────────────────

/// `:unload <module-path>` — remove a loaded explicit module.
///
/// Rejected when `module-path` is an implicit module (the user
/// removes its explicit referent instead) or when another loaded
/// explicit module still references it. On success, orphaned
/// implicit deps cascade-remove and the current pointer falls back
/// if it pointed at the removed module.
fn cmd_unload(session: &mut Session, arg: &str) -> String {
    let Some(module_path) = resolve_module_arg(session, arg) else {
        // A compound-expression argument suggests the value views.
        return hint_for(
            format!("`{arg}` is not a loaded module — `:mods` lists loaded modules"),
            "unload",
            arg,
        );
    };
    let surface = surface_module_path(&module_path);
    let Some(entry) = session.module(&module_path) else {
        return format!("module `{surface}` is not loaded");
    };
    if !entry.is_explicit() {
        let via = match &entry.kind {
            super::session::LoadKind::Implicit { via } => surface_module_path(via),
            super::session::LoadKind::Explicit => unreachable!("checked is_explicit above"),
        };
        return format!(
            "`{surface}` is an implicit dependency (pulled in by `{via}`) — \
             unload `{via}` instead"
        );
    }
    let referrers = session.explicit_referrers(&module_path);
    if !referrers.is_empty() {
        let surface_referrers: Vec<String> =
            referrers.iter().map(|r| surface_module_path(r)).collect();
        return format!(
            "cannot unload `{surface}` — still referenced by loaded module(s): {}",
            surface_referrers.join(", ")
        );
    }
    session.unload(&module_path);
    let mut out = format!("unloaded {surface}");
    match session.current() {
        Some(c) => {
            let _ = write!(out, "\n  current module is now {}", surface_module_path(c));
        }
        None => {
            let _ = write!(out, "\n  no current module");
        }
    }
    out
}

// ── :which ──────────────────────────────────────────────────────────────────

/// `:which <name>` — report which loaded module declares `name`, as
/// an FQN. A bare name is searched across every loaded module's
/// declared items, whether `pub`-exported or private.
fn cmd_which(session: &Session, name: &str) -> String {
    // An FQN argument: report it directly if it resolves.
    if name.contains('.')
        && let Some(found) = resolve_name(session, name)
    {
        return format!(
            "{}.{}",
            surface_module_path(&found.module_path),
            found.short_name
        );
    }
    let mut hits: Vec<String> = Vec::new();
    for m in session.modules_in_load_order() {
        for item in &m.module.items {
            if surface_item_declares(item, name) {
                hits.push(format!("{}.{name}", surface_module_path(&m.path)));
            }
        }
    }
    if !hits.is_empty() {
        return match hits.len() {
            1 => hits.into_iter().next().expect("len checked"),
            _ => format!(
                "`{name}` is declared by several modules:\n  {}",
                hits.join("\n  ")
            ),
        };
    }
    // No declared item — try compiler-provided builtin bindings.
    if let Some(builtin) = resolve_builtin(session, name) {
        return which_for_builtin(&builtin);
    }
    hint_for(
        format!("no loaded module declares `{name}` — `:mods` lists loaded modules"),
        "which",
        name,
    )
}

// ── :refs ───────────────────────────────────────────────────────────────────

/// `:refs <name>` — list every place `name` is referenced across
/// loaded modules: call sites, type-position uses, and `import` lines.
///
/// The scan runs the [`crate::tokens`] classifier — the same
/// token walk `:source` highlighting and the LSP semantic-tokens
/// provider use — over each loaded module's source, and reports
/// every identifier / type-name / module-segment token whose text
/// equals `name`. That uniformly covers call sites, type-position
/// uses, and the names inside `import` clauses, keyed on the token's
/// classification so a same-spelled keyword or string is never
/// mistaken for a reference. A braced selector restricts the scan to
/// label tokens. References are reported as
/// `module-path:line:col  in <kind> <name>`, where the trailing
/// label names the enclosing top-level item (or the module, for a
/// reference outside every item — typically an `import`-clause name).
fn cmd_refs(session: &Session, name: &str) -> String {
    use crate::tokens::{TokenKind, dump};

    // `name` may be an FQN; the reference search keys on the short
    // name (the spelling that appears at use sites).
    let short = name.rsplit('.').next().unwrap_or(name);
    let label_name = short.strip_prefix('{').and_then(|s| s.strip_suffix('}'));
    let mut hits: Vec<RefHit> = Vec::new();
    for m in session.modules_in_load_order() {
        let Some(source) = session.source_of(&m.file_path) else {
            continue;
        };
        let Ok(tokens) = dump(source) else {
            continue;
        };
        for tok in tokens {
            // Only name-bearing token kinds count as references.
            let label_token = matches!(
                tok.kind,
                TokenKind::EntityNameLabel
                    | TokenKind::EntityNameLabelReference
                    | TokenKind::EntityNameQualifiedLabelReference
            );
            let counts = if label_name.is_some() {
                label_token
            } else {
                matches!(
                    tok.kind,
                    TokenKind::Identifier
                        | TokenKind::EntityNameFunction
                        | TokenKind::EntityNameFunctionReference
                        | TokenKind::EntityNameType
                        | TokenKind::EntityNameModule
                        | TokenKind::EntityNameLabel
                        | TokenKind::EntityNameLabelReference
                        | TokenKind::EntityNameQualifiedLabelReference
                        | TokenKind::VariableParameter
                )
            };
            if !counts {
                continue;
            }
            let start = tok.span.start as usize;
            let end = (tok.span.end as usize).min(source.len());
            if start > end {
                continue;
            }
            if &source[start..end] != label_name.unwrap_or(short) {
                continue;
            }
            let (line, col) = line_col(source, tok.span.start);
            let surface = surface_module_path(&m.path);
            hits.push(RefHit {
                context: enclosing_item_label(&m.module, &surface, tok.span.start),
                module: surface,
                line,
                col,
            });
        }
    }
    if hits.is_empty() {
        // A compound-expression argument suggests the value views.
        return hint_for(
            format!("no references to `{short}` in loaded modules"),
            "refs",
            name,
        );
    }
    hits.sort_by(|a, b| {
        a.module
            .cmp(&b.module)
            .then(a.line.cmp(&b.line))
            .then(a.col.cmp(&b.col))
    });
    let mut out = format!("references to `{short}`:");
    for h in hits {
        let _ = write!(out, "\n  {}:{}:{}  {}", h.module, h.line, h.col, h.context);
    }
    out
}

/// One `:refs` hit: where the name appears and the label of its
/// enclosing top-level item (or `in module <path>` when no item
/// contains it).
struct RefHit {
    module: String,
    line: usize,
    col: usize,
    context: String,
}

/// Label for `:refs`: name the top-level item whose declaration span
/// contains `offset`, formatted as `in <kind> <name>` — e.g.
/// `in fn parse_op`, `in newtype Token`, `in op _ + _`, `in equiv id_eq`,
/// `in type Foo`, `in labels T`. A reference outside every item — an
/// `import` clause's name, the `module ...;` header — gets
/// `in module <module-path>`. Each item's `meta.span` covers the
/// whole declaration (header + body), so the first item whose span
/// contains `offset` is the enclosing one.
fn enclosing_item_label(module: &Module<Surface>, module_path: &str, offset: u32) -> String {
    for item in &module.items {
        let span = item.span();
        if span.start <= offset && offset < span.end {
            if let Item::RecGroup(group, _) = item {
                return group
                    .members
                    .iter()
                    .find(|member| {
                        member.meta.span.start <= offset && offset < member.meta.span.end
                    })
                    .map(|member| format!("in fn {}", member.name))
                    .unwrap_or_else(|| format!("in module {module_path}"));
            }
            if let Item::TypeRecGroup(group) = item {
                return group
                    .members
                    .iter()
                    .find(|member| {
                        let span = member.meta().span;
                        span.start <= offset && offset < span.end
                    })
                    .map(|member| match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => {
                            format!("in type {}", alias.name)
                        }
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            format!("in newtype {}", newtype.name)
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            labels.type_alias_name.as_ref().map_or_else(
                                || format!("in module {module_path}"),
                                |name| format!("in labels {name}"),
                            )
                        }
                    })
                    .unwrap_or_else(|| format!("in module {module_path}"));
            }
            return item_label(item, module_path);
        }
    }
    format!("in module {module_path}")
}

/// Render one item's `:refs` label. Fixed and variadic operators use
/// their complete tagged grammar. Items without a declared name,
/// such as an anonymous `labels { ... };` block, fall back to the
/// module label.
fn item_label(item: &Item<Surface>, module_path: &str) -> String {
    match item {
        Item::FnDef(d) => format!("in fn {}", d.name),
        Item::RecGroup(_, _) => unreachable!("recursive members are selected by their own span"),
        Item::TypeRecGroup(_) => {
            unreachable!("recursive type members are selected by their own span")
        }
        Item::Newtype(n) => format!("in newtype {}", n.name),
        Item::TypeAlias(a) => format!("in type {}", a.name),
        Item::LiteralAlias(l, _) => format!("in literal {}", l.name),
        Item::Elaborator(s, _) => format!("in elaborator {}", s.name),
        Item::Equiv(e, _) => format!("in equiv {}", e.name),
        Item::Labels(t, _) => match &t.type_alias_name {
            Some(n) => format!("in labels {n}"),
            // An anonymous `labels { ... };` block has no name. Falling
            // back to the module label keeps the label informative.
            None => format!("in module {module_path}"),
        },
        Item::LabelForward(forward, _) => format!("in type {{{}}}", forward.name),
        Item::HostType(h) => format!("in host type {}", h.name),
        Item::HostFn(h) => format!("in host fn {}", h.name),
        // Name the op by its canonical name — covers variadics too.
        Item::Op(o, _) => format!("in {}", crate::pass::parser::op_name(&o.body)),
        Item::VariadicOperator(f, _) => format!("in {}", crate::pass::parser::variadic_name(f)),
    }
}

// ── name resolution ─────────────────────────────────────────────────────────

/// A resolved name: the owning module, the item, and display names.
struct Resolved {
    /// The owning module's slash key (rendered to the
    /// surface `/` spelling at display sites).
    module_path: String,
    /// The item's short (un-qualified) name.
    short_name: String,
    /// The name as the user should see it echoed — the FQN when the
    /// resolution was qualified, the short name otherwise.
    display_name: String,
    /// The owning module's Surface AST (for `:doc` directive scope).
    module: Module<Surface>,
    /// The resolved top-level item.
    item: Item<Surface>,
    /// Absolute path to the owning module's `.kio` file. Used by
    /// `:source` to slice preceding comments out of the on-disk text.
    file_path: std::path::PathBuf,
}

impl Resolved {
    fn doc_entry(&self) -> Option<crate::doc_entry::DocEntry<'_>> {
        crate::doc_entry::doc_entry_for_name(&self.item, &self.short_name)
    }

    fn selected_item(&self) -> Item<Surface> {
        match &self.item {
            Item::RecGroup(group, _) => Item::FnDef(
                group
                    .members
                    .iter()
                    .find(|member| member.name == self.short_name)
                    .cloned()
                    .expect("resolved recursion group contains selected member"),
            ),
            Item::TypeRecGroup(group) => match group
                .members
                .iter()
                .find(|member| match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => alias.name == self.short_name,
                    crate::ast::TypeRecMember::Newtype(newtype) => newtype.name == self.short_name,
                    crate::ast::TypeRecMember::Labels(labels, _) => {
                        labels.type_alias_name.as_deref() == Some(self.short_name.as_str())
                            || labels.entries.iter().any(|entry| {
                                !entry.is_reuse_marker()
                                    && crate::ast::mint_label_newtype_name(&entry.name)
                                        == self.short_name
                            })
                    }
                })
                .cloned()
                .expect("resolved type-recursive group contains selected member")
            {
                crate::ast::TypeRecMember::TypeAlias(alias) => Item::TypeAlias(alias),
                crate::ast::TypeRecMember::Newtype(newtype) => Item::Newtype(newtype),
                crate::ast::TypeRecMember::Labels(labels, ext) => Item::Labels(labels, ext),
            },
            item => item.clone(),
        }
    }
}

/// A resolved compiler-provided builtin, optionally with the loaded
/// module that brought it into scope.
struct ResolvedBuiltin {
    doc: crate::builtin_docs::BuiltinDoc,
    via_module: Option<String>,
}

/// Resolve `name` to a top-level item.
///
/// - A fully-qualified `X/a/b.name` is split into a module path and a
///   final item segment; the module must be loaded and must declare
///   the item.
/// - A bare `name` resolves through the current scope: an
///   item the current module declares, or a name it selectively
///   imports (resolved to the source module). With no current
///   module, a bare name does not resolve.
fn resolve_name(session: &Session, name: &str) -> Option<Resolved> {
    if let Some((module_path, short)) = split_fqn(session, name) {
        let entry = session.module(&module_path)?;
        let item = entry
            .module
            .items
            .iter()
            .find(|it| surface_item_declares(it, short.as_str()))?;
        return Some(Resolved {
            display_name: format!("{}.{short}", surface_module_path(&module_path)),
            module_path: module_path.clone(),
            short_name: short.clone(),
            module: entry.module.clone(),
            item: item.clone(),
            file_path: entry.file_path.clone(),
        });
    }

    // Bare name: resolve through the current scope.
    let current = session.current()?;
    let cur_entry = session.module(current)?;

    // (1) An item the current module declares.
    if let Some(item) = cur_entry
        .module
        .items
        .iter()
        .find(|it| surface_item_declares(it, name))
    {
        return Some(Resolved {
            module_path: current.to_owned(),
            short_name: name.to_owned(),
            display_name: name.to_owned(),
            module: cur_entry.module.clone(),
            item: item.clone(),
            file_path: cur_entry.file_path.clone(),
        });
    }

    // (2) A name the current module selectively imports — resolve to
    // the source module if it is loaded.
    for u in &cur_entry.module.imports {
        if let ImportKind::Selective { items, from } = &u.kind {
            let label_name = name.strip_prefix('{').and_then(|s| s.strip_suffix('}'));
            let imported = items.iter().any(|it| {
                it.as_name() == Some(name)
                    || label_name.is_some_and(|label| {
                        it.as_label().is_some_and(|(imported, _)| imported == label)
                    })
            });
            if imported {
                let from_path = module_path_key(from);
                if let Some(src_entry) = session.module(&from_path)
                    && let Some(item) = src_entry
                        .module
                        .items
                        .iter()
                        .find(|it| surface_item_declares(it, name))
                {
                    return Some(Resolved {
                        display_name: format!("{}.{name}", surface_module_path(&from_path)),
                        module_path: from_path.clone(),
                        short_name: name.to_owned(),
                        module: src_entry.module.clone(),
                        item: item.clone(),
                        file_path: src_entry.file_path.clone(),
                    });
                }
            }
        }
    }

    None
}

/// Resolve `name` as a compiler-provided builtin in scope.
fn resolve_builtin(session: &Session, name: &str) -> Option<ResolvedBuiltin> {
    let doc = crate::builtin_docs::doc_for_label(name)?;
    if matches!(
        doc.signature,
        crate::builtin_docs::BuiltinSignature::ModuleImport
    ) {
        return Some(ResolvedBuiltin {
            doc,
            via_module: None,
        });
    }
    let via_module = builtin_scope_module(session, doc.module)?;
    Some(ResolvedBuiltin {
        doc,
        via_module: Some(via_module),
    })
}

fn builtin_scope_module(
    session: &Session,
    builtin_module: crate::builtin_docs::BuiltinModule,
) -> Option<String> {
    if let Some(cur) = session.current()
        && let Some(entry) = session.module(cur)
        && module_imports_builtin(&entry.module, builtin_module)
    {
        return Some(cur.to_owned());
    }
    for m in session.modules_in_load_order() {
        if module_imports_builtin(&m.module, builtin_module) {
            return Some(m.path.clone());
        }
    }
    None
}

fn module_imports_builtin(
    module: &Module<Surface>,
    builtin_module: crate::builtin_docs::BuiltinModule,
) -> bool {
    module.imports.iter().any(|u| match builtin_module {
        crate::builtin_docs::BuiltinModule::Intrinsics => matches!(u.kind, ImportKind::Intrinsics),
        crate::builtin_docs::BuiltinModule::Comptime => matches!(u.kind, ImportKind::Comptime),
    })
}

fn builtin_kind_label(module: crate::builtin_docs::BuiltinModule) -> &'static str {
    match module {
        crate::builtin_docs::BuiltinModule::Intrinsics => "intrinsic",
        crate::builtin_docs::BuiltinModule::Comptime => "compile-time helper",
    }
}

fn which_for_builtin(builtin: &ResolvedBuiltin) -> String {
    match &builtin.via_module {
        Some(via_module) => format!(
            "`{}` is a {} in scope via `{}` in {}",
            builtin.doc.name,
            builtin_kind_label(builtin.doc.module),
            builtin.doc.module.import_line(),
            surface_module_path(via_module)
        ),
        None => format!(
            "`{}` is a compiler-provided builtin import",
            builtin.doc.name
        ),
    }
}

/// Resolve a complete tagged grammar in the current module's explicit scope.
fn resolve_operator(session: &Session, spelling: &str) -> Option<Resolved> {
    let grammar = crate::pass::parser::parse_operator_grammar(spelling).ok()?;
    let target = grammar.render();
    let current = session.current()?;
    let current_entry = session.module(current)?;
    let find = |entry: &crate::repl_core::session::LoadedModule| {
        entry
            .module
            .items
            .iter()
            .find(|item| {
                matches!(item, Item::Op(_, _) | Item::VariadicOperator(_, _))
                    && crate::doc_entry::doc_entry_for_name(item, &target).is_some()
            })
            .cloned()
    };
    if let Some(item) = find(current_entry) {
        return Some(resolved_operator(
            current,
            &current_entry.module,
            &item,
            current_entry,
            &target,
        ));
    }
    for import in &current_entry.module.imports {
        let ImportKind::Selective { items, from } = &import.kind else {
            continue;
        };
        if !items.iter().any(|item| matches!(item,
            crate::ast::ImportItem::OperatorPattern { grammar: selected, .. } if selected == &grammar
        )) { continue; }
        let provider_path = module_path_key(from);
        let provider = session.module(&provider_path)?;
        if let Some(item) = find(provider) {
            return Some(resolved_operator(
                &provider_path,
                &provider.module,
                &item,
                provider,
                &target,
            ));
        }
    }
    None
}

fn resolved_operator(
    module_path: &str,
    module: &Module<Surface>,
    item: &Item<Surface>,
    entry: &crate::repl_core::session::LoadedModule,
    display: &str,
) -> Resolved {
    Resolved {
        module_path: module_path.to_owned(),
        short_name: display.to_owned(),
        display_name: display.to_owned(),
        module: module.clone(),
        item: item.clone(),
        file_path: entry.file_path.clone(),
    }
}

/// Split a name into `(module_path, short)` when it is a
/// fully-qualified path naming a loaded module. Returns `None` for a
/// bare name or a dotted member path whose prefix is not a loaded module.
fn split_fqn(session: &Session, name: &str) -> Option<(String, String)> {
    let idx = name.rfind('.')?;
    let module_path = module_arg_to_key(&name[..idx]);
    let short = &name[idx + 1..];
    if short.is_empty() {
        return None;
    }
    if session.module(&module_path).is_some() {
        Some((module_path, short.to_owned()))
    } else {
        None
    }
}

/// Resolve a `:ls` / `:unload` module argument to a loaded module's
/// slash key. The argument is the module path the user types, or a
/// module alias the current module binds via `import X/Y as alias;`.
fn resolve_module_arg(session: &Session, arg: &str) -> Option<String> {
    let key = module_arg_to_key(arg);
    if session.module(&key).is_some() {
        return Some(key);
    }
    // A module alias bound by the current module.
    let current = session.current()?;
    let cur_entry = session.module(current)?;
    for u in &cur_entry.module.imports {
        if let ImportKind::Qualified { path, alias, .. } = &u.kind
            && alias == arg
        {
            let target = module_path_key(path);
            if session.module(&target).is_some() {
                return Some(target);
            }
        }
    }
    None
}

// ── shared helpers ───────────────────────────────────────────────────────────

/// The System-F type of a `fn` declaration.
fn fn_def_type(f: &FnDef<Surface>) -> crate::ast::Type<Surface> {
    f.sig.signature_ty(f.ret.clone(), f.meta.span)
}

/// The declared (short) name of a Surface item, or `None` for an
/// anonymous one.
fn surface_item_name(item: &Item<Surface>) -> Option<&str> {
    match item {
        Item::FnDef(d) => Some(&d.name),
        Item::RecGroup(g, _) => rec_group_module_members(g).next().map(|f| f.name.as_str()),
        Item::TypeRecGroup(g) => g.members.first().and_then(|member| match member {
            crate::ast::TypeRecMember::TypeAlias(alias) => Some(alias.name.as_str()),
            crate::ast::TypeRecMember::Newtype(newtype) => Some(newtype.name.as_str()),
            crate::ast::TypeRecMember::Labels(labels, _) => labels.type_alias_name.as_deref(),
        }),
        Item::TypeAlias(a) => Some(&a.name),
        Item::LiteralAlias(l, _) => Some(&l.name),
        Item::Elaborator(s, _) => Some(&s.name),
        Item::Newtype(n) => Some(&n.name),
        Item::Labels(t, _) => t.type_alias_name.as_deref(),
        Item::LabelForward(_, _) => None,
        Item::Equiv(e, _) => Some(&e.name),
        Item::HostType(h) => Some(&h.name),
        Item::HostFn(h) => Some(&h.name),
        Item::Op(o, _) => match &o.body {
            crate::ast::OpBody::Normal { function, .. } => {
                function.last().map(crate::ast::PathSegment::as_str)
            }
        },
        Item::VariadicOperator(_, _) => None,
    }
}

fn surface_item_declares(item: &Item<Surface>, name: &str) -> bool {
    match item {
        Item::RecGroup(g, _) => rec_group_module_members(g).any(|f| f.name == name),
        Item::TypeRecGroup(_) => crate::doc_entry::doc_entry_for_name(item, name).is_some(),
        Item::Labels(_, _) | Item::LabelForward(_, _) => {
            crate::doc_entry::doc_entry_for_name(item, name).is_some()
        }
        _ => surface_item_name(item) == Some(name),
    }
}

fn rec_group_module_members(
    g: &crate::ast::RecGroup<Surface>,
) -> impl Iterator<Item = &FnDef<Surface>> {
    g.members.iter()
}

fn module_path_key(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn surface_module_path(key: &str) -> String {
    key.to_owned()
}

fn module_arg_to_key(arg: &str) -> String {
    arg.to_owned()
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

/// Format the "name did not resolve" diagnostic, hinting whether the
/// failure is a no-current-module situation.
fn unresolved(session: &Session, name: &str) -> String {
    if session.current().is_none() {
        format!(
            "`{name}` did not resolve — no current module is set, so only \
             fully-qualified names (`X/a/b.name`) resolve. `:load <module-path>` \
             loads a module."
        )
    } else {
        format!(
            "`{name}` did not resolve in the current scope — `:scope` lists \
             everything in scope"
        )
    }
}

/// Render an [`crate::cmd::check::AnalysisFailure`] as a one-line
/// diagnostic for the `:load` abort message.
fn render_failure(failure: &crate::cmd::check::AnalysisFailure) -> String {
    let located = failure.primary_error();
    let (span, message) = located.error.diag();
    // Find the offending file's source for line/col.
    if let Some(src) = failure.sources.get(&located.file_path) {
        let (line, col) = line_col(src, span.start);
        format!(
            "{}:{}:{}: {}",
            located.file_path.display(),
            line,
            col,
            message
        )
    } else {
        format!("{}: {}", located.file_path.display(), message)
    }
}

/// Every spelling for a canonical command — the canonical name plus each
/// synonym folding onto it (`:help` carries two, `:h` and `:?`) — ordered
/// full-name-first for the `:help` block. An abbreviation is by
/// definition shorter than the command it abbreviates, so the longest
/// spelling leads and the shorter forms follow, regardless of which
/// spelling [`SYNONYMS`] happens to make canonical. A stable sort keeps
/// declaration order among ties (`:h` before `:?`). Deriving the row from
/// the synonym table means a command gaining an alias shows it here
/// automatically.
fn display_spellings(canonical: &'static str) -> Vec<&'static str> {
    let mut spellings: Vec<&'static str> = vec![canonical];
    spellings.extend(
        SYNONYMS
            .iter()
            .filter(|(_, long)| *long == canonical)
            .map(|(short, _)| *short),
    );
    spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));
    spellings
}

/// The `:help` text — commands grouped, synonyms shown inline. The
/// command rows are rendered from [`HELP_SECTIONS`], the single source
/// of truth shared with the popup-menu description column, so the help
/// block and the menu descriptions can never drift.
fn help_text() -> String {
    /// The width the primary-spelling column is padded to (`:references`,
    /// the widest full name at 11 chars including the `:`), so the
    /// abbreviation column lines up across rows.
    const PRIMARY_COL: usize = 12;
    /// The width the abbreviation-or-blank column is padded to (`:h :?`,
    /// help's two short forms, is the widest at 5 chars), so the argument
    /// / summary column lines up.
    const ALIAS_COL: usize = 6;
    /// The width the argument-hint column is padded to
    /// (`<name-or-expression>` is the widest hint), so every summary starts at
    /// the same column.
    const ARG_COL: usize = 21;

    let mut out = String::from("kio repl — the module inspector\n");
    for (section, rows) in HELP_SECTIONS {
        out.push('\n');
        out.push_str(section);
        out.push_str(":\n");
        for row in *rows {
            let spellings = display_spellings(row.canonical);
            let primary = format!(":{}", spellings[0]);
            let aliases = spellings[1..]
                .iter()
                .map(|s| format!(":{s}"))
                .collect::<Vec<_>>()
                .join(" ");
            // Primary spelling padded, abbreviation(s) (or blank) padded,
            // arg-hint padded, then the summary — one aligned layout.
            let _ = writeln!(
                out,
                "  {primary:PRIMARY_COL$} {aliases:ALIAS_COL$}{arg:ARG_COL$} {summary}",
                arg = row.arg,
                summary = row.summary,
            );
        }
    }
    out.push_str(
        "\n\
Bare input (no `:`) is classified by what it is. A bare name (foo,
X/a/b.item, an operator) routes to `:doc` — its doc-comment and
signature. Anything that parses as a Kio expression (42, foo(),
1 + 2) prints its type and its residual normal form (the `:type` and
`:normalize` views together). Input that is neither prints what each
command would have wanted. Use `:normalize foo` to force-reduce a
name instead of viewing its doc.

A name is resolved through the current scope, or by fully-qualified
path (X/a/b.name). `:scope` lists everything in scope.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parsing ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_non_colon_line_is_a_bare_input_query() {
        // A non-`:` line is a bare-input query, not an error —
        // the dispatcher classifies it at run time.
        assert_eq!(
            parse_command("hello"),
            Ok(Command::Expr("hello".to_owned()))
        );
        assert_eq!(
            parse_command("1 + 2"),
            Ok(Command::Expr("1 + 2".to_owned()))
        );
    }

    #[test]
    fn parse_normalize_command() {
        assert_eq!(
            parse_command(":normalize 1 + 2"),
            Ok(Command::Normalize("1 + 2".to_owned()))
        );
        // The `:norm` short form folds onto `:normalize`.
        assert_eq!(
            parse_command(":norm 1 + 2"),
            Ok(Command::Normalize("1 + 2".to_owned()))
        );
        // `:normalize` needs an argument.
        assert!(parse_command(":normalize").is_err());
    }

    #[test]
    fn parse_pure_command() {
        assert_eq!(
            parse_command(":pure identity(())"),
            Ok(Command::Pure("identity(())".to_owned()))
        );
        let error = parse_command(":pure").expect_err("`:pure` needs an argument");
        assert!(error.0.contains("Usage: :pure"), "got: {}", error.0);
    }

    #[test]
    fn parse_signature_command() {
        assert_eq!(
            parse_command(":signature foo"),
            Ok(Command::Signature("foo".to_owned()))
        );
        // The `:sig` short form folds onto `:signature`.
        assert_eq!(
            parse_command(":sig foo"),
            Ok(Command::Signature("foo".to_owned()))
        );
        // `:signature` needs an argument.
        assert!(parse_command(":signature").is_err());
    }

    #[test]
    fn parse_source_short_synonym() {
        // `:src` folds onto `:source`.
        assert_eq!(
            parse_command(":src foo"),
            Ok(Command::Source("foo".to_owned()))
        );
        assert_eq!(
            parse_command(":source foo"),
            Ok(Command::Source("foo".to_owned()))
        );
    }

    #[test]
    fn parse_load_with_path() {
        assert_eq!(
            parse_command(":load pkg/a"),
            Ok(Command::Load("pkg/a".to_owned()))
        );
    }

    #[test]
    fn parse_load_short_synonym() {
        assert_eq!(
            parse_command(":l pkg/a"),
            Ok(Command::Load("pkg/a".to_owned()))
        );
    }

    #[test]
    fn packages_command_reads_complete_overlay_package_files() {
        let root = PathBuf::from("/kio-wasm/demo");
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            root.join("demo.pkg.kio"),
            "package demo;\n\nbridge { main; }\n".to_owned(),
        );
        files.insert(root.join("main.kio"), "module main;\n".to_owned());
        let mut session = Session::new_in_memory(root, files);

        let list = Command::Packages(None).run(&mut session, Palette::plain());
        assert!(list.output.contains("demo"), "got:\n{}", list.output);

        let contract =
            Command::Packages(Some("demo".to_owned())).run(&mut session, Palette::plain());
        assert!(
            contract.output.contains("package demo"),
            "got:\n{}",
            contract.output
        );
        assert!(
            contract.output.contains("main"),
            "got:\n{}",
            contract.output
        );
    }

    fn imported_operator_session() -> Session {
        let root = PathBuf::from("/kio-repl-tests/app");
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            root.join("app.pkg.kio"),
            "package app;\n\nbridge {\n  app/**;\n}\n".to_owned(),
        );
        files.insert(
            root.join("app/syntax.kio"),
            "module app/syntax;\npub fn choose(left: ., right: .) -> . { left }\npub op ? _ : _ { impl choose }\n"
                .to_owned(),
        );
        files.insert(
            root.join("app/main.kio"),
            "module app/main;\nimport app/syntax(op ? _ : _);\nfn invoke() -> . { ? () : () }\n/// Runs [`@source invoke`].\npub fn run() -> . { invoke() }\n"
                .to_owned(),
        );
        Session::new_in_memory(root, files)
    }

    fn slash_operator_session() -> Session {
        let root = PathBuf::from("/kio-repl-tests/slash");
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            root.join("app.pkg.kio"),
            "package app;\n\nbridge {\n  app/**;\n}\n".to_owned(),
        );
        files.insert(
            root.join("app/syntax.kio"),
            "module app/syntax;\npub fn choose[A](left: A, right: A) -> A { right }\n\
             pub op _ / _ { impl choose; };\n"
                .to_owned(),
        );
        files.insert(
            root.join("app/value.kio"),
            "module app/value;\npub fn item() -> . { () }\n".to_owned(),
        );
        files.insert(
            root.join("left/value.kio"),
            "module left/value;\npub fn item(left: ., right: .) -> . { left }\n".to_owned(),
        );
        files.insert(
            root.join("app/main.kio"),
            "module app/main;\nimport app/syntax(op _ / _);\nimport app/value as value;\n\
             import left/value as declared;\nfn left() -> . { () }\nfn other() -> . { () }\n"
                .to_owned(),
        );
        Session::new_in_memory(root, files)
    }

    fn structural_totality_session() -> Session {
        let root = PathBuf::from("/kio-repl-tests/totality");
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            root.join("totality.pkg.kio"),
            "package totality;\n\nbridge {\n  app/**;\n  elaborators;\n}\n".to_owned(),
        );
        files.insert(
            root.join("elaborators.kio"),
            r#"module elaborators;

import __comptime__;

pub type Target_request = __Type__ | .;

pure fn run(ct: __Comptime__, k: . -> __Checked_term__, value: __Checked_term__) -> __Checked_term__ {
  __structural_recur__(
    , ct
    , __Type__
    , __Checked_term__
    , __Checked_term__
    , __type_unit__(ct)
    , value
    , .(
        , _recur: (__Type__ & __Checked_term__) -> __Checked_term__
        , _fuel: __Type__
        , _input: __Checked_term__
        ) -> __Checked_term__ { k(()) }
    )
}

pure fn reenter(ct: __Comptime__, value: __Checked_term__) -> __Checked_term__ {
  run(
    , ct
    , .(_outer: .) -> __Checked_term__ {
        run(ct, .(_inner: .) -> __Checked_term__ { value }, value)
      }
    , value
    )
}

pub pure fn reentrant_checked(
  , ct: __Comptime__
  , _source: __Type__
  , value: __Checked_term__
  , _target: Target_request
  ) -> __Checked_term__ { reenter(ct, value) }

pub elab reentrant : [Source] Source -> [Target] Target { impl reentrant_checked; };
"#
            .to_owned(),
        );
        files.insert(
            root.join("app/main.kio"),
            "module app/main;\n\nimport elaborators(reentrant);\n\nfn idle() -> . { () }\n"
                .to_owned(),
        );
        Session::new_in_memory(root, files)
    }

    #[test]
    fn imported_operator_module_loads_and_supports_bare_query() {
        let mut session = imported_operator_session();
        let loaded = Command::Load("app/main".to_owned()).run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );

        let query = Command::Expr("? () : ()".to_owned()).run(&mut session, Palette::plain());
        assert!(query.output.contains("? () : () : ."), "{}", query.output);
        assert!(!query.output.contains("not a name or a Kio expression"));

        let doc = Command::Doc("run".to_owned()).run(&mut session, Palette::plain());
        assert!(doc.output.contains("? () : ()"), "{}", doc.output);
        assert!(!doc.output.contains("@source"), "{}", doc.output);
    }

    #[test]
    fn normalize_renders_structural_fault_as_totality() {
        let mut session = structural_totality_session();
        let loaded = Command::Load("app/main".to_owned()).run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );

        let result =
            Command::Normalize("reentrant!((), .)".to_owned()).run(&mut session, Palette::plain());
        assert!(
            result
                .output
                .starts_with("compile-time evaluation failed its Totality check:"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("root=1, current=1, next=1"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("package does not type-check"));
    }

    #[test]
    fn normalize_tries_a_slash_operator_expression_before_the_fqn_hint() {
        let mut session = slash_operator_session();
        let loaded = Command::Load("app/main".to_owned()).run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );

        let result =
            Command::Normalize("left/value.item".to_owned()).run(&mut session, Palette::plain());
        assert_eq!(result.output, "<closure app/value.item()>");
        assert!(
            !result.output.contains("looks like a qualified name"),
            "a valid slash-operator expression was rejected as an FQN: {}",
            result.output
        );
        assert!(
            !result.output.contains("not a name or a Kio expression"),
            "the valid expression reached the invalid-input fallback: {}",
            result.output
        );
    }

    #[test]
    fn normalize_fqn_suggests_entity_views_without_denying_name_shape() {
        let mut session = imported_operator_session();
        let loaded = Command::Load("app/main".to_owned()).run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );

        let actual = Command::Normalize("app/syntax.choose".to_owned())
            .run(&mut session, Palette::plain())
            .output;
        assert!(actual.contains("looks like a qualified name"), "{actual}");
        assert!(actual.contains(":signature app/syntax.choose"), "{actual}");
        assert!(!actual.contains("is not a name"), "{actual}");
    }

    #[test]
    fn type_prefers_a_resolvable_fqn_over_the_same_slash_operator_shape() {
        let mut session = slash_operator_session();
        let loaded = Command::Load("app/main".to_owned()).run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );

        let expected = cmd_type_name(&session, "left/value.item", Palette::plain());
        assert!(
            !expected.contains("did not resolve"),
            "test FQN did not resolve: {expected}"
        );
        let actual = Command::Type("left/value.item".to_owned())
            .run(&mut session, Palette::plain())
            .output;
        assert_eq!(actual, expected);
    }

    #[test]
    fn type_falls_back_from_an_unresolved_fqn_shape_to_a_slash_expression() {
        let mut session = slash_operator_session();
        let loaded = Command::Load("app/main".to_owned()).run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );

        let actual = Command::Type("other/value.item".to_owned())
            .run(&mut session, Palette::plain())
            .output;
        assert!(
            actual.starts_with("other/value.item : "),
            "unresolved FQN shape did not reach the expression branch: {actual}"
        );
        assert!(!actual.contains("did not resolve"), "{actual}");
    }

    #[test]
    fn type_falls_back_from_an_unresolved_slash_only_shape_to_an_expression() {
        let mut session = slash_operator_session();
        let loaded = Command::Load("app/main".to_owned()).run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );

        let actual = Command::Type("left/other".to_owned())
            .run(&mut session, Palette::plain())
            .output;
        assert_eq!(actual, "left/other : . -> .");
    }

    #[test]
    fn type_prefers_a_loaded_module_over_the_same_slash_expression_shape() {
        let mut session = slash_operator_session();
        let loaded = Command::Load("app/main".to_owned()).run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );

        let actual = Command::Type("left/value".to_owned())
            .run(&mut session, Palette::plain())
            .output;
        assert_eq!(
            actual,
            "`left/value` is a module, not a value — `:ls left/value` lists its items"
        );
    }

    #[test]
    fn type_treats_alias_qualified_value_path_as_an_expression() {
        let mut session = slash_operator_session();
        let loaded = Command::Load("app/main".to_owned()).run(&mut session, Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );

        let actual = Command::Type("value.item".to_owned())
            .run(&mut session, Palette::plain())
            .output;
        assert_eq!(actual, "value.item : . -> .");
    }

    #[test]
    fn load_plan_uses_one_analysis_source_snapshot() {
        let root = tempfile::tempdir().expect("temporary package root");
        std::fs::write(
            root.path().join("app.pkg.kio"),
            "package app;\n\nbridge {\n  app/**;\n}\n",
        )
        .expect("package file");
        let module_dir = root.path().join("app");
        std::fs::create_dir_all(&module_dir).expect("module directory");
        let provider_path = module_dir.join("syntax.kio");
        std::fs::write(
            &provider_path,
            "module app/syntax;\npub fn choose(left: ., right: .) -> . { left }\npub op ? _ : _ { impl choose }\n",
        )
        .expect("provider module");
        std::fs::write(
            module_dir.join("main.kio"),
            "module app/main;\nimport app/syntax(op ? _ : _);\npub fn run() -> . { ? () : () }\n",
        )
        .expect("consumer module");
        let mut session = Session::new(root.path().to_path_buf());
        session.refresh().expect("package analysis");

        std::fs::write(
            provider_path,
            "module app/syntax;\npub fn choose(left: ., right: .) -> . { left }\npub op ? _ ! _ { impl choose; };\n",
        )
        .expect("provider changed after analysis");
        let staged = plan_load(&session, "app/main").expect("snapshot-consistent load plan");
        assert!(staged.iter().any(|module| module.path == "app/main"));
        assert!(staged.iter().any(|module| module.path == "app/syntax"));
    }

    #[test]
    fn every_synonym_parses_in_both_directions() {
        // :l / :load
        assert!(matches!(parse_command(":l x"), Ok(Command::Load(_))));
        assert!(matches!(parse_command(":load x"), Ok(Command::Load(_))));
        // :u / :unload
        assert!(matches!(parse_command(":u x"), Ok(Command::Unload(_))));
        assert!(matches!(parse_command(":unload x"), Ok(Command::Unload(_))));
        // :q / :quit
        assert_eq!(parse_command(":q"), Ok(Command::Quit));
        assert_eq!(parse_command(":quit"), Ok(Command::Quit));
        // :h / :? / :help — help carries two short forms
        assert_eq!(parse_command(":h"), Ok(Command::Help));
        assert_eq!(parse_command(":?"), Ok(Command::Help));
        assert_eq!(parse_command(":help"), Ok(Command::Help));
        // :t / :type
        assert!(matches!(parse_command(":t x"), Ok(Command::Type(_))));
        assert!(matches!(parse_command(":type x"), Ok(Command::Type(_))));
        // :sig / :signature
        assert!(matches!(parse_command(":sig x"), Ok(Command::Signature(_))));
        assert!(matches!(
            parse_command(":signature x"),
            Ok(Command::Signature(_))
        ));
        // :src / :source
        assert!(matches!(parse_command(":src x"), Ok(Command::Source(_))));
        assert!(matches!(parse_command(":source x"), Ok(Command::Source(_))));
        // :ls / :list — both with and without an argument
        assert!(matches!(
            parse_command(":ls x"),
            Ok(Command::Ls {
                module: Some(_),
                verbose: false
            })
        ));
        assert!(matches!(
            parse_command(":list x"),
            Ok(Command::Ls {
                module: Some(_),
                verbose: false
            })
        ));
        assert!(matches!(
            parse_command(":ls"),
            Ok(Command::Ls {
                module: None,
                verbose: false
            })
        ));
        assert!(matches!(
            parse_command(":list"),
            Ok(Command::Ls {
                module: None,
                verbose: false
            })
        ));
        // :mods / :modules
        assert_eq!(parse_command(":mods"), Ok(Command::Mods));
        assert_eq!(parse_command(":modules"), Ok(Command::Mods));
        // :refs / :references
        assert!(matches!(parse_command(":refs x"), Ok(Command::Refs(_))));
        assert!(matches!(
            parse_command(":references x"),
            Ok(Command::Refs(_))
        ));
    }

    #[test]
    fn parse_rejects_missing_argument() {
        assert!(parse_command(":load").is_err());
        assert!(parse_command(":t").is_err());
        assert!(parse_command(":pure").is_err());
        assert!(parse_command(":refs").is_err());
    }

    #[test]
    fn missing_argument_renders_per_command_usage_hint() {
        // `:doc` and `:t` (and the others) get a per-command usage
        // hint instead of a generic "see `:help`" pointer.
        let err = parse_command(":doc").expect_err("empty :doc is an error");
        assert!(err.0.contains("Usage: :doc"), "got: {}", err.0);
        let err = parse_command(":t").expect_err("empty :t is an error");
        assert!(err.0.contains("Usage: :t"), "got: {}", err.0);
        let err = parse_command(":pure").expect_err("empty :pure is an error");
        assert!(err.0.contains("Usage: :pure"), "got: {}", err.0);
        let err = parse_command(":signature").expect_err("empty :signature is an error");
        assert!(err.0.contains("Usage: :signature"), "got: {}", err.0);
        let err = parse_command(":normalize").expect_err("empty :normalize is an error");
        assert!(err.0.contains("Usage: :normalize"), "got: {}", err.0);
    }

    #[test]
    fn parse_rejects_argument_for_no_arg_command() {
        assert!(parse_command(":mods extra").is_err());
        assert!(parse_command(":help extra").is_err());
        assert!(parse_command(":quit now").is_err());
        assert!(parse_command(":scope extra").is_err());
    }

    #[test]
    fn parse_scope_command() {
        assert_eq!(
            parse_command(":scope"),
            Ok(Command::Scope { verbose: false })
        );
        assert_eq!(
            parse_command(":scope -v"),
            Ok(Command::Scope { verbose: true })
        );
    }

    #[test]
    fn parse_ls_verbose_flag() {
        // `-v` is recognised in any position and strips from the
        // module argument.
        assert_eq!(
            parse_command(":ls -v"),
            Ok(Command::Ls {
                module: None,
                verbose: true
            })
        );
        assert_eq!(
            parse_command(":ls -v demo/main"),
            Ok(Command::Ls {
                module: Some("demo/main".to_owned()),
                verbose: true
            })
        );
        assert_eq!(
            parse_command(":ls demo/main -v"),
            Ok(Command::Ls {
                module: Some("demo/main".to_owned()),
                verbose: true
            })
        );
    }

    #[test]
    fn parse_rejects_unknown_command() {
        assert!(parse_command(":frobnicate").is_err());
    }

    #[test]
    fn parse_tolerates_leading_and_trailing_whitespace() {
        assert_eq!(
            parse_command("   :load pkg/a   "),
            Ok(Command::Load("pkg/a".to_owned()))
        );
    }

    #[test]
    fn parse_empty_colon_is_error() {
        assert!(parse_command(":").is_err());
    }

    // ── synonym table ────────────────────────────────────────────────────────

    #[test]
    fn canonical_name_folds_synonyms() {
        assert_eq!(canonical_name("l"), "load");
        assert_eq!(canonical_name("type"), "t");
        assert_eq!(canonical_name("sig"), "signature");
        assert_eq!(canonical_name("src"), "source");
        assert_eq!(canonical_name("modules"), "mods");
        assert_eq!(canonical_name("references"), "refs");
        assert_eq!(canonical_name("norm"), "normalize");
        // Already canonical maps to itself.
        assert_eq!(canonical_name("load"), "load");
        assert_eq!(canonical_name("t"), "t");
        assert_eq!(canonical_name("signature"), "signature");
        assert_eq!(canonical_name("source"), "source");
    }

    #[test]
    fn all_command_spellings_includes_both_forms() {
        let v = all_command_spellings();
        assert!(v.contains(&":load".to_owned()));
        assert!(v.contains(&":l".to_owned()));
        assert!(v.contains(&":references".to_owned()));
        assert!(v.contains(&":refs".to_owned()));
        // The new query-trio short forms tab-complete.
        assert!(v.contains(&":signature".to_owned()));
        assert!(v.contains(&":sig".to_owned()));
        assert!(v.contains(&":source".to_owned()));
        assert!(v.contains(&":src".to_owned()));
        // `:normalize` and its `:norm` short form tab-complete.
        assert!(v.contains(&":normalize".to_owned()));
        assert!(v.contains(&":norm".to_owned()));
        // `:help`'s two short forms — `:h` and `:?` — both tab-complete.
        assert!(v.contains(&":help".to_owned()));
        assert!(v.contains(&":h".to_owned()));
        assert!(v.contains(&":?".to_owned()));
    }

    // ── completion shape table ───────────────────────────────────────────────

    #[test]
    fn completion_shape_classifies_canonical_commands() {
        // Module-path arguments.
        assert_eq!(completion_shape("load"), CompletionShape::ModulePath);
        assert_eq!(completion_shape("unload"), CompletionShape::ModulePath);
        assert_eq!(completion_shape("ls"), CompletionShape::ModulePath);
        // Named-item views that also resolve operator tokens.
        assert_eq!(completion_shape("signature"), CompletionShape::NameFqnOrOp);
        assert_eq!(completion_shape("source"), CompletionShape::NameFqnOrOp);
        assert_eq!(completion_shape("doc"), CompletionShape::NameFqnOpOrBuiltin);
        // Name / FQN arguments without operators.
        assert_eq!(completion_shape("t"), CompletionShape::NameOrFqn);
        assert_eq!(completion_shape("pure"), CompletionShape::NameOrFqn);
        assert_eq!(
            completion_shape("which"),
            CompletionShape::NameOrFqnOrBuiltin
        );
        assert_eq!(completion_shape("refs"), CompletionShape::NameOrFqn);
        // Expression argument.
        assert_eq!(completion_shape("normalize"), CompletionShape::Expression);
        // No-argument commands.
        assert_eq!(completion_shape("mods"), CompletionShape::None);
        assert_eq!(completion_shape("help"), CompletionShape::None);
        assert_eq!(completion_shape("reset"), CompletionShape::None);
        assert_eq!(completion_shape("quit"), CompletionShape::None);
        assert_eq!(completion_shape("scope"), CompletionShape::None);
    }

    #[test]
    fn completion_shape_unknown_command_completes_nothing() {
        // An unrecognized canonical name (the completer reaches this
        // when the user typed a command that doesn't exist) yields
        // `None` — there is nothing sensible to complete.
        assert_eq!(completion_shape("frobnicate"), CompletionShape::None);
    }

    #[test]
    fn completion_shape_after_canonical_name_folds_synonyms() {
        // Synonyms must fold through `canonical_name` before the shape
        // lookup: `:l` → `load` → ModulePath, `:sig` → `signature` →
        // NameFqnOrOp, `:references` → `refs` → NameOrFqn.
        assert_eq!(
            completion_shape(canonical_name("l")),
            CompletionShape::ModulePath
        );
        assert_eq!(
            completion_shape(canonical_name("u")),
            CompletionShape::ModulePath
        );
        assert_eq!(
            completion_shape(canonical_name("list")),
            CompletionShape::ModulePath
        );
        assert_eq!(
            completion_shape(canonical_name("sig")),
            CompletionShape::NameFqnOrOp
        );
        assert_eq!(
            completion_shape(canonical_name("src")),
            CompletionShape::NameFqnOrOp
        );
        assert_eq!(
            completion_shape(canonical_name("type")),
            CompletionShape::NameOrFqn
        );
        assert_eq!(
            completion_shape(canonical_name("references")),
            CompletionShape::NameOrFqn
        );
        assert_eq!(
            completion_shape(canonical_name("modules")),
            CompletionShape::None
        );
    }

    // ── handler smoke tests ──────────────────────────────────────────────────

    #[test]
    fn expression_totality_error_has_a_distinct_renderer() {
        let error = ReplExprQueryError::Totality(
            "same helper origin did not descend (root=1, current=1, next=1)".to_owned(),
        );
        let rendered = render_expr_error(&error);
        assert!(rendered.starts_with("compile-time evaluation failed its Totality check:"));
        assert!(!rendered.contains("package does not type-check"));
    }

    #[test]
    fn package_error_text_cannot_impersonate_totality() {
        let error = ReplExprQueryError::Query(ExprQueryError::PackageError(
            "compile-time evaluation failed its Totality check: ordinary package failure"
                .to_owned(),
        ));
        let rendered = render_expr_error(&error);
        assert!(rendered.starts_with("the package does not type-check:"));
        assert!(rendered.contains("ordinary package failure"));
    }

    fn session_with(modules: &[(&str, &str)]) -> Session {
        // Build a session by staging modules directly (no disk /
        // analysis). The handlers under test here (`:t`, `:doc`,
        // `:source`, `:ls`, `:mods`, `:which`) read only the parsed
        // ASTs, so a hand-staged session exercises them. `:refs` uses
        // `session_with_analysis` below because it reads cached source.
        let mut s = Session::new(std::path::PathBuf::from("/tmp/pkg"));
        for (path, src) in modules {
            let module = crate::pass::parser::parse(src).expect("test module parses");
            let staged = StagedModule {
                path: (*path).to_owned(),
                file_path: std::path::PathBuf::from(format!("/tmp/{path}.kio")),
                module,
            };
            s.commit_load(path, &[staged]);
        }
        s
    }

    fn session_with_analysis(path: &str, source: &str) -> (tempfile::TempDir, Session) {
        let root = tempfile::tempdir().expect("temporary package root");
        let package = path.split('/').next().expect("package segment");
        std::fs::write(
            root.path().join(format!("{package}.pkg.kio")),
            format!("package {package};\n\nbridge {{\n  {package}/**;\n}}\n"),
        )
        .expect("package file");
        let file_path = root.path().join(format!("{path}.kio"));
        std::fs::create_dir_all(file_path.parent().expect("module parent"))
            .expect("module directory");
        std::fs::write(&file_path, source).expect("module source");

        let module = crate::pass::parser::parse(source).expect("test module parses");
        let staged = StagedModule {
            path: path.to_owned(),
            file_path,
            module,
        };
        let mut session = Session::new(root.path().to_path_buf());
        session.commit_load(path, &[staged]);
        session.refresh().expect("package analysis");
        (root, session)
    }

    #[test]
    fn cmd_type_renders_fn_type() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\npub fn add(x: . , y: .) -> . { () }\n",
        )]);
        let out = cmd_type_name(&s, "add", Palette::plain());
        assert!(out.contains("add :"), "got: {out}");
    }

    #[test]
    fn cmd_type_reports_type_level_name_with_kind_aware_error() {
        // `:t` on a `type` is a kind-aware error — the same message
        // shape `kio doc check` produces for `@type`.
        let s = session_with(&[("pkg/a", "module pkg/a;\ntype Same[A] = A;\n")]);
        let out = cmd_type_name(&s, "Same", Palette::plain());
        assert!(out.contains("type alias"), "got: {out}");
        assert!(out.contains("value binding"), "got: {out}");
    }

    #[test]
    fn cmd_type_reports_labels_name_with_kind_aware_error() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\npub labels Color = { red: ., green: . };\n",
        )]);
        let out = cmd_type_name(&s, "Color", Palette::plain());
        assert!(out.contains("`labels`"), "got: {out}");
        assert!(out.contains("value binding"), "got: {out}");
    }

    #[test]
    fn variadic_operator_queries_report_current_binding_kind() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\nfn seed() -> . { () }\nfn step(x: ., y: .) -> . { x }\n\
             varop [* *] { foldr step seed; };\n",
        )]);
        let grammar = "varop [* *]";
        assert_eq!(
            cmd_type_name(&s, grammar, Palette::plain()),
            "`varop [* *]` is a variadic operator binding — use `:source varop [* *]`"
        );
        assert_eq!(
            cmd_pure_name(&s, grammar),
            "`varop [* *]` is a variadic operator binding; `:pure` expects an executable value binding (fn or host fn)"
        );
    }

    #[test]
    fn cmd_pure_uses_declaration_metadata_and_compiler_pure_context() {
        let (_root, mut session) = session_with_analysis(
            "pkg/a",
            "module pkg/a;\n\
             host type Text role(str);\n\
             host fn emit(value: Text) -> .;\n\
             pure fn identity(x: .) -> . { x }\n\
             fn unrestricted(x: .) -> . { x }\n\
             type Alias = .;\n",
        );

        let cases = [
            ("()", "pure"),
            ("() // trailing comment", "pure"),
            ("identity", "pure"),
            ("pkg/a.identity", "pure"),
            ("identity(())", "pure"),
            ("unrestricted", "impure"),
            ("unrestricted(())", "impure"),
            ("emit", "impure"),
            ("emit(\"seen\")", "impure"),
            (".(x: .) { x }", "pure"),
            ("(.(x: .) { x })(())", "pure"),
            (".(_x: .) { emit(\"captured\") }", "impure"),
        ];
        for (query, expected) in cases {
            assert_eq!(cmd_pure(&mut session, query), expected, "for `{query}`");
        }

        let escaped = cmd_pure(&mut session, "(); () } fn injected() { emit(\"seen\")");
        assert_ne!(escaped, "pure", "wrapper escape must not earn a verdict");
        assert_ne!(escaped, "impure", "wrapper escape must not earn a verdict");

        assert_eq!(
            cmd_pure(&mut session, "Alias"),
            "`Alias` is a type alias; `:pure` expects an executable value binding (fn or host fn)"
        );
    }

    #[test]
    fn cmd_pure_preserves_invalid_input_and_no_module_diagnostics() {
        let (_root, mut session) = session_with_analysis(
            "pkg/a",
            "module pkg/a;\npure fn identity(x: .) -> . { x }\n",
        );
        let invalid = cmd_pure(&mut session, "missing(())");
        assert_ne!(invalid, "pure");
        assert_ne!(invalid, "impure");
        assert!(invalid.contains("missing"), "got: {invalid}");
        assert!(!invalid.contains("_expr_"), "got: {invalid}");

        let malformed = cmd_pure(&mut session, "(");
        assert_ne!(malformed, "pure");
        assert_ne!(malformed, "impure");
        assert!(!malformed.contains("_expr_"), "got: {malformed}");

        let mut no_module = Session::new(std::path::PathBuf::from("/tmp/pkg"));
        let diagnostic = cmd_pure(&mut no_module, "()");
        assert!(
            diagnostic.contains("no current module"),
            "got: {diagnostic}"
        );
    }

    #[test]
    fn forwarding_declaration_commands_keep_local_imported_and_qualified_selectors() {
        let provider = concat!(
            "module pkg/api; pub fn field() -> . { () }\n",
            "/// The forwarded field.\n",
            "pub type {field} = {original};\n",
        );
        for modules in [
            vec![("pkg/api", provider)],
            vec![
                ("pkg/api", provider),
                (
                    "pkg/main",
                    "module pkg/main; import pkg/api(field, {field});",
                ),
            ],
        ] {
            let mut session = session_with(&modules);
            for selector in ["{field}", "pkg/api.{field}"] {
                for command in ["signature", "source"] {
                    let output = parse_command(&format!(":{command} {selector}"))
                        .unwrap()
                        .run(&mut session, Palette::plain())
                        .output;
                    assert_eq!(output, "pub type {field} = {original};");
                }
                let output = parse_command(&format!(":doc {selector}"))
                    .unwrap()
                    .run(&mut session, Palette::plain())
                    .output;
                assert!(output.contains("The forwarded field."), "{output}");
                assert!(
                    output.contains("pub type {field} = {original};"),
                    "{output}"
                );
                assert!(!output.contains("newtype Field"), "{output}");
            }
            assert!(cmd_signature(&session, "field", Palette::plain()).starts_with("pub fn field"));
            assert!(resolve_name(&session, "Field").is_none());
        }
        assert!(matches!(
            classify_input("{field}"),
            Some(InputKind::Compound(_))
        ));
        assert_eq!(doc_parse("{field}"), None);
    }

    #[test]
    fn forwarded_label_refs_do_not_select_a_same_spelled_function() {
        let (_root, session) = session_with_analysis(
            "pkg/api",
            concat!(
                "module pkg/api; labels { original: . };\n",
                "fn field() -> . { () }\n",
                "type {field} = {original};\n",
            ),
        );
        let references = cmd_refs(&session, "{field}");
        assert!(references.contains("in type {field}"), "{references}");
        assert!(!references.contains("in fn field"), "{references}");
        assert_eq!(references.lines().count(), 2, "{references}");
    }

    #[test]
    fn cmd_signature_prints_fn_header() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\npub fn add(x: ., y: .) -> . { x }\n",
        )]);
        let out = cmd_signature(&s, "add", Palette::plain());
        // The declaration header — no body.
        assert!(out.contains("fn add"), "got: {out}");
        assert!(!out.contains('{'), "got: {out}");
    }

    #[test]
    fn named_views_select_exact_rec_group_member() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\n\
             rec(loop) {\n\
               /// First member docs.\n\
               fn first(value: First) -> First { value };\n\
               /// Second member docs.\n\
               fn second(value: Second) -> Second { value }\n\
             }\n",
        )]);

        let signature = cmd_signature(&s, "second", Palette::plain());
        assert!(signature.contains("fn second"), "got: {signature}");
        assert!(!signature.contains("fn first"), "got: {signature}");

        let ty = cmd_type_name(&s, "second", Palette::plain());
        assert!(ty.contains("second :"), "got: {ty}");
        assert!(ty.contains("Second"), "got: {ty}");
        assert!(!ty.contains("First"), "got: {ty}");

        let doc = cmd_doc(&s, "second", Palette::plain());
        assert!(doc.contains("Second member docs"), "got: {doc}");
        assert!(!doc.contains("First member docs"), "got: {doc}");
        assert!(doc.contains("fn second"), "got: {doc}");

        let source = cmd_source(&s, "second", Palette::plain());
        assert!(source.contains("fn second"), "got: {source}");
        assert!(!source.contains("fn first"), "got: {source}");
    }

    #[test]
    fn named_views_select_exact_type_rec_group_member_by_bare_name_and_fqn() {
        let mut s = session_with(&[(
            "pkg/a",
            "module pkg/a;\n\
             rec {\n\
               /// First member docs.\n\
               type First = Second;\n\
               /// Second member docs.\n\
               newtype Second : First { constructor mk; projector un; };\n\
             }\n",
        )]);

        for name in ["Second", "pkg/a.Second"] {
            let signature = cmd_signature(&s, name, Palette::plain());
            assert!(signature.contains("newtype Second"), "got: {signature}");
            assert!(signature.contains("rec {"), "got: {signature}");
            assert!(signature.contains("type First ="), "got: {signature}");

            let doc = cmd_doc(&s, name, Palette::plain());
            assert!(doc.contains("Second member docs"), "got: {doc}");
            assert!(!doc.contains("First member docs"), "got: {doc}");
            assert!(doc.contains("newtype Second"), "got: {doc}");

            let source = cmd_source(&s, name, Palette::plain());
            assert!(source.contains("newtype Second"), "got: {source}");
            assert!(source.contains("rec {"), "got: {source}");
            assert!(source.contains("type First ="), "got: {source}");
        }

        for (name, kind) in [("First", "type alias"), ("pkg/a.Second", "`newtype`")] {
            let purity = cmd_pure(&mut s, name);
            assert!(purity.contains(kind), "got: {purity}");
            assert!(purity.contains("executable value binding"), "got: {purity}");
        }
    }

    #[test]
    fn cmd_signature_prints_type_level_header() {
        // `:signature` works on a type-level name too — it prints the
        // declaration header, no kind error (that is `:type`'s job).
        let s = session_with(&[("pkg/a", "module pkg/a;\ntype Same[A] = A;\n")]);
        let out = cmd_signature(&s, "Same", Palette::plain());
        assert!(out.contains("type Same"), "got: {out}");
    }

    #[test]
    fn cmd_source_prints_declaration() {
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn run() -> . { () }\n")]);
        let out = cmd_source(&s, "run", Palette::plain());
        assert!(out.contains("fn run"));
        assert!(out.contains('{'));
    }

    #[test]
    fn cmd_source_resolves_operator_token() {
        // Named-item queries resolve the same complete tagged grammar.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\npub fn add(x: ., y: .) -> . { x }\nop _ + __ { impl add }\n",
        )]);
        let out = cmd_source(&s, "op _ + __", Palette::plain());
        assert!(out.contains("op _ + __ { impl add"), "got: {out}");
        assert!(!out.contains("unresolved"), "got: {out}");
    }

    #[test]
    fn cmd_signature_resolves_operator_token() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\npub fn add(x: ., y: .) -> . { x }\nop _ + __ { impl add }\n",
        )]);
        let out = cmd_signature(&s, "op _ + __", Palette::plain());
        assert!(out.contains("op _ + __ { impl add"), "got: {out}");
    }

    // ── :source preceding-comment block ─────────────────────────────────────

    #[test]
    fn preceding_comment_block_captures_line_comments() {
        let src = "module pkg/a;\n\
                   // a leading\n\
                   // two-line note\n\
                   pub fn run() -> . { () }\n";
        let item_start = src.find("pub fn").expect("locate item");
        let prefix = preceding_comment_block(src, item_start);
        assert_eq!(prefix, "// a leading\n// two-line note");
    }

    #[test]
    fn preceding_comment_block_captures_doc_comments() {
        let src = "module pkg/a;\n\
                   /// One-line doc-comment.\n\
                   pub fn run() -> . { () }\n";
        let item_start = src.find("pub fn").expect("locate item");
        let prefix = preceding_comment_block(src, item_start);
        assert_eq!(prefix, "/// One-line doc-comment.");
    }

    #[test]
    fn preceding_comment_block_captures_mixed_runs() {
        // A run of mixed `//` and `///` lines is kept contiguous.
        let src = "module pkg/a;\n\
                   // note\n\
                   /// doc\n\
                   // more\n\
                   pub fn run() -> . { () }\n";
        let item_start = src.find("pub fn").expect("locate item");
        let prefix = preceding_comment_block(src, item_start);
        assert_eq!(prefix, "// note\n/// doc\n// more");
    }

    #[test]
    fn preceding_comment_block_stops_at_blank_line() {
        // A blank line above the comment block terminates the run —
        // the comment is NOT part of the item.
        let src = "module pkg/a;\n\
                   // far away, separated\n\
                   \n\
                   pub fn run() -> . { () }\n";
        let item_start = src.find("pub fn").expect("locate item");
        let prefix = preceding_comment_block(src, item_start);
        assert_eq!(prefix, "");
    }

    #[test]
    fn preceding_comment_block_stops_at_previous_code() {
        // The previous item's closing brace terminates the run.
        let src = "module pkg/a;\n\
                   pub fn first() -> . { () }\n\
                   // attached to second\n\
                   pub fn second() -> . { () }\n";
        let item_start = src.find("pub fn second").expect("locate item");
        let prefix = preceding_comment_block(src, item_start);
        assert_eq!(prefix, "// attached to second");
    }

    #[test]
    fn preceding_comment_block_handles_no_preceding_comment() {
        let src = "module pkg/a;\npub fn run() -> . { () }\n";
        let item_start = src.find("pub fn").expect("locate item");
        assert_eq!(preceding_comment_block(src, item_start), "");
    }

    #[test]
    fn cmd_ls_lists_items() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\npub fn f() -> . { () }\ntype T[A] = A;\n",
        )]);
        let out = cmd_ls(&s, Some("pkg/a"), false, Palette::plain());
        assert!(out.contains("pub fn f"));
        assert!(out.contains("type T"));
        // The terse listing elided a signature, so the `-v` footer
        // advertises the fuller view.
        assert!(
            out.contains(":ls -v"),
            "terse `:ls` should advertise `-v`, got:\n{out}"
        );
    }

    #[test]
    fn cmd_ls_verbose_renders_full_signatures() {
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn f(x: ., y: .) -> . { x }\n")]);
        let terse = cmd_ls(&s, Some("pkg/a"), false, Palette::plain());
        // Terse: keyword + name only, no parameter list.
        assert!(terse.contains("pub fn f"), "got:\n{terse}");
        assert!(!terse.contains("x: ."), "terse elides params: {terse}");
        let verbose = cmd_ls(&s, Some("pkg/a"), true, Palette::plain());
        // Verbose: the full signature, with the parameter types.
        assert!(verbose.contains("x: ."), "verbose shows params:\n{verbose}");
        // And no footer — `-v` is already in effect.
        assert!(
            !verbose.contains(":ls -v"),
            "verbose suppresses the footer:\n{verbose}"
        );
    }

    #[test]
    fn cmd_ls_flattens_recursive_members_in_both_modes() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\n\
             rec(loop) {\n\
               fn first(value: .) -> . { rec second(value) };\n\
               pub fn second(value: .) -> . { rec first(value) }\n\
             }\n",
        )]);

        let terse = cmd_ls(&s, Some("pkg/a"), false, Palette::plain());
        assert!(terse.contains("fn first"), "got:\n{terse}");
        assert!(terse.contains("pub fn second"), "got:\n{terse}");
        assert!(!terse.contains("rec fn"), "got:\n{terse}");

        let verbose = cmd_ls(&s, Some("pkg/a"), true, Palette::plain());
        assert!(
            verbose.contains("fn first(value: .) -> ."),
            "got:\n{verbose}"
        );
        assert!(
            verbose.contains("pub fn second(value: .) -> ."),
            "got:\n{verbose}"
        );
        assert!(!verbose.contains("rec(loop)"), "got:\n{verbose}");
        assert!(!verbose.contains("rec second(value)"), "got:\n{verbose}");
    }

    #[test]
    fn cmd_ls_op_terse_shows_name_verbose_shows_pattern() {
        // Terse output retains the complete grammar; verbose output also
        // shows the callable clause.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;
             fn cond(c: ., a: ., b: .) -> . { a }
             op _ ? _ : __ { impl cond; };
",
        )]);
        let terse = cmd_ls(&s, Some("pkg/a"), false, Palette::plain());
        assert!(
            terse.contains("op _ ? _ : __"),
            "terse shows the name:
{terse}"
        );
        assert!(
            !terse.contains("impl cond"),
            "terse elides the callable clause:
{terse}"
        );
        assert!(
            terse.contains(":ls -v"),
            "terse `:ls` advertises `-v`:
{terse}"
        );
        let verbose = cmd_ls(&s, Some("pkg/a"), true, Palette::plain());
        assert!(
            verbose.contains("op _ ? _ : __ { impl cond"),
            "verbose shows the full pattern:
{verbose}"
        );
    }

    #[test]
    fn cmd_ls_no_footer_when_nothing_to_expand() {
        // An anonymous-labels-only module elides no signature, so the
        // terse listing carries no `-v` footer.
        let s = session_with(&[("pkg/a", "module pkg/a;\nlabels { red: ., green: . };\n")]);
        let out = cmd_ls(&s, Some("pkg/a"), false, Palette::plain());
        assert!(
            !out.contains(":ls -v"),
            "no footer when nothing expands, got:\n{out}"
        );
    }

    #[test]
    fn cmd_ls_does_not_leak_label_braces_for_anonymous_labels() {
        // Regression: an anonymous `labels { ... };` block used to
        // render as the literal `labels { … }` entry — an internal
        // structural fragment leaking through. The fix expands the
        // anonymous form to one line per declared entry instead.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\n\
             pub fn run() -> . { () }\n\
             labels { red: ., green: . };\n",
        )]);
        let out = cmd_ls(&s, Some("pkg/a"), false, Palette::plain());
        // No literal `{ … }` or `labels { … }` in the output.
        assert!(
            !out.contains("{ … }"),
            "literal `{{ … }}` should not appear, got:\n{out}"
        );
        assert!(
            !out.contains("labels { ... }") && !out.contains("labels { … }"),
            "literal `labels {{ … }}` should not appear, got:\n{out}"
        );
        // The entries surface individually.
        assert!(out.contains("label red"), "got:\n{out}");
        assert!(out.contains("label green"), "got:\n{out}");
    }

    fn assert_fragments_once_in_order(output: &str, fragments: &[&str]) {
        let mut remainder = output;
        for fragment in fragments {
            assert_eq!(
                output.matches(fragment).count(),
                1,
                "expected exactly one `{fragment}` in:\n{output}"
            );
            let start = remainder
                .find(fragment)
                .unwrap_or_else(|| panic!("`{fragment}` was out of order in:\n{output}"));
            remainder = &remainder[start + fragment.len()..];
        }
    }

    #[test]
    fn cmd_ls_preserves_anonymous_recursive_labels_exactly_once_in_source_order() {
        let (_root, s) = session_with_analysis(
            "pkg/a",
            "module pkg/a;\n\
             rec {\n\
               pub labels { left: Right, right: Branch };\n\
               pub newtype Branch : Left { pub constructor branch; pub projector un_branch; };\n\
             }\n",
        );

        let terse = cmd_ls(&s, Some("pkg/a"), false, Palette::plain());
        assert_eq!(
            terse,
            "pkg/a:\n  pub label left\n  pub label right\n  pub newtype Branch\n\
             (… :ls -v for full signatures)"
        );

        let verbose = cmd_ls(&s, Some("pkg/a"), true, Palette::plain());
        assert_fragments_once_in_order(
            &verbose,
            &[
                "pub labels",
                "left: Right",
                "right: Branch",
                "pub newtype Branch : Left",
            ],
        );
    }

    #[test]
    fn cmd_ls_preserves_named_labels_inside_a_type_rec_group() {
        let (_root, s) = session_with_analysis(
            "pkg/a",
            "module pkg/a;\n\
             rec {\n\
               pub labels Node = { next: Tree };\n\
               pub newtype Tree : Node { pub constructor tree; pub projector un_tree; };\n\
             }\n",
        );

        let terse = cmd_ls(&s, Some("pkg/a"), false, Palette::plain());
        assert_eq!(
            terse,
            "pkg/a:\n  labels Node\n  pub newtype Tree\n(… :ls -v for full signatures)"
        );

        let verbose = cmd_ls(&s, Some("pkg/a"), true, Palette::plain());
        assert_fragments_once_in_order(
            &verbose,
            &["pub labels Node =", "next: Tree", "pub newtype Tree : Node"],
        );
    }

    #[test]
    fn cmd_ls_named_labels_renders_as_named_labels() {
        // The named form `labels T = { ... };` still renders as one
        // `labels T` entry — the alias name is the queryable handle.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\ntype Unit = .;\npub labels Color = { red: Unit, green: Unit };\n",
        )]);
        let out = cmd_ls(&s, Some("pkg/a"), false, Palette::plain());
        assert!(out.contains("labels Color"), "got:\n{out}");
    }

    #[test]
    fn cmd_ls_with_no_arg_lists_current_module() {
        // `:ls` with no argument lists the current module — pkg/a was
        // loaded last, so it is current.
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn f() -> . { () }\n")]);
        let out = cmd_ls(&s, None, false, Palette::plain());
        assert!(out.contains("pkg/a"), "got: {out}");
        assert!(out.contains("pub fn f"), "got: {out}");
    }

    #[test]
    fn cmd_ls_with_no_arg_and_no_current_module_diagnoses() {
        // `:ls` with no argument and no current module prints the
        // "no current module" diagnostic.
        let s = Session::new(std::path::PathBuf::from("/tmp/pkg"));
        let out = cmd_ls(&s, None, false, Palette::plain());
        assert!(out.contains("no current module"), "got: {out}");
    }

    #[test]
    fn cmd_mods_marks_current() {
        let s = session_with(&[("pkg/a", "module pkg/a;\n"), ("pkg/b", "module pkg/b;\n")]);
        let out = cmd_mods(&s);
        // pkg/b was loaded last → current → starred.
        assert!(out.contains("* pkg/b"), "got: {out}");
        assert!(out.contains("  pkg/a"), "got: {out}");
    }

    #[test]
    fn cmd_scope_lists_declared_imports_operators_aliases_intrinsics_and_comptime() {
        // A module with one of each: a declared `fn`, a selective
        // import, an operator binding, a module alias, `import
        // __intrinsics__;`, and `import __comptime__;`. `:scope` lists
        // each under its section header.
        let s = session_with(&[
            (
                "pkg/other",
                "module pkg/other;\npub fn callee() -> . { () }\n",
            ),
            (
                "pkg/a",
                "module pkg/a;\n\
                 import __intrinsics__;\n\
                 import __comptime__;\n\
                 import pkg/other(callee);\n\
                 import pkg/other as o;\n\
                 pub fn run() -> . { callee() }\n\
                 fn add(x: ., y: .) -> . { x }\n\
                 op _ + __ { impl add }\n",
            ),
        ]);
        let out = cmd_scope(&s, false, Palette::plain());
        assert!(out.contains("scope of pkg/a"), "got:\n{out}");
        // (1) declared items
        assert!(out.contains("declared:"), "got:\n{out}");
        assert!(out.contains("pub fn run"), "got:\n{out}");
        assert!(out.contains("fn add"), "got:\n{out}");
        // (2) imported names, grouped by source module
        assert!(out.contains("imported:"), "got:\n{out}");
        assert!(out.contains("from pkg/other:"), "got:\n{out}");
        assert!(out.contains("callee"), "got:\n{out}");
        // (3) operator bindings — listed by their complete tagged grammar
        assert!(out.contains("operators: op _ + __"), "got:\n{out}");
        // (4) module aliases
        assert!(out.contains("module aliases:"), "got:\n{out}");
        assert!(out.contains("o -> pkg/other"), "got:\n{out}");
        // (5) intrinsics and (6) comptime
        assert!(out.contains("intrinsics: in scope"), "got:\n{out}");
        assert!(out.contains("comptime: in scope"), "got:\n{out}");
    }

    #[test]
    fn cmd_scope_minimal_module_has_builtins_not_in_scope() {
        // A module with nothing imported and no builtin modules shows
        // only its declared section plus the builtin status lines in the
        // not-in-scope form.
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn f() -> . { () }\n")]);
        let out = cmd_scope(&s, false, Palette::plain());
        assert!(out.contains("scope of pkg/a"), "got:\n{out}");
        assert!(out.contains("pub fn f"), "got:\n{out}");
        assert!(out.contains("intrinsics: not in scope"), "got:\n{out}");
        assert!(out.contains("comptime: not in scope"), "got:\n{out}");
        // No `imported:` / `operators:` / `module aliases:` sections
        // are emitted when their lists are empty.
        assert!(!out.contains("imported:"), "got:\n{out}");
        assert!(!out.contains("operators:"), "got:\n{out}");
        assert!(!out.contains("module aliases:"), "got:\n{out}");
    }

    #[test]
    fn cmd_scope_without_current_module_diagnoses() {
        let s = Session::new(std::path::PathBuf::from("/tmp/pkg"));
        let out = cmd_scope(&s, false, Palette::plain());
        assert!(out.contains("no current module"), "got:\n{out}");
    }

    #[test]
    fn cmd_which_finds_declaring_module() {
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn helper() -> . { () }\n")]);
        assert_eq!(cmd_which(&s, "helper"), "pkg/a.helper");
    }

    #[test]
    fn cmd_which_finds_private_item() {
        // A private (no `pub`) item is reachable via `:which` —
        // declarations are scanned, not exports.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\nfn priv_helper() -> . { () }\n\
             pub fn run() -> . { () }\n",
        )]);
        assert_eq!(cmd_which(&s, "priv_helper"), "pkg/a.priv_helper");
        assert_eq!(cmd_which(&s, "run"), "pkg/a.run");
    }

    #[test]
    fn cmd_which_reports_missing() {
        let s = session_with(&[("pkg/a", "module pkg/a;\n")]);
        assert!(cmd_which(&s, "nothere").contains("no loaded module"));
    }

    // ── builtin-module resolution ────────────────────────────────────────────

    #[test]
    fn cmd_which_reports_intrinsic_in_scope() {
        // When some loaded module declares `import __intrinsics__;`, the
        // intrinsic name resolves through `:which` to a scope-aware
        // diagnostic naming the providing module.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\nimport __intrinsics__;\npub fn run() -> . { () }\n",
        )]);
        let out = cmd_which(&s, "__left__");
        assert!(
            out.contains("intrinsic in scope"),
            "expected intrinsic-in-scope shape, got:\n{out}"
        );
        assert!(
            out.contains("pkg/a"),
            "should name the providing module, got:\n{out}"
        );
    }

    #[test]
    fn cmd_which_unknown_double_underscore_name_falls_through_to_unresolved() {
        // A name that isn't in the canonical intrinsics list still
        // gets the "no loaded module declares" diagnostic, even when
        // intrinsics are in scope.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\nimport __intrinsics__;\npub fn run() -> . { () }\n",
        )]);
        let out = cmd_which(&s, "__not_an_intrinsic__");
        assert!(out.contains("no loaded module"), "got:\n{out}");
    }

    #[test]
    fn cmd_doc_renders_intrinsic_type_scheme() {
        // `:doc __left__` (in a module that `import __intrinsics__;`)
        // renders the intrinsic's scope provenance plus its type
        // scheme.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\nimport __intrinsics__;\npub fn run() -> . { () }\n",
        )]);
        let out = cmd_doc(&s, "__left__", Palette::plain());
        assert!(
            out.contains("intrinsic in scope"),
            "should explain provenance, got:\n{out}"
        );
        // The scheme of `__left__` is `[A] [B] (x: A) -> A | B`.
        // `pretty_type` formats the sum and the arrow; the scheme's
        // `a | b` return is the most stable substring.
        assert!(
            out.contains("__left__"),
            "should label the type with the intrinsic name, got:\n{out}"
        );
    }

    #[test]
    fn cmd_which_reports_comptime_in_scope() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\nimport __comptime__;\npub fn run() -> . { () }\n",
        )]);
        let out = cmd_which(&s, "__reflect_type__");
        assert!(
            out.contains("compile-time helper in scope"),
            "expected comptime-in-scope shape, got:\n{out}"
        );
        assert!(
            out.contains("pkg/a"),
            "should name the providing module, got:\n{out}"
        );
    }

    #[test]
    fn cmd_doc_renders_comptime_type_and_value_docs() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\nimport __comptime__;\npub fn run() -> . { () }\n",
        )]);
        let type_out = cmd_doc(&s, "__Type__", Palette::plain());
        assert!(type_out.contains("type __Type__"), "got:\n{type_out}");
        assert!(
            type_out.contains("compile-time helper in scope"),
            "got:\n{type_out}"
        );
        let value_out = cmd_doc(&s, "__reflect_type__", Palette::plain());
        assert!(
            value_out.contains("__reflect_type__ :"),
            "got:\n{value_out}"
        );
        assert!(
            value_out.contains("Reflects a type argument"),
            "got:\n{value_out}"
        );
    }

    #[test]
    fn resolve_builtin_returns_none_when_not_in_scope() {
        // No `import __intrinsics__;` anywhere — the intrinsic name is
        // not in scope and resolution returns `None`.
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn run() -> . { () }\n")]);
        assert!(resolve_builtin(&s, "__left__").is_none());
        assert!(resolve_builtin(&s, "__reflect_type__").is_none());
    }

    #[test]
    fn resolve_builtin_prefers_current_module() {
        // Two loaded modules, one with `import __intrinsics__;` and the
        // current pointing at it — `resolve_builtin` reports the
        // current module as the provenance source.
        let s = session_with(&[
            ("pkg/other", "module pkg/other;\nimport __intrinsics__;\n"),
            (
                "pkg/a",
                "module pkg/a;\nimport __intrinsics__;\npub fn run() -> . { () }\n",
            ),
        ]);
        // `pkg/a` was loaded last → current.
        let builtin = resolve_builtin(&s, "__left__").expect("intrinsic in scope");
        assert_eq!(builtin.via_module.as_deref(), Some("pkg/a"));
    }

    #[test]
    fn cmd_doc_renders_doc_comment_and_signature() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\n/// Adds the unit to itself.\npub fn f() -> . { () }\n",
        )]);
        let out = cmd_doc(&s, "f", Palette::plain());
        assert!(out.contains("Adds the unit"), "got: {out}");
        assert!(out.contains("fn f"), "got: {out}");
    }

    #[test]
    fn cmd_doc_renders_newtype_doc_comment() {
        // A `///` doc-comment on a `newtype` is surfaced by `:doc`
        // (previously silently dropped). Routes through the shared
        // `doc_entry` chokepoint.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\n\
             /// Wraps a unit value.\n\
             pub newtype Wrapped : . { constructor mk_w; projector un_w; };\n",
        )]);
        let out = cmd_doc(&s, "Wrapped", Palette::plain());
        assert!(out.contains("Wraps a unit value"), "got: {out}");
        assert!(out.contains("newtype Wrapped"), "got: {out}");
    }

    #[test]
    fn ordinary_label_nominal_queries_keep_the_written_owner() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\n/// The field label.\npub labels { field: . };\n",
        )]);
        let doc = cmd_doc(&s, "Field", Palette::plain());
        assert!(doc.contains("pub labels { field: . };"), "{doc}");
        assert!(doc.contains("The field label."), "{doc}");
        assert!(!doc.contains("newtype Field"));
        let signature = cmd_signature(&s, "Field", Palette::plain());
        assert!(
            signature.contains("pub labels { field: . };"),
            "{signature}"
        );
        let ty = cmd_type_name(&s, "Field", Palette::plain());
        assert!(ty.contains("is a `newtype`"), "{ty}");
        assert!(ty.contains("expects a value binding"), "{ty}");
        let purity = cmd_pure_name(&s, "Field");
        assert!(purity.contains("is a `newtype`"), "{purity}");
        assert!(
            purity.contains("expects an executable value binding"),
            "{purity}"
        );
    }

    #[test]
    fn recursive_label_nominal_queries_distinguish_the_named_alias() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\nrec { pub labels Row = { field: . }; }\n",
        )]);
        for name in ["Field", "Row"] {
            let kind = if name == "Field" {
                "a `newtype`"
            } else {
                "a `labels` declaration"
            };
            let ty = cmd_type_name(&s, name, Palette::plain());
            assert!(ty.contains(kind), "{ty}");
            let purity = cmd_pure_name(&s, name);
            assert!(purity.contains(kind), "{purity}");
        }
    }

    #[test]
    fn cmd_doc_signature_only_without_doc_comment() {
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn f() -> . { () }\n")]);
        let out = cmd_doc(&s, "f", Palette::plain());
        assert!(out.contains("fn f"));
    }

    #[test]
    fn cmd_doc_resolves_operator_token() {
        // The complete tagged grammar identifies the operator's doc entry.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\npub fn add(x: ., y: .) -> . { x }\nop _ + __ { impl add }\n",
        )]);
        let out = cmd_doc(&s, "op _ + __", Palette::plain());
        assert!(out.contains("op _ + __ { impl add"), "got: {out}");
        assert!(!out.contains("unresolved"), "got: {out}");
    }

    #[test]
    fn cmd_doc_on_a_module_renders_summary() {
        // `:doc <module>` renders the module's `///` doc-comment +
        // a one-line item/import count.
        let s = session_with(&[
            (
                "pkg/a",
                "/// Module-level doc for pkg/a.\nmodule pkg/a;\n\
             import pkg/other(f);\n\
             pub fn run() -> . { () }\n\
             fn helper() -> . { () }\n",
            ),
            ("pkg/other", "module pkg/other;\npub fn f() -> . { () }\n"),
        ]);
        let out = cmd_doc(&s, "pkg/a", Palette::plain());
        assert!(
            out.contains("Module-level doc for pkg/a."),
            "module doc-comment should render, got:\n{out}"
        );
        assert!(out.contains("module pkg/a"), "got:\n{out}");
        assert!(out.contains("2 declared items"), "got:\n{out}");
        assert!(out.contains("1 import"), "got:\n{out}");
    }

    #[test]
    fn cmd_doc_on_a_module_without_doc_comment_still_summarises() {
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn run() -> . { () }\n")]);
        let out = cmd_doc(&s, "pkg/a", Palette::plain());
        assert!(out.contains("module pkg/a"), "got:\n{out}");
        assert!(out.contains("1 declared item"), "got:\n{out}");
        assert!(out.contains("0 imports"), "got:\n{out}");
    }

    #[test]
    fn cmd_doc_counts_recursive_members_as_declarations() {
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\n\
             rec(loop) {\n\
               fn first(value: .) -> . { rec second(value) };\n\
               fn second(value: .) -> . { rec first(value) }\n\
             }\n",
        )]);
        let out = cmd_doc(&s, "pkg/a", Palette::plain());
        assert!(out.contains("2 declared items"), "got:\n{out}");
    }

    #[test]
    fn cmd_refs_finds_call_site() {
        let (_root, s) = session_with_analysis(
            "pkg/a",
            "module pkg/a;\npub fn helper() -> . { () }\n\
             pub fn run() -> . { helper() }\n",
        );
        let out = cmd_refs(&s, "helper");
        assert_eq!(out.matches("pkg/a:").count(), 2, "got: {out}");
    }

    #[test]
    fn cmd_refs_includes_label_reuse_markers() {
        let (_root, s) = session_with_analysis(
            "pkg/a",
            "module pkg/a;\nlabels { field: . };\nlabels Row = { field: _ };\n",
        );
        let out = cmd_refs(&s, "field");
        assert_eq!(out.matches("pkg/a:").count(), 2, "got: {out}");
    }

    #[test]
    fn enclosing_item_label_picks_each_item_kind() {
        // Drive each `Item` variant through `enclosing_item_label` —
        // offset inside the item's span returns the matching label,
        // offset outside falls back to the module label.
        let m = crate::pass::parser::parse(
            "module pkg/a;\n\
             pub fn run() -> . { () }\n\
             type Foo = .;\n\
             newtype Box[A] : A { constructor mk; projector get; };\n\
             labels Color = { red: ., green: . };\n\
             equiv e { (); () }\n\
             op _ + __ { impl run; };\n",
        )
        .expect("parses");
        let pick = |needle: &str| {
            // Pick an offset inside the named item's declaration.
            let src = "module pkg/a;\n\
                       pub fn run() -> . { () }\n\
                       type Foo = .;\n\
                       newtype Box[A] : A { constructor mk; projector get; };\n\
                       labels Color = { red: ., green: . };\n\
                       equiv e { (); () }\n\
                       op _ + __ { impl run; };\n";
            src.find(needle).expect("needle present") as u32
        };
        assert_eq!(
            enclosing_item_label(&m, "pkg/a", pick("fn run")),
            "in fn run"
        );
        assert_eq!(
            enclosing_item_label(&m, "pkg/a", pick("type Foo")),
            "in type Foo"
        );
        assert_eq!(
            enclosing_item_label(&m, "pkg/a", pick("newtype Box")),
            "in newtype Box"
        );
        assert_eq!(
            enclosing_item_label(&m, "pkg/a", pick("labels Color")),
            "in labels Color"
        );
        assert_eq!(
            enclosing_item_label(&m, "pkg/a", pick("equiv e")),
            "in equiv e"
        );
        assert_eq!(
            enclosing_item_label(&m, "pkg/a", pick("op _")),
            "in op _ + __"
        );
        // Offset at byte 0 — before any item, inside the `module` header.
        assert_eq!(
            enclosing_item_label(&m, "pkg/a", 0),
            "in module pkg/a",
            "an offset outside every item falls back to the module label"
        );
    }

    #[test]
    fn enclosing_item_label_picks_recursive_member() {
        let source = "module pkg/a;\n\
                      rec(loop) {\n\
                        fn first(value: .) -> . { rec second(value) };\n\
                        fn second(value: .) -> . { rec first(value) }\n\
                      }\n";
        let module = crate::pass::parser::parse(source).expect("parses");
        let first_call = source.find("rec second").expect("first body") as u32;
        let second_call = source.find("rec first").expect("second body") as u32;
        assert_eq!(
            enclosing_item_label(&module, "pkg/a", first_call),
            "in fn first"
        );
        assert_eq!(
            enclosing_item_label(&module, "pkg/a", second_call),
            "in fn second"
        );
    }

    #[test]
    fn unresolved_name_reports_no_current_module() {
        let s = Session::new(std::path::PathBuf::from("/tmp/pkg"));
        let out = cmd_type_name(&s, "x", Palette::plain());
        assert!(out.contains("no current module"), "got: {out}");
    }

    #[test]
    fn fqn_resolution_across_loaded_modules() {
        let s = session_with(&[
            ("pkg/a", "module pkg/a;\npub fn fa() -> . { () }\n"),
            ("pkg/b", "module pkg/b;\npub fn fb() -> . { () }\n"),
        ]);
        // Current is pkg/b; resolve pkg/a.fa by FQN.
        let out = cmd_source(&s, "pkg/a.fa", Palette::plain());
        assert!(out.contains("fn fa"), "got: {out}");
    }

    fn single_component_fqn_session() -> Session {
        let root = PathBuf::from("/kio-repl-tests/root-fqn");
        let files = [
            (
                "list.kio",
                "module list;\n/// Root item.\npub pure fn item(value: .) -> . { value }\n",
            ),
            (
                "other.kio",
                "module other;\npub fn item(left: ., right: .) -> . { left }\npub fn alias_only() -> . { () }\n",
            ),
            (
                "app.kio",
                "module app;\nimport list as declared;\nimport other as list;\nfn item() -> . { () }\nfn use_root() -> . { declared.item(()) }\n",
            ),
        ]
        .into_iter()
        .map(|(path, source)| (root.join(path), source.to_owned()))
        .collect();
        let mut session = Session::new_in_memory(root, files);
        let loaded = Command::Load("app".to_owned()).run(&mut session, Palette::plain());
        assert!(loaded.output.contains("loaded app"), "{}", loaded.output);
        session
    }

    #[test]
    fn single_component_fqn_completion_executes_named_views() {
        use crate::repl_core::completion::{AstScopeProvider, NameSet, complete};

        let mut session = single_component_fqn_session();
        let names = NameSet::from_session(&session);
        let scope = AstScopeProvider::new(&names);
        for (view, expected) in [
            ("signature", "pub pure fn item(value: .) -> ."),
            (
                "source",
                "/// Root item.\npub pure fn item(value: .) -> . { value }",
            ),
            ("doc", "Root item.\n\npub pure fn item(value: .) -> ."),
            ("which", "list.item"),
            ("type", "list.item : . -> ."),
            ("pure", "pure"),
        ] {
            let mut line = format!(":{view} list.it");
            let completion = complete(&names, &scope, &line, line.len());
            let candidate = completion
                .candidates
                .iter()
                .find(|candidate| candidate.label == "list.item")
                .expect("loaded root-module FQN is offered");
            line.replace_range(completion.replace, &candidate.label);
            assert_eq!(line, format!(":{view} list.item"));
            let actual = parse_command(&line)
                .unwrap()
                .run(&mut session, Palette::plain())
                .output;
            assert_eq!(actual, expected, "{line}");
        }
    }

    #[test]
    fn single_component_fqn_preserves_alias_expressions_and_unresolved_names() {
        let mut session = single_component_fqn_session();
        let found = resolve_name(&session, "list.item").expect("exact loaded item");
        assert_eq!(found.module_path, "list");
        assert_eq!(resolve_name(&session, "item").unwrap().module_path, "app");
        assert_eq!(cmd_pure_name(&session, "item"), "impure");
        let alias_type = cmd_type_expression(&mut session, "list.item", Palette::plain());
        assert!(alias_type.starts_with("list.item : "), "{alias_type}");
        assert_ne!(alias_type, "list.item : . -> .");
        for (input, expected) in [
            (":type list.alias_only", "list.alias_only : . -> ."),
            (":pure list.alias_only", "impure"),
            (":normalize list.item((), ())", "()"),
        ] {
            let actual = parse_command(input)
                .unwrap()
                .run(&mut session, Palette::plain())
                .output;
            assert_eq!(actual, expected, "{input}");
        }
        for name in [
            "list.alias_only",
            "list.missing",
            "missing.item",
            "list.item.extra",
            "list.",
        ] {
            assert!(resolve_name(&session, name).is_none(), "{name}");
            assert!(cmd_source(&session, name, Palette::plain()).contains("did not resolve"));
        }
    }

    #[test]
    fn help_text_lists_every_command() {
        let h = help_text();
        for c in CANONICAL_COMMANDS {
            assert!(h.contains(c), "help should mention `:{c}`");
        }
        // `:help`'s second short form shows in the rendered block.
        assert!(h.contains(":?"), "help should advertise the `:?` alias");
    }

    #[test]
    fn display_spellings_lists_full_command_first() {
        // Every command leads with its full name and follows with its
        // abbreviation(s), regardless of which spelling is canonical: the
        // short-canonical commands (`t`, `ls`, `mods`, `refs`) list
        // long-first just like the long-canonical ones.
        assert_eq!(display_spellings("mods"), ["modules", "mods"]);
        assert_eq!(display_spellings("t"), ["type", "t"]);
        assert_eq!(display_spellings("ls"), ["list", "ls"]);
        assert_eq!(display_spellings("refs"), ["references", "refs"]);
        assert_eq!(display_spellings("load"), ["load", "l"]);
        assert_eq!(display_spellings("normalize"), ["normalize", "norm"]);
        // Help's two short forms follow the full name in declaration order.
        assert_eq!(display_spellings("help"), ["help", "h", "?"]);
        // A command with no synonym is a singleton.
        assert_eq!(display_spellings("doc"), ["doc"]);
    }

    #[test]
    fn help_text_renders_every_summary() {
        // `help_text` renders from `HELP_SECTIONS`, the same table the
        // popup menu reads; every canonical command's summary appears in
        // the rendered help block.
        let h = help_text();
        for c in CANONICAL_COMMANDS {
            let summary = command_summary(c).unwrap_or_else(|| panic!("no summary for `:{c}`"));
            assert!(
                h.contains(summary),
                "help should render the `:{c}` summary {summary:?}"
            );
        }
    }

    #[test]
    fn pure_help_usage_completion_and_mode_share_the_dual_input_contract() {
        let help = help_text();
        let pure_row = help
            .lines()
            .find(|line| line.trim_start().starts_with(":pure"))
            .expect("`:pure` help row");
        assert!(pure_row.contains("<name-or-expression>"), "got: {pure_row}");
        assert!(
            usage_for("pure").contains("<name-or-expression>"),
            "got: {}",
            usage_for("pure")
        );
        assert_eq!(completion_shape("pure"), CompletionShape::NameOrFqn);
        assert_eq!(command_mode("pure"), Some(CommandMode::NameOrExpression));
    }

    #[test]
    fn command_summary_known_and_unknown() {
        // A canonical command yields its one-line summary.
        assert_eq!(
            command_summary("load"),
            Some("load a module (and its import-closure)")
        );
        assert_eq!(command_summary("quit"), Some("leave the REPL"));
        // A synonym is not a canonical name — the caller folds it first.
        assert_eq!(command_summary("l"), None);
        // An unknown command has no summary.
        assert_eq!(command_summary("frobnicate"), None);
    }

    // ── bare-input dispatch ──────────────────────────────────────────────────

    #[test]
    fn doc_parse_accepts_bare_names() {
        // Identifier, dotted path, operator, parenthesized operator.
        assert_eq!(doc_parse("foo"), Some("foo".to_owned()));
        assert_eq!(doc_parse("X.a.b"), Some("X.a.b".to_owned()));
        assert_eq!(doc_parse("+"), Some("+".to_owned()));
        assert_eq!(doc_parse("(+)"), Some("+".to_owned()));
    }

    #[test]
    fn doc_parse_rejects_compound_expressions() {
        // Literals, applications, operator expressions — not bare names.
        assert_eq!(doc_parse("42"), None);
        assert_eq!(doc_parse("\"hi\""), None);
        assert_eq!(doc_parse("foo()"), None);
        assert_eq!(doc_parse("1 + 2"), None);
    }

    #[test]
    fn expr_parse_accepts_kio_expressions() {
        // Vanilla session (no current module) — operator-free
        // expressions still parse via the fallback wrap.
        let s = Session::new(std::path::PathBuf::from("/tmp/pkg"));
        assert!(expr_parse(&s, "42").is_some());
        assert!(expr_parse(&s, "\"hi\"").is_some());
        assert!(expr_parse(&s, "()").is_some());
        assert!(expr_parse(&s, "foo()").is_some());
    }

    // (operator-expression parsing against the current module is
    // covered by the integration test `repl_bare_compound_runs_normalize`;
    // unit-testing it here would need a full `Session::refresh()` to
    // populate the source cache, which `session_with` skips.)

    #[test]
    fn expr_parse_rejects_garbage() {
        // Two bare idents juxtaposed don't parse as a Kio expression.
        let s = Session::new(std::path::PathBuf::from("/tmp/pkg"));
        assert!(expr_parse(&s, "hello world").is_none());
        // Empty input.
        assert!(expr_parse(&s, "").is_none());
        assert!(expr_parse(&s, "   ").is_none());
    }

    #[test]
    fn invalid_bare_input_is_a_single_honest_line() {
        // Input that matches no kind drops the old `:doc`/`:normalize`
        // guess and prints one honest line pointing at `:help`.
        let out = invalid_bare_input("hello world");
        assert!(
            out.contains("hello world"),
            "should echo input, got:\n{out}"
        );
        assert!(
            out.contains("not a name or a Kio expression"),
            "got:\n{out}"
        );
        assert!(out.contains(":help"), "should point at :help, got:\n{out}");
        // The retired two-command dump must not return.
        assert!(!out.contains(":doc"), "no fixed two-command dump: {out}");
    }

    #[test]
    fn applicable_commands_dedupes_the_identifier_is_both_case() {
        // A bare identifier is both a name and an expression; `t` /
        // `normalize` appear under both kinds but list once.
        let kinds = BareInputKinds {
            name: true,
            expression: true,
            module_path: false,
        };
        let cmds = applicable_commands(kinds);
        assert_eq!(cmds.iter().filter(|c| **c == "t").count(), 1);
        assert_eq!(cmds.iter().filter(|c| **c == "normalize").count(), 1);
        assert!(cmds.contains(&"signature"));
        assert!(cmds.contains(&"doc"));
    }

    #[test]
    fn a_name_only_footer_does_not_offer_normalize() {
        let kinds = BareInputKinds {
            name: true,
            expression: false,
            module_path: false,
        };
        let cmds = applicable_commands(kinds);
        assert!(cmds.contains(&"t"));
        assert!(
            !cmds.contains(&"normalize"),
            "an FQN that is not an expression cannot be normalized: {cmds:?}"
        );
    }

    #[test]
    fn applicable_commands_follow_the_canonical_order() {
        // The footer presents commands in `CANONICAL_COMMANDS` order —
        // the same order `:help` lists them — regardless of which kind
        // contributed each command.
        let all = BareInputKinds {
            name: true,
            expression: true,
            module_path: true,
        };
        let cmds = applicable_commands(all);
        let positions: Vec<usize> = cmds
            .iter()
            .map(|c| {
                CANONICAL_COMMANDS
                    .iter()
                    .position(|k| k == c)
                    .expect("footer command is canonical")
            })
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "footer must follow :help's order");
    }

    #[test]
    fn applicable_commands_footer_shows_full_spellings() {
        // The footer renders each command's full spelling — `:type`,
        // never the short `:t` — matching `:help`'s primary column.
        let kinds = BareInputKinds {
            name: false,
            expression: true,
            module_path: false,
        };
        let footer = applicable_commands_footer(kinds, Palette::plain());
        assert!(
            footer.contains(":type") && footer.contains(":normalize"),
            "full spellings expected, got:\n{footer}"
        );
        assert!(
            !footer.contains(":t "),
            "short spelling must not appear, got:\n{footer}"
        );
    }

    #[test]
    fn applicable_commands_match_the_command_table() {
        // Every command the map names must be a real, argument-taking
        // command with the mode its kind implies — the map can't drift
        // from the `:command` table.
        let all = BareInputKinds {
            name: true,
            expression: true,
            module_path: true,
        };
        for c in applicable_commands(all) {
            let canonical = canonical_name(c.trim_start_matches(':'));
            assert!(
                CANONICAL_COMMANDS.contains(&canonical),
                "`{c}` is not a real command"
            );
        }
        // Name-kind commands accept names; expression-kind commands accept
        // expressions. The dual `:t` mode belongs to both sets.
        let name_only = BareInputKinds {
            name: true,
            ..BareInputKinds::default()
        };
        for c in applicable_commands(name_only) {
            let canonical = canonical_name(c.trim_start_matches(':'));
            assert!(
                command_mode(canonical).is_some_and(CommandMode::accepts_name),
                "`{c}` does not accept a name"
            );
        }
        let expression_only = BareInputKinds {
            expression: true,
            ..BareInputKinds::default()
        };
        for c in applicable_commands(expression_only) {
            let canonical = canonical_name(c.trim_start_matches(':'));
            assert!(
                command_mode(canonical).is_some_and(CommandMode::accepts_expression),
                "`{c}` does not accept an expression"
            );
        }
    }

    #[test]
    fn operator_query_identity_preserves_slots_and_tail() {
        use crate::ast::{Op, OpBody};
        let m = crate::pass::parser::parse(
            "module pkg/a;\npub fn cond(x: ., y: ., z: .) -> . { x }\n\
             op _ ? _ : __ { impl cond; };\n",
        )
        .expect("parses");
        let op = m.items.iter().find_map(|it| match it {
            Item::Op(o, _) => Some((**o).clone()),
            _ => None,
        });
        let op: Op<Surface> = op.expect("the module declares an op");
        // The ternary `_ ? _ : __` spells `?:`.
        assert_eq!(crate::pass::parser::op_name(&op.body), "op _ ? _ : __");
        // A pattern with no tokens is not a spellable operator.
        assert!(matches!(op.body, OpBody::Normal { .. }));
    }

    #[test]
    fn repl_operator_query_normalizes_to_canonical_name() {
        // The REPL resolves an operator by its canonical name, parsing
        // the user's argument through the same parser the `import` clause
        // uses — so spacing variations of the same name resolve, and
        // the canonical form is the comparison key.
        use crate::ast::{Item, OpBody};
        let m = crate::pass::parser::parse(
            "module pkg/a;\npub fn cond(x: ., y: ., z: .) -> . { x }\n\
             op _ ? _ : __ { impl cond; };\n",
        )
        .expect("parses");
        let body = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Op(o, _) => Some(o.body.clone()),
                _ => None,
            })
            .expect("op");
        assert!(matches!(body, OpBody::Normal { .. }));
        let name = crate::pass::parser::op_name(&body);
        assert_eq!(name, "op _ ? _ : __");
        // Both a tight and a spaced spelling of the name normalize to
        // the op's one name — the REPL would match either.
        for typed in ["op _ ? _ : __", "op _  ? _ :  __"] {
            let parsed = crate::pass::parser::parse_operator_grammar(typed)
                .unwrap_or_else(|e| panic!("parse failed for {typed:?}: {e:?}"));
            assert_eq!(parsed.render(), name, "for input {typed:?}");
        }
    }

    // ── command input modes ──────────────────────────────────────────────────

    #[test]
    fn command_mode_records_the_value_and_entity_categories() {
        // `:t` and `:pure` accept either category, `:normalize` is
        // expression-only, declared-entity slots take a name/FQN, and
        // no-argument commands have no input mode.
        assert_eq!(command_mode("t"), Some(CommandMode::NameOrExpression));
        assert_eq!(command_mode("pure"), Some(CommandMode::NameOrExpression));
        assert_eq!(command_mode("normalize"), Some(CommandMode::Expression));
        for fqn in [
            "load",
            "unload",
            "ls",
            "signature",
            "doc",
            "source",
            "which",
            "refs",
        ] {
            assert_eq!(command_mode(fqn), Some(CommandMode::Fqn), "for {fqn}");
        }
        for none in ["mods", "scope", "help", "quit", "reset"] {
            assert_eq!(command_mode(none), None, "for {none}");
        }
        for command in ["t", "pure"] {
            let dual = command_mode(command).expect("dual query input mode");
            assert!(dual.accepts_name(), "for :{command}");
            assert!(dual.accepts_expression(), "for :{command}");
        }
    }

    #[test]
    fn command_mode_partitions_every_arg_taking_command() {
        // The value-vs-entity seam must cover exactly the argument-taking
        // commands `completion_shape` recognizes — no command is left
        // without a committed mode, none gains a spurious one.
        for canonical in CANONICAL_COMMANDS {
            let takes_arg = completion_shape(canonical) != CompletionShape::None;
            assert_eq!(
                command_mode(canonical).is_some(),
                takes_arg,
                "mode/shape disagree on whether `:{canonical}` takes an argument"
            );
        }
    }

    // ── fallible sibling-command correction ──────────────────────────────────

    #[test]
    fn fqn_in_expression_slot_suggests_the_entity_views() {
        // A slash FQN handed to a value slot: the shape test fires
        // and the hint names `:signature` / `:doc`.
        let hint = mode_mismatch_hint(CommandMode::Expression, "foo/bar.Baz")
            .expect("a slash FQN trips the FQN-shape hint");
        assert!(hint.contains(":signature foo/bar.Baz"), "got: {hint}");
        assert!(hint.contains(":doc foo/bar.Baz"), "got: {hint}");
    }

    #[test]
    fn non_fqn_names_in_expression_slot_do_not_suggest() {
        // Bare and dotted-only names are legitimate value/member
        // references — they must NOT trip the FQN-shape hint.
        assert_eq!(mode_mismatch_hint(CommandMode::Expression, "foo"), None);
        assert_eq!(mode_mismatch_hint(CommandMode::Expression, "foo.Bar"), None);
    }

    #[test]
    fn compound_expression_in_fqn_slot_suggests_the_value_views() {
        // An operator expression handed to an entity slot: the hint
        // names `:type` / `:normalize`.
        let hint = mode_mismatch_hint(CommandMode::Fqn, "1 + 2")
            .expect("a compound expression trips the expression-shape hint");
        assert!(hint.contains(":type 1 + 2"), "got: {hint}");
        assert!(hint.contains(":normalize 1 + 2"), "got: {hint}");
    }

    #[test]
    fn fqn_in_fqn_slot_does_not_suggest() {
        // A name handed to the slot it belongs in is no mismatch.
        assert_eq!(mode_mismatch_hint(CommandMode::Fqn, "foo.Bar"), None);
        assert_eq!(mode_mismatch_hint(CommandMode::Fqn, "foo"), None);
    }

    #[test]
    fn compound_expression_in_expression_slot_does_not_suggest() {
        // An expression handed to a value slot is no mismatch — a
        // failed `:normalize 1 + 2` reports the type error alone.
        assert_eq!(mode_mismatch_hint(CommandMode::Expression, "1 + 2"), None);
    }

    #[test]
    fn name_or_expression_slot_never_suggests_a_sibling_view() {
        assert_eq!(
            mode_mismatch_hint(CommandMode::NameOrExpression, "foo/bar.Baz"),
            None
        );
        assert_eq!(
            mode_mismatch_hint(CommandMode::NameOrExpression, "1 + 2"),
            None
        );
    }

    #[test]
    fn correction_is_gated_on_failure_not_on_shape_alone() {
        // The hint never overrides a command that *succeeds* in its own
        // mode: `:signature add` resolves, so its output carries no
        // sibling suggestion even though `add` is a bare name.
        let s = session_with(&[(
            "pkg/a",
            "module pkg/a;\npub fn add(x: ., y: .) -> . { x }\n",
        )]);
        let out = cmd_signature(&s, "add", Palette::plain());
        assert!(out.contains("fn add"), "got: {out}");
        assert!(!out.contains(":type"), "no sibling hint on success: {out}");
    }

    #[test]
    fn signature_on_a_compound_expression_appends_the_value_view_hint() {
        // `:signature 1 + 2` fails to resolve (a compound expression is
        // not a declared entity); the failure diagnostic carries the
        // `:type` / `:normalize` suggestion.
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn run() -> . { () }\n")]);
        let out = cmd_signature(&s, "1 + 2", Palette::plain());
        assert!(out.contains("did not resolve"), "own-mode failure: {out}");
        assert!(out.contains(":type 1 + 2"), "sibling hint present: {out}");
    }

    #[test]
    fn type_on_an_unresolved_qualified_name_stays_in_its_name_branch() {
        // `:t` deliberately accepts FQNs as names. If one does not resolve,
        // and no `/` operator makes the tokens an expression, report that
        // failure without pretending it was a mode mismatch.
        let mut s = session_with(&[("pkg/a", "module pkg/a;\npub fn run() -> . { () }\n")]);
        let out = Command::Type("pkg/b.Thing".to_owned())
            .run(&mut s, Palette::plain())
            .output;
        assert!(out.contains("did not resolve"), "own-mode failure: {out}");
        assert!(
            !out.contains(":signature pkg/b.Thing"),
            "a dual-mode name failure received a mismatch hint: {out}"
        );
    }

    #[test]
    fn which_on_a_compound_expression_appends_the_value_view_hint() {
        // `:which 1 + 2` finds nothing; the locator failure suggests the
        // value views. `:which` stays a pure locator — the hint is the
        // only thing it adds, never a kind report.
        let s = session_with(&[("pkg/a", "module pkg/a;\npub fn run() -> . { () }\n")]);
        let out = cmd_which(&s, "1 + 2");
        assert!(out.contains("no loaded module declares"), "own-mode: {out}");
        assert!(out.contains(":type 1 + 2"), "sibling hint: {out}");
    }
}
