//! Implementation of `kio completions <shell>`.
//!
//! Prints a shell completion script for `bash`, `zsh`, or `fish` to
//! stdout. The user pipes or installs the output per their shell's
//! convention (see the quick-start guide linked from `--help`).
//!
//! The scripts are **static**: they complete the fixed CLI grammar —
//! the subcommand names, the `doc` / `cache` / `dep` / `sig`
//! sub-subcommand names plus the `completions` shell names, and the
//! flags each subcommand accepts. A
//! positional argument slot (a path to a `.kio` file, a target id, a
//! source selector, a module selector) falls back to the shell's
//! default filename completion; the scripts do not introspect the
//! package on disk to offer module names. After upgrading `kio`, the
//! user re-runs `kio completions <shell>` to pick up any new
//! subcommand.
//!
//! The completed surface is assembled at runtime from [`grammar`] so
//! it tracks the cfg-gated subcommands: a build without the `lsp`
//! feature emits a script that does not complete `lsp`, the same way
//! the top-level `--help` omits the `lsp` line.

use crate::exit_code::ExitCode;

const HELP_TEMPLATE: &str = "\
Usage: kio completions <shell>

Print a shell completion script for `kio` to stdout.

<shell> is one of:
  bash          Bourne-Again Shell.
  zsh           Z Shell.
  fish          Friendly Interactive Shell.

The script completes the fixed CLI grammar — subcommand names, the
`doc` / `cache` / `dep` / `sig` sub-subcommand names, the shell names
this command itself takes, and each subcommand's flags. Positional
argument slots (a source path, a target id, a source selector, a
module selector) fall back to the shell's default filename
completion; the script does not read the package on disk. Re-run
after upgrading `kio` to pick up new subcommands.

Install per your shell's convention — see the guide linked below for
copy-paste snippets.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success; 2 on CLI
usage error (missing shell argument, unknown shell, unknown flag,
extra arguments).

See {base}/specs/cli.md#kio-completions-shell for full command behavior.";

/// A top-level subcommand and the static grammar that follows it.
struct Subcommand {
    /// The subcommand word (`build`, `doc`, …).
    name: &'static str,
    /// Flags the subcommand accepts (excluding the universal `-h` /
    /// `--help`, which every node carries — the per-shell emitters add
    /// those uniformly).
    flags: &'static [&'static str],
    /// Sub-subcommand words (`doc` → `check` / `fmt` / `build`). Empty for a
    /// leaf subcommand.
    subs: &'static [&'static str],
}

/// The completed CLI grammar, assembled at runtime so it tracks the
/// cfg-gated subcommands. Mirrors the dispatch arms in
/// [`crate::run`]; the unadvertised `debug` namespace is
/// intentionally excluded, the same way it is absent from `--help`.
mod grammar {
    use super::Subcommand;

    /// Build the subcommand table for this build of `kio`. The `lsp`
    /// and `repl` rows are present only when their cargo feature is
    /// compiled in.
    pub fn subcommands() -> Vec<Subcommand> {
        let mut subs = vec![
            Subcommand {
                name: "init",
                flags: &[],
                subs: &[],
            },
            Subcommand {
                name: "check",
                flags: &[],
                subs: &[],
            },
            Subcommand {
                name: "build",
                flags: &["--skip-unsupported-targets"],
                subs: &[],
            },
            Subcommand {
                name: "fmt",
                flags: &["--check", "-"],
                subs: &[],
            },
            Subcommand {
                name: "test",
                flags: &["--include-deps"],
                subs: &[],
            },
            Subcommand {
                name: "sig",
                flags: &["--force", "--stdout", "--message", "--breaking", "--since"],
                subs: &["stage", "commit", "uncommit", "status", "log", "compact"],
            },
            Subcommand {
                name: "doc",
                flags: &[],
                subs: &["check", "fmt", "build"],
            },
            Subcommand {
                name: "cache",
                flags: &[],
                subs: &["clear", "path", "gc"],
            },
            Subcommand {
                name: "dep",
                flags: &[],
                subs: &["fetch", "update", "clean"],
            },
        ];
        #[cfg(feature = "lsp")]
        subs.push(Subcommand {
            name: "lsp",
            flags: &[],
            subs: &[],
        });
        #[cfg(feature = "repl")]
        subs.push(Subcommand {
            name: "repl",
            flags: &[],
            subs: &[],
        });
        subs.push(Subcommand {
            name: "completions",
            flags: &[],
            subs: &["bash", "zsh", "fish"],
        });
        subs
    }

    /// Flags `doc fmt` and `doc build` accept. Surfaced here so the
    /// per-shell emitters can special-case doc sub-subcommand slots
    /// without threading per-sub flag tables through the whole grammar.
    pub const DOC_FMT_FLAGS: &[&str] = &["--check"];
    pub const DOC_BUILD_FLAGS: &[&str] = &["--md", "--html"];
}

/// Dispatch for `kio completions <shell>`.
pub fn run(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    let positional: Vec<&str> = args.iter().map(String::as_str).collect();
    let shell = match positional.as_slice() {
        [shell] => *shell,
        [] => {
            eprintln!("error: `kio completions` requires a shell argument (bash, zsh, or fish)");
            eprintln!();
            eprintln!(
                "{}",
                HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
            );
            return ExitCode::Usage;
        }
        _ => {
            eprintln!(
                "error: `kio completions` takes exactly one shell argument, got {}",
                positional.len()
            );
            eprintln!();
            eprintln!(
                "{}",
                HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
            );
            return ExitCode::Usage;
        }
    };
    let script = match shell {
        "bash" => render_bash(),
        "zsh" => render_zsh(),
        "fish" => render_fish(),
        other => {
            eprintln!("error: unknown shell: {other} (expected bash, zsh, or fish)");
            eprintln!();
            eprintln!(
                "{}",
                HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
            );
            return ExitCode::Usage;
        }
    };
    print!("{script}");
    ExitCode::Success
}

/// The top-level subcommand words, space-joined, for this build.
fn subcommand_words() -> String {
    grammar::subcommands()
        .iter()
        .map(|s| s.name)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Render the bash completion script.
///
/// The function name (`_kio`) is the conventional `_<prog>` form
/// `complete -F` expects. The completer walks `COMP_WORDS` to find
/// the active subcommand, then offers that subcommand's sub-words and
/// flags; with no subcommand chosen yet it offers the top-level
/// subcommand set. Any slot the static grammar doesn't enumerate
/// falls through to `compgen -f` (filename completion).
fn render_bash() -> String {
    let mut out = String::new();
    out.push_str("# bash completion for kio. Source this file, or install it where\n");
    out.push_str("# your bash-completion setup loads per-command scripts (commonly\n");
    out.push_str("# /etc/bash_completion.d/ or $XDG_DATA_HOME/bash-completion/completions/kio).\n");
    out.push_str("_kio() {\n");
    out.push_str("    local cur prev words cword\n");
    out.push_str("    _init_completion 2>/dev/null || {\n");
    out.push_str("        cur=\"${COMP_WORDS[COMP_CWORD]}\"\n");
    out.push_str("        prev=\"${COMP_WORDS[COMP_CWORD-1]}\"\n");
    out.push_str("        words=(\"${COMP_WORDS[@]}\")\n");
    out.push_str("        cword=$COMP_CWORD\n");
    out.push_str("    }\n\n");
    out.push_str(&format!(
        "    local subcommands=\"{}\"\n",
        subcommand_words()
    ));
    out.push_str("    local sub=\"\"\n");
    out.push_str("    local i\n");
    out.push_str("    for ((i=1; i < cword; i++)); do\n");
    out.push_str("        case \"${words[i]}\" in\n");
    out.push_str("            -*) ;;\n");
    out.push_str("            *) sub=\"${words[i]}\"; break ;;\n");
    out.push_str("        esac\n");
    out.push_str("    done\n\n");
    out.push_str("    if [ -z \"$sub\" ]; then\n");
    out.push_str(
        "        COMPREPLY=( $(compgen -W \"$subcommands -h --help -V --version\" -- \"$cur\") )\n",
    );
    out.push_str("        return 0\n");
    out.push_str("    fi\n\n");
    out.push_str("    case \"$sub\" in\n");
    for s in grammar::subcommands() {
        let mut words: Vec<String> = s.subs.iter().map(|w| (*w).to_owned()).collect();
        for f in s.flags {
            words.push((*f).to_owned());
        }
        if s.name == "doc" {
            // `doc fmt` / `doc build` carry their own flags. Bash's
            // simple completion path does not inspect the sub-subcommand,
            // so offer the union in the doc slot.
            words.extend(grammar::DOC_FMT_FLAGS.iter().map(|f| (*f).to_owned()));
            words.extend(grammar::DOC_BUILD_FLAGS.iter().map(|f| (*f).to_owned()));
        }
        words.push("-h".to_owned());
        words.push("--help".to_owned());
        out.push_str(&format!("        {})\n", s.name));
        out.push_str(&format!(
            "            COMPREPLY=( $(compgen -W \"{}\" -- \"$cur\") )\n",
            words.join(" ")
        ));
        out.push_str("            ;;\n");
    }
    out.push_str("        *)\n");
    out.push_str("            COMPREPLY=( $(compgen -f -- \"$cur\") )\n");
    out.push_str("            ;;\n");
    out.push_str("    esac\n");
    out.push_str("    return 0\n");
    out.push_str("}\n");
    out.push_str("complete -F _kio kio\n");
    out
}

/// Render the zsh completion script.
///
/// Uses the `#compdef` autoload header plus a `_arguments`-driven
/// state machine. The first positional offers the subcommand set;
/// each subcommand's second positional offers its sub-words. A slot
/// with no static candidates uses `_files` (filename completion).
fn render_zsh() -> String {
    let mut out = String::new();
    out.push_str("#compdef kio\n");
    out.push_str("# zsh completion for kio. Install as a file named `_kio` on your\n");
    out.push_str("# $fpath (e.g. ~/.zsh/completions/_kio, with that dir added to fpath\n");
    out.push_str("# before `compinit` runs in ~/.zshrc).\n");
    out.push_str("_kio() {\n");
    out.push_str("    local -a subcommands\n");
    out.push_str("    subcommands=(\n");
    for s in grammar::subcommands() {
        out.push_str(&format!("        '{}'\n", s.name));
    }
    out.push_str("    )\n");
    out.push_str("    local curcontext=\"$curcontext\" state line\n");
    out.push_str("    _arguments -C \\\n");
    out.push_str("        '1: :->subcmd' \\\n");
    out.push_str("        '*::arg:->args'\n\n");
    out.push_str("    case $state in\n");
    out.push_str("        subcmd)\n");
    out.push_str("            _describe -t subcommands 'kio subcommand' subcommands\n");
    out.push_str("            ;;\n");
    out.push_str("        args)\n");
    out.push_str("            case $line[1] in\n");
    for s in grammar::subcommands() {
        out.push_str(&format!("                {})\n", s.name));
        if s.subs.is_empty() {
            // No sub-subcommands: positional slots are paths / ids the
            // script can't enumerate — fall back to filename
            // completion (or nothing for the flag-only `-h`).
            out.push_str("                    _files\n");
        } else {
            let listed = s
                .subs
                .iter()
                .map(|w| (*w).to_owned())
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!(
                "                    _values '{} subcommand' {}\n",
                s.name, listed
            ));
        }
        out.push_str("                    ;;\n");
    }
    out.push_str("                *)\n");
    out.push_str("                    _files\n");
    out.push_str("                    ;;\n");
    out.push_str("            esac\n");
    out.push_str("            ;;\n");
    out.push_str("    esac\n");
    out.push_str("}\n");
    out.push_str("_kio \"$@\"\n");
    out
}

/// Render the fish completion script.
///
/// fish completions are a flat list of `complete` directives.
/// `__fish_use_subcommand` gates the top-level subcommand offers;
/// `__fish_seen_subcommand_from <name>` gates each subcommand's
/// sub-words. fish offers file completion in every slot by default,
/// so positional path slots need no explicit directive.
fn render_fish() -> String {
    let mut out = String::new();
    out.push_str("# fish completion for kio. Install at\n");
    out.push_str("# ~/.config/fish/completions/kio.fish (fish autoloads it).\n");
    out.push_str("complete -c kio -f\n");
    for s in grammar::subcommands() {
        out.push_str(&format!(
            "complete -c kio -n '__fish_use_subcommand' -a '{}'\n",
            s.name
        ));
    }
    for s in grammar::subcommands() {
        for w in s.subs {
            out.push_str(&format!(
                "complete -c kio -n '__fish_seen_subcommand_from {}' -a '{}'\n",
                s.name, w
            ));
        }
        for f in s.flags {
            // fish `complete` wants a long flag under `-l` (no dashes)
            // and a short flag under `-s`. The grammar's flags are all
            // long (`--check`) except `fmt`'s `-` stdin marker, which
            // is a positional sentinel, not a flag — skip it here so
            // fish doesn't render a spurious option.
            if let Some(long) = f.strip_prefix("--") {
                out.push_str(&format!(
                    "complete -c kio -n '__fish_seen_subcommand_from {}' -l '{}'\n",
                    s.name, long
                ));
            }
        }
        if s.name == "doc" {
            for f in grammar::DOC_FMT_FLAGS {
                let long = f.strip_prefix("--").unwrap_or(f);
                out.push_str(&format!(
                    "complete -c kio -n '__fish_seen_subcommand_from doc; and __fish_seen_subcommand_from fmt' -l '{long}'\n"
                ));
            }
            for f in grammar::DOC_BUILD_FLAGS {
                let long = f.strip_prefix("--").unwrap_or(f);
                out.push_str(&format!(
                    "complete -c kio -n '__fish_seen_subcommand_from doc; and __fish_seen_subcommand_from build' -l '{long}'\n"
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn help_flag_returns_success() {
        assert_eq!(run(&argv(&["-h"])), ExitCode::Success);
        assert_eq!(run(&argv(&["--help"])), ExitCode::Success);
        // `--help` wins even when a shell is also named.
        assert_eq!(run(&argv(&["bash", "--help"])), ExitCode::Success);
    }

    #[test]
    fn missing_shell_returns_usage() {
        assert_eq!(run(&[]), ExitCode::Usage);
    }

    #[test]
    fn unknown_shell_returns_usage() {
        assert_eq!(run(&argv(&["powershell"])), ExitCode::Usage);
    }

    #[test]
    fn extra_arguments_return_usage() {
        assert_eq!(run(&argv(&["bash", "zsh"])), ExitCode::Usage);
    }

    #[test]
    fn each_shell_returns_success() {
        assert_eq!(run(&argv(&["bash"])), ExitCode::Success);
        assert_eq!(run(&argv(&["zsh"])), ExitCode::Success);
        assert_eq!(run(&argv(&["fish"])), ExitCode::Success);
    }

    /// Each rendered script is non-empty and names every advertised
    /// subcommand, so a new dispatch arm that forgets to extend the
    /// grammar is caught here rather than shipping a script that
    /// silently can't complete the new subcommand.
    #[test]
    fn scripts_name_every_subcommand() {
        let words = subcommand_words();
        for script in [render_bash(), render_zsh(), render_fish()] {
            assert!(!script.is_empty());
            for word in words.split(' ') {
                assert!(
                    script.contains(word),
                    "rendered script is missing subcommand `{word}`"
                );
            }
        }
    }

    /// The bash script wires the `complete -F _kio kio` registration
    /// and the zsh script carries the `#compdef kio` autoload header —
    /// the two load-bearing lines each shell needs to pick the script
    /// up.
    #[test]
    fn scripts_carry_registration_lines() {
        assert!(render_bash().contains("complete -F _kio kio"));
        assert!(render_zsh().starts_with("#compdef kio"));
        assert!(render_fish().contains("complete -c kio"));
    }
}
